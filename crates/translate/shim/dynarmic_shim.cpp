// A C interface over Dynarmic's A64 JIT, loaded by `uxdroid-translate` with dlopen.
//
// Guest memory is the host's: guest addresses are host addresses (fastmem with a zero base and a
// 64-bit address space), so translated code reads and writes the process' memory directly. The
// memory callbacks below are the slow path Dynarmic falls back to after a fastmem access faults,
// and do the same direct access. Every exported function is noexcept: no C++ exception crosses
// into Rust.

#include <atomic>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <ctime>
#include <exception>
#include <mutex>

#include "dynarmic/interface/A64/a64.h"
#include "dynarmic/interface/A64/config.h"
#include "dynarmic/interface/exclusive_monitor.h"

namespace {

constexpr std::uint32_t ABI_VERSION = 1;
// Every live CPU needs its own slot in the exclusive monitor, which scans all of them on each
// exclusive store, so the count stays small: one CPU per guest thread and nesting depth.
constexpr std::size_t MAX_CPUS = 64;
// Executable memory each CPU may fill with translated code before Dynarmic clears its cache.
constexpr std::size_t CODE_CACHE_SIZE = 64 * 1024 * 1024;
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

namespace {

Dynarmic::ExclusiveMonitor& monitor() {
    static Dynarmic::ExclusiveMonitor instance{MAX_CPUS};
    return instance;
}

std::mutex slots_lock;
bool slots[MAX_CPUS];

int take_slot() {
    std::lock_guard<std::mutex> guard{slots_lock};
    for (std::size_t i = 0; i < MAX_CPUS; ++i) {
        if (!slots[i]) {
            slots[i] = true;
            return static_cast<int>(i);
        }
    }
    return -1;
}

void give_slot(std::size_t slot) {
    std::lock_guard<std::mutex> guard{slots_lock};
    slots[slot] = false;
}

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

struct Callbacks final : Dynarmic::A64::UserCallbacks {
    uxd_callbacks host{};
    Dynarmic::A64::Jit* jit = nullptr;

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

    void InterpreterFallback(std::uint64_t pc, std::size_t) override {
        host.exception(host.context, pc, KIND_INTERPRETER_FALLBACK);
        jit->HaltExecution();
    }

    void CallSVC(std::uint32_t number) override {
        if (host.svc(host.context, number) != 0) {
            jit->HaltExecution();
        }
    }

    void ExceptionRaised(std::uint64_t pc, Dynarmic::A64::Exception exception) override {
        using E = Dynarmic::A64::Exception;
        switch (exception) {
        // Hints a user-space thread may execute; nothing to do.
        case E::Yield:
        case E::WaitForEvent:
        case E::WaitForInterrupt:
        case E::SendEvent:
        case E::SendEventLocal:
            jit->SetPC(pc + 4);
            return;
        case E::UnallocatedEncoding:
            host.exception(host.context, pc, KIND_UNALLOCATED);
            break;
        case E::ReservedValue:
            host.exception(host.context, pc, KIND_RESERVED);
            break;
        case E::UnpredictableInstruction:
            host.exception(host.context, pc, KIND_UNPREDICTABLE);
            break;
        case E::Breakpoint:
            host.exception(host.context, pc, KIND_BREAKPOINT);
            break;
        case E::NoExecuteFault:
            host.exception(host.context, pc, KIND_NO_EXECUTE);
            break;
        default:
            host.exception(host.context, pc, KIND_OTHER);
            break;
        }
        jit->HaltExecution();
    }

    void AddTicks(std::uint64_t) override {}
    std::uint64_t GetTicksRemaining() override { return UINT64_MAX; }
    std::uint64_t GetCNTPCT() override {
        timespec now{};
        clock_gettime(CLOCK_MONOTONIC, &now);
        return static_cast<std::uint64_t>(now.tv_sec) * CNTFRQ + static_cast<std::uint64_t>(now.tv_nsec);
    }
};

}  // namespace

struct uxd_cpu {
    Callbacks callbacks;
    std::size_t slot = 0;
    Dynarmic::A64::Jit* jit = nullptr;
};

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
    const int slot = take_slot();
    if (slot < 0) {
        return fail("too many guest CPUs");
    }
    uxd_cpu* cpu = nullptr;
    try {
        cpu = new uxd_cpu;
        cpu->slot = static_cast<std::size_t>(slot);
        cpu->callbacks.host = *callbacks;

        Dynarmic::A64::UserConfig config;
        config.callbacks = &cpu->callbacks;
        config.processor_id = cpu->slot;
        config.global_monitor = &monitor();
        config.tpidr_el0 = tpidr_el0;
        config.cntfrq_el0 = CNTFRQ;
        config.fastmem_pointer = 0;
        config.fastmem_address_space_bits = 64;
        config.fastmem_exclusive_access = true;
        config.enable_cycle_counting = false;
        config.code_cache_size = CODE_CACHE_SIZE;
        cpu->jit = new Dynarmic::A64::Jit(config);
        cpu->callbacks.jit = cpu->jit;
        return cpu;
    } catch (const std::exception& exception) {
        delete cpu;
        give_slot(static_cast<std::size_t>(slot));
        return fail(exception.what());
    } catch (...) {
        delete cpu;
        give_slot(static_cast<std::size_t>(slot));
        return fail("cannot create the JIT");
    }
}

__attribute__((visibility("default"))) void uxd_cpu_free(uxd_cpu* cpu) noexcept {
    if (cpu == nullptr) {
        return;
    }
    monitor().ClearProcessor(cpu->slot);
    delete cpu->jit;
    give_slot(cpu->slot);
    delete cpu;
}

// Runs from the current PC until a callback halts. Returns Dynarmic's halt reason bits.
__attribute__((visibility("default"))) std::uint32_t uxd_cpu_run(uxd_cpu* cpu) noexcept {
    const auto reason = cpu->jit->Run();
    cpu->jit->ClearHalt(Dynarmic::HaltReason::UserDefined1);
    return static_cast<std::uint32_t>(reason);
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
__attribute__((visibility("default"))) void uxd_invalidate(uxd_cpu* cpu, std::uint64_t start, std::uint64_t len) noexcept {
    cpu->jit->InvalidateCacheRange(start, static_cast<std::size_t>(len));
}

}  // extern "C"
