// A C interface over Dynarmic's A64 JIT, loaded by `uxdroid-translate` with dlopen.
//
// Guest memory is the host's: guest addresses are host addresses (fastmem with a zero base and a
// 64-bit address space), so translated code reads and writes the process' memory directly. The
// memory callbacks below are the slow path Dynarmic falls back to after a fastmem access faults,
// and do the same direct access. Every exported function is noexcept: no C++ exception crosses
// into Rust.
//
// Every guest CPU of the process, one per guest thread and nesting level, runs code from one
// shared code cache (shared_code.patch, beside this file), so a block is translated once and not
// again on each thread. The rules:
//
// - Translated code is the same for every CPU: what differs per CPU (registers, TPIDR_EL0, the
//   exclusive reservation, the fast dispatch table) is reached through the CPU's own state. The
//   callbacks baked into the code are `dispatch` below, which finds the CPU through `running`, the
//   innermost CPU running on this thread.
// - Each cache's lock is Dynarmic's (shared_code.patch): held to find a block and to emit and link
//   one, not while guest code is decoded. `shared.mutex` only guards which cache is current and is
//   taken only when a CPU is made or moves; it is never held while guest code runs, and a cache's
//   lock is never taken under it.
// - Nothing is invalidated or cleared while any CPU may run the code. A cache with no room left,
//   or retired, makes the CPU that needs a new block stop with CodeCacheFull; `uxd_cpu_run` then
//   moves that CPU to the current cache, making a new one if the full one is still current, and
//   goes on at the same PC. A CPU also moves at the start of each run, so idle CPUs do not keep an
//   old cache alive. A cache is freed when its last CPU leaves it, which is never while a CPU runs
//   it or sits in a callback that returns into it, since a CPU only moves between runs.
// - Exclusive stores compare and swap against the value the exclusive load read, as QEMU does,
//   instead of using a global monitor: the same code must work for any number of CPUs.
//
// A cache has several emitters (UXDROID_CODE_EMITTERS, default 2), each with its own part of the
// cache and its own lock, so threads translating at once seldom wait for each other; the blocks of
// all of them link to each other. UXDROID_SHARED_CODE=0 gives each CPU a cache of its own with one
// emitter, as before sharing, to measure what sharing saves, and UXDROID_CODE_CACHE_MIB sets the
// size of a cache (default 128, at least 8 per emitter).

#include <algorithm>
#include <atomic>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <ctime>
#include <exception>
#include <memory>
#include <mutex>
#include <optional>
#include <vector>

#include "dynarmic/interface/A64/a64.h"
#include "dynarmic/interface/A64/config.h"
#include "dynarmic/interface/exclusive_monitor.h"

namespace {

constexpr std::uint32_t ABI_VERSION = 4;
// Address space a code cache reserves; only the pages code is written to take memory.
constexpr std::size_t DEFAULT_CODE_CACHE_MIB = 128;
// Dynarmic keeps a 2 MiB constant pool and 1 MiB of headroom inside each emitter's part.
constexpr std::size_t MINIMUM_CODE_CACHE_MIB = 8;
constexpr std::size_t DEFAULT_EMITTERS = 2;
constexpr std::size_t MAXIMUM_EMITTERS = 16;
constexpr std::size_t MAXIMUM_CODE_CACHE_MIB = 1024;
constexpr std::uint32_t CNTFRQ = 1000000000;

enum : std::uint32_t {
    KIND_UNALLOCATED = 1,
    KIND_RESERVED = 2,
    KIND_UNPREDICTABLE = 3,
    KIND_BREAKPOINT = 4,
    KIND_NO_EXECUTE = 5,
    KIND_OTHER = 6,
    KIND_INTERPRETER_FALLBACK = 7,
};

}  // namespace

extern "C" {

struct uxd_callbacks {
    void* context;
    // Called for every `svc #number`. A nonzero result ends the current run after this call.
    int (*svc)(void* context, std::uint32_t number);
    // Called once for an instruction the JIT cannot run; the run then ends.
    void (*exception)(void* context, std::uint64_t pc, std::uint32_t kind);
};

}  // extern "C"

struct uxd_stats {
    // Blocks translated by every cache of the process.
    std::uint64_t blocks;
    // Bytes of code in the current cache.
    std::uint64_t code_bytes;
    // Caches made, the first included.
    std::uint64_t caches;
    // Times the watched address was read to translate it (uxd_watch).
    std::uint64_t watched_translations;
};

struct uxd_cpu;

namespace {

// Dynarmic asserts a monitor exists; shared code never uses it (see above).
Dynarmic::ExclusiveMonitor& monitor() {
    static Dynarmic::ExclusiveMonitor instance{1};
    return instance;
}

thread_local uxd_cpu* running = nullptr;

// While this thread translates ahead (uxd_pretranslate): the code it may read, as start and end
// pairs. Decoding stops at the first instruction outside them, so a location near a range's end
// cannot make Dynarmic read into the unmapped gap after it. Null otherwise.
thread_local const std::uint64_t* pretranslate_ranges = nullptr;
thread_local std::size_t pretranslate_range_count = 0;

bool may_read_code(std::uint64_t vaddr) {
    if (pretranslate_ranges == nullptr) {
        return true;
    }
    for (std::size_t i = 0; i < pretranslate_range_count; ++i) {
        const std::uint64_t start = pretranslate_ranges[2 * i];
        const std::uint64_t end = pretranslate_ranges[2 * i + 1];
        if (vaddr >= start && end >= 4 && vaddr <= end - 4) {
            return true;
        }
    }
    return false;
}

std::atomic<std::uint64_t> watched_address{0};
std::atomic<std::uint64_t> watched_translations{0};

template<typename T>
T load(std::uint64_t vaddr) {
    T value;
    std::memcpy(&value, reinterpret_cast<const void*>(vaddr), sizeof(T));
    return value;
}

template<typename T>
void store(std::uint64_t vaddr, T value) {
    std::memcpy(reinterpret_cast<void*>(vaddr), &value, sizeof(T));
}

template<typename T>
bool exchange(std::uint64_t vaddr, T value, T expected) {
    return __atomic_compare_exchange_n(reinterpret_cast<T*>(vaddr), &expected, value, false,
                                       __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
}

// The callbacks every translated block calls, on whichever thread runs it: memory is the same for
// all, and the rest goes to the CPU running on this thread.
struct Callbacks final : Dynarmic::A64::UserCallbacks {
    std::optional<std::uint32_t> MemoryReadCode(std::uint64_t vaddr) override {
        // Dynarmic ends the block there with a NoExecuteFault, as for code it may not read.
        if (!may_read_code(vaddr)) {
            return std::nullopt;
        }
        if (vaddr == watched_address.load(std::memory_order_relaxed)) {
            watched_translations.fetch_add(1, std::memory_order_relaxed);
        }
        return load<std::uint32_t>(vaddr);
    }
    std::uint8_t MemoryRead8(std::uint64_t vaddr) override { return load<std::uint8_t>(vaddr); }
    std::uint16_t MemoryRead16(std::uint64_t vaddr) override { return load<std::uint16_t>(vaddr); }
    std::uint32_t MemoryRead32(std::uint64_t vaddr) override { return load<std::uint32_t>(vaddr); }
    std::uint64_t MemoryRead64(std::uint64_t vaddr) override { return load<std::uint64_t>(vaddr); }
    Dynarmic::A64::Vector MemoryRead128(std::uint64_t vaddr) override {
        return {load<std::uint64_t>(vaddr), load<std::uint64_t>(vaddr + 8)};
    }
    void MemoryWrite8(std::uint64_t vaddr, std::uint8_t value) override { store(vaddr, value); }
    void MemoryWrite16(std::uint64_t vaddr, std::uint16_t value) override { store(vaddr, value); }
    void MemoryWrite32(std::uint64_t vaddr, std::uint32_t value) override { store(vaddr, value); }
    void MemoryWrite64(std::uint64_t vaddr, std::uint64_t value) override { store(vaddr, value); }
    void MemoryWrite128(std::uint64_t vaddr, Dynarmic::A64::Vector value) override {
        store(vaddr, value[0]);
        store(vaddr + 8, value[1]);
    }
    bool MemoryWriteExclusive8(std::uint64_t vaddr, std::uint8_t value, std::uint8_t expected) override {
        return exchange(vaddr, value, expected);
    }
    bool MemoryWriteExclusive16(std::uint64_t vaddr, std::uint16_t value, std::uint16_t expected) override {
        return exchange(vaddr, value, expected);
    }
    bool MemoryWriteExclusive32(std::uint64_t vaddr, std::uint32_t value, std::uint32_t expected) override {
        return exchange(vaddr, value, expected);
    }
    bool MemoryWriteExclusive64(std::uint64_t vaddr, std::uint64_t value, std::uint64_t expected) override {
        return exchange(vaddr, value, expected);
    }
    bool MemoryWriteExclusive128(std::uint64_t vaddr, Dynarmic::A64::Vector value,
                                 Dynarmic::A64::Vector expected) override {
        unsigned __int128 wanted = (static_cast<unsigned __int128>(value[1]) << 64) | value[0];
        unsigned __int128 old = (static_cast<unsigned __int128>(expected[1]) << 64) | expected[0];
        return __sync_bool_compare_and_swap(reinterpret_cast<unsigned __int128*>(vaddr), old, wanted);
    }

    void InterpreterFallback(std::uint64_t pc, std::size_t) override;
    void CallSVC(std::uint32_t number) override;
    void ExceptionRaised(std::uint64_t pc, Dynarmic::A64::Exception exception) override;

    void AddTicks(std::uint64_t) override {}
    std::uint64_t GetTicksRemaining() override { return UINT64_MAX; }
    std::uint64_t GetCNTPCT() override {
        timespec now{};
        clock_gettime(CLOCK_MONOTONIC, &now);
        return static_cast<std::uint64_t>(now.tv_sec) * CNTFRQ + static_cast<std::uint64_t>(now.tv_nsec);
    }
};

Callbacks dispatch;

}  // namespace

struct uxd_cpu {
    uxd_callbacks host{};
    Dynarmic::A64::Jit* jit = nullptr;
};

namespace {

void Callbacks::InterpreterFallback(std::uint64_t pc, std::size_t) {
    running->host.exception(running->host.context, pc, KIND_INTERPRETER_FALLBACK);
    running->jit->HaltExecution();
}

void Callbacks::CallSVC(std::uint32_t number) {
    // Read again after the call: the service may run guest code on this thread, which sets and
    // restores `running` around its own run.
    if (running->host.svc(running->host.context, number) != 0) {
        running->jit->HaltExecution();
    }
}

void Callbacks::ExceptionRaised(std::uint64_t pc, Dynarmic::A64::Exception exception) {
    using E = Dynarmic::A64::Exception;
    uxd_cpu* const cpu = running;
    switch (exception) {
    // Hints a user-space thread may execute; nothing to do.
    case E::Yield:
    case E::WaitForEvent:
    case E::WaitForInterrupt:
    case E::SendEvent:
    case E::SendEventLocal:
        cpu->jit->SetPC(pc + 4);
        return;
    case E::UnallocatedEncoding:
        cpu->host.exception(cpu->host.context, pc, KIND_UNALLOCATED);
        break;
    case E::ReservedValue:
        cpu->host.exception(cpu->host.context, pc, KIND_RESERVED);
        break;
    case E::UnpredictableInstruction:
        cpu->host.exception(cpu->host.context, pc, KIND_UNPREDICTABLE);
        break;
    case E::Breakpoint:
        cpu->host.exception(cpu->host.context, pc, KIND_BREAKPOINT);
        break;
    case E::NoExecuteFault:
        cpu->host.exception(cpu->host.context, pc, KIND_NO_EXECUTE);
        break;
    default:
        cpu->host.exception(cpu->host.context, pc, KIND_OTHER);
        break;
    }
    cpu->jit->HaltExecution();
}

// What every CPU and cache is made with. TPIDR_EL0 is set per CPU.
Dynarmic::A64::UserConfig base_config() {
    Dynarmic::A64::UserConfig config;
    config.callbacks = &dispatch;
    config.processor_id = 0;
    config.global_monitor = &monitor();
    config.cntfrq_el0 = CNTFRQ;
    config.fastmem_pointer = 0;
    config.fastmem_address_space_bits = 64;
    config.fastmem_exclusive_access = true;
    config.enable_cycle_counting = false;
    std::size_t mib = DEFAULT_CODE_CACHE_MIB;
    if (const char* value = std::getenv("UXDROID_CODE_CACHE_MIB")) {
        char* end = nullptr;
        const unsigned long long parsed = std::strtoull(value, &end, 10);
        if (end != value && *end == '\0' && parsed >= MINIMUM_CODE_CACHE_MIB && parsed <= MAXIMUM_CODE_CACHE_MIB) {
            mib = static_cast<std::size_t>(parsed);
        }
    }
    config.code_cache_size = mib * 1024 * 1024;
    // Floating point on the host's fast path. By default it differs from Arm only in which NaN a
    // result carries, the precision of reciprocal estimates, and assuming the default FPCR when
    // choosing the fast path; UXDROID_FAST_FP=1 also splits fused multiply-adds, which rounds
    // them twice, and UXDROID_FAST_FP=0 keeps every operation exact.
    using Dynarmic::OptimizationFlag;
    const char* fast_fp = std::getenv("UXDROID_FAST_FP");
    if (!fast_fp || std::strcmp(fast_fp, "0") != 0) {
        config.unsafe_optimizations = true;
        config.optimizations |= OptimizationFlag::Unsafe_ReducedErrorFP | OptimizationFlag::Unsafe_InaccurateNaN
                                | OptimizationFlag::Unsafe_IgnoreStandardFPCRValue;
    }
    if (fast_fp && std::strcmp(fast_fp, "1") == 0) {
        config.optimizations |= OptimizationFlag::Unsafe_UnfuseFMA;
    }
    return config;
}

// Which cache new and moving CPUs use. See the rules at the top.
struct Shared {
    std::mutex mutex;
    bool sharing = true;
    std::size_t emitters = DEFAULT_EMITTERS;
    Dynarmic::A64::UserConfig config;
    std::shared_ptr<Dynarmic::A64::CodeCache> current;
    // `current`'s address, read without the lock at the start of every run. A CPU holds its own
    // cache alive, so an address equal to its cache's is that cache, never a new one at a reused
    // address.
    std::atomic<const Dynarmic::A64::CodeCache*> current_address{nullptr};
    std::uint64_t caches = 0;
};

// Under `shared.mutex`.
void set_current(Shared& state, std::shared_ptr<Dynarmic::A64::CodeCache> cache) {
    state.current = std::move(cache);
    state.current_address.store(state.current.get(), std::memory_order_release);
}

Shared& shared() {
    static Shared* instance = [] {
        auto* made = new Shared;
        const char* value = std::getenv("UXDROID_SHARED_CODE");
        made->sharing = !(value != nullptr && std::strcmp(value, "0") == 0);
        made->config = base_config();
        if (const char* count = std::getenv("UXDROID_CODE_EMITTERS")) {
            char* end = nullptr;
            const unsigned long long parsed = std::strtoull(count, &end, 10);
            if (end != count && *end == '\0' && parsed >= 1 && parsed <= MAXIMUM_EMITTERS) {
                made->emitters = static_cast<std::size_t>(parsed);
            }
        }
        const std::size_t room = made->config.code_cache_size / (MINIMUM_CODE_CACHE_MIB * 1024 * 1024);
        made->emitters = std::max<std::size_t>(1, std::min(made->emitters, room));
        if (!made->sharing) {
            made->emitters = 1;
        }
        return made;
    }();
    return *instance;
}

// Under `shared.mutex`.
std::shared_ptr<Dynarmic::A64::CodeCache> make_cache(Shared& state) {
    auto cache = std::make_shared<Dynarmic::A64::CodeCache>(state.config, state.emitters);
    ++state.caches;
    return cache;
}

// The cache a CPU should run from now: the current one, or a new one of its own without sharing.
// `full` is the cache it just found full, which must not be handed back.
std::shared_ptr<Dynarmic::A64::CodeCache> cache_for(const Dynarmic::A64::CodeCache* full) {
    Shared& state = shared();
    std::lock_guard<std::mutex> guard{state.mutex};
    if (!state.sharing) {
        return make_cache(state);
    }
    if (!state.current || state.current.get() == full) {
        set_current(state, make_cache(state));
    }
    return state.current;
}

}  // namespace

extern "C" {

__attribute__((visibility("default"))) std::uint32_t uxd_abi_version() noexcept {
    return ABI_VERSION;
}

// Makes a CPU whose `TPIDR_EL0` is `*tpidr_el0`. Null, with a reason in `error`, on failure.
__attribute__((visibility("default"))) uxd_cpu* uxd_cpu_new(const uxd_callbacks* callbacks,
                                                            std::uint64_t* tpidr_el0, char* error,
                                                            std::size_t error_len) noexcept {
    auto fail = [&](const char* reason) -> uxd_cpu* {
        if (error_len > 0) {
            std::snprintf(error, error_len, "%s", reason);
        }
        return nullptr;
    };
    uxd_cpu* cpu = nullptr;
    try {
        cpu = new uxd_cpu;
        cpu->host = *callbacks;
        Dynarmic::A64::UserConfig config = shared().config;
        config.tpidr_el0 = tpidr_el0;
        cpu->jit = new Dynarmic::A64::Jit(config, cache_for(nullptr));
        return cpu;
    } catch (const std::exception& exception) {
        delete cpu;
        return fail(exception.what());
    } catch (...) {
        delete cpu;
        return fail("cannot create the JIT");
    }
}

__attribute__((visibility("default"))) void uxd_cpu_free(uxd_cpu* cpu) noexcept {
    if (cpu == nullptr) {
        return;
    }
    delete cpu->jit;
    delete cpu;
}

// Runs from the current PC until a callback halts. Returns Dynarmic's halt reason bits, never
// CodeCacheFull, which is handled here.
__attribute__((visibility("default"))) std::uint32_t uxd_cpu_run(uxd_cpu* cpu) noexcept {
    uxd_cpu* const outer = running;
    running = cpu;
    Dynarmic::HaltReason reason{};
    try {
        // Between runs, so the CPU is in no code of its cache. The lock only when it moves.
        Shared& state = shared();
        if (state.sharing && state.current_address.load(std::memory_order_acquire) != cpu->jit->GetCodeCache().get()) {
            std::shared_ptr<Dynarmic::A64::CodeCache> current;
            {
                std::lock_guard<std::mutex> guard{state.mutex};
                current = state.current;
            }
            if (current && current != cpu->jit->GetCodeCache()) {
                cpu->jit->SetCodeCache(std::move(current));
            }
        }
        for (;;) {
            reason = cpu->jit->Run();
            if (!Dynarmic::Has(reason, Dynarmic::HaltReason::CodeCacheFull)) {
                break;
            }
            cpu->jit->SetCodeCache(cache_for(cpu->jit->GetCodeCache().get()));
            reason = reason & ~Dynarmic::HaltReason::CodeCacheFull;
            // Only the full cache stopped the run: go on at the same PC.
            if (static_cast<std::uint32_t>(reason) == 0) {
                continue;
            }
            break;
        }
    } catch (...) {
        // A cache that could not be made: Dynarmic throws only for allocation failures.
        std::abort();
    }
    running = outer;
    cpu->jit->ClearHalt(Dynarmic::HaltReason::UserDefined1);
    return static_cast<std::uint32_t>(reason);
}

__attribute__((visibility("default"))) void uxd_stats_get(uxd_stats* out) noexcept {
    Shared& state = shared();
    std::lock_guard<std::mutex> guard{state.mutex};
    out->blocks = Dynarmic::A64::CodeCache::TotalBlockCount();
    out->code_bytes = state.current ? state.current->CodeSize() : 0;
    out->caches = state.caches;
    out->watched_translations = watched_translations.load(std::memory_order_relaxed);
}

// Counts from now on how many times the code at `address` is read to be translated.
__attribute__((visibility("default"))) void uxd_watch(std::uint64_t address) noexcept {
    watched_translations.store(0, std::memory_order_relaxed);
    watched_address.store(address, std::memory_order_relaxed);
}

// Copies the location descriptors (PC and FPCR bits) of the blocks the current cache translated,
// oldest first, up to `capacity` of them, and returns how many there are.
__attribute__((visibility("default"))) std::size_t uxd_translated(std::uint64_t* out, std::size_t capacity) noexcept {
    std::shared_ptr<Dynarmic::A64::CodeCache> current;
    {
        Shared& state = shared();
        std::lock_guard<std::mutex> guard{state.mutex};
        current = state.current;
    }
    if (!current) {
        return 0;
    }
    try {
        const std::vector<std::uint64_t> locations = current->Locations();
        const std::size_t copied = std::min(capacity, locations.size());
        std::memcpy(out, locations.data(), copied * sizeof(std::uint64_t));
        return locations.size();
    } catch (...) {
        return 0;
    }
}

// Translates the block at each location descriptor ahead of any CPU needing it, into the current
// cache, on the calling thread, which must not be running guest code. Only code inside `ranges`,
// `range_count` pairs of start and end addresses that must be readable, is decoded: a block that
// reaches a range's end stops there. Returns how many locations it went through before the cache
// filled or translation failed.
__attribute__((visibility("default"))) std::size_t uxd_pretranslate(const std::uint64_t* locations, std::size_t count,
                                                                   const std::uint64_t* ranges, std::size_t range_count) noexcept {
    if (!shared().sharing || running != nullptr || ranges == nullptr || range_count == 0) {
        return 0;
    }
    std::size_t done = 0;
    pretranslate_ranges = ranges;
    pretranslate_range_count = range_count;
    try {
        std::shared_ptr<Dynarmic::A64::CodeCache> cache = cache_for(nullptr);
        while (done < count && cache->Translate(locations[done])) {
            ++done;
        }
    } catch (...) {
        // `done` still counts what was translated before the failure.
    }
    pretranslate_ranges = nullptr;
    pretranslate_range_count = 0;
    return done;
}

// Makes the current cache take no more blocks and a new one current: each CPU moves to it at the
// start of its next run, or, while it runs, at the first block the old cache lacks.
__attribute__((visibility("default"))) void uxd_retire_cache() noexcept {
    Shared& state = shared();
    std::lock_guard<std::mutex> guard{state.mutex};
    if (!state.sharing) {
        return;
    }
    if (state.current) {
        state.current->Retire();
    }
    try {
        set_current(state, make_cache(state));
    } catch (...) {
        // Left without a current cache, the next CPU to need one makes it.
        set_current(state, nullptr);
    }
}

__attribute__((visibility("default"))) std::uint64_t uxd_get_x(const uxd_cpu* cpu, std::uint32_t i) noexcept {
    return cpu->jit->GetRegister(i);
}
__attribute__((visibility("default"))) void uxd_set_x(uxd_cpu* cpu, std::uint32_t i, std::uint64_t value) noexcept {
    cpu->jit->SetRegister(i, value);
}
__attribute__((visibility("default"))) std::uint64_t uxd_get_sp(const uxd_cpu* cpu) noexcept {
    return cpu->jit->GetSP();
}
__attribute__((visibility("default"))) void uxd_set_sp(uxd_cpu* cpu, std::uint64_t value) noexcept {
    cpu->jit->SetSP(value);
}
__attribute__((visibility("default"))) std::uint64_t uxd_get_pc(const uxd_cpu* cpu) noexcept {
    return cpu->jit->GetPC();
}
__attribute__((visibility("default"))) void uxd_set_pc(uxd_cpu* cpu, std::uint64_t value) noexcept {
    cpu->jit->SetPC(value);
}
__attribute__((visibility("default"))) void uxd_get_v(const uxd_cpu* cpu, std::uint32_t i, std::uint64_t out[2]) noexcept {
    const auto vector = cpu->jit->GetVector(i);
    out[0] = vector[0];
    out[1] = vector[1];
}
__attribute__((visibility("default"))) void uxd_set_v(uxd_cpu* cpu, std::uint32_t i, const std::uint64_t value[2]) noexcept {
    cpu->jit->SetVector(i, {value[0], value[1]});
}
__attribute__((visibility("default"))) std::uint32_t uxd_get_fpcr(const uxd_cpu* cpu) noexcept {
    return cpu->jit->GetFpcr();
}
__attribute__((visibility("default"))) void uxd_set_fpcr(uxd_cpu* cpu, std::uint32_t value) noexcept {
    cpu->jit->SetFpcr(value);
}
}  // extern "C"
