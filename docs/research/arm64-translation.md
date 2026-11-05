# arm64 translation: options for milestone 5

Research of 2026-11-04 for running apps whose native libraries exist only for `arm64-v8a` on an `x86_64` host, inside the native bridge. Sources are primary (AOSP Gitiles, project repositories); speed classes are qualitative, nothing here was measured.

## Recommendation

Keep the frame in Rust inside `crates/bridge` and put the translation engine behind one trait. Start with Dynarmic (0BSD) as the engine, measure Berberis/Digitalis as the competitor and fallback, and grow install-time work into a persistent translation cache.

## Findings

| Engine | License | Direction | Ahead of time | Fit |
|--------|---------|-----------|---------------|-----|
| AOSP Berberis | Apache-2.0 | riscv64 to x86_64 only upstream; arm64 has guest state and ABI code but no decoder, interpreter or translator; public commits stop about April 2025 | No | Design reference; proxy-library idea matches uxdroid's signature tables |
| Digitalis (Berberis fork, branch `android16`) | Apache-2.0 sources | arm64 to x86_64 at every Berberis stage, active in 2026 | No | Builds only inside an AOSP tree for the emulator; small team; competitor and fallback engine |
| Dynarmic (`azahar-emu/dynarmic` fork keeps `frontend/A64`) | 0BSD | A32/A64 to x64 | No | Embeddable JIT with callbacks for memory and system calls and no OS layer, so libc stays on the host |
| QEMU user (TCG) | GPL-2.0 | arm64 to x86_64 | No | Isolated benchmark only |
| Box64, FEX | MIT | x86 to arm64 | No | Wrong direction; FEX thunks are a design reference |
| Houdini, Google's closed arm64 translator | Proprietary | arm to x86 | No | Cannot ship |
| Remill, mctoll (lift to LLVM IR) | Apache-2.0 | static lifting | Yes | Heavy LLVM dependency, still needs a JIT for indirect branches |

None of the usable engines translates ahead of time, so install time can resolve imports, build JNI trampolines and record function boundaries, and machine code cached across runs is uxdroid's own work.

## What the bridge needs first

- Guest libc as a signature table of its own: `shims.rs` resolves libc names without signatures, and `glibc_compatible.txt` relies on bionic and glibc layouts matching on x86_64, which does not hold for arm64 (`stat`, `epoll_event`, `sigaction`, signal sets, `ucontext`, `jmp_buf`).
- Callback signatures (`fn:i32(i32,i32,ptr)` style) instead of a bare `ptr`, for ALooper, audio, `pthread_create`, `qsort`, `atexit`, `dl_iterate_phdr` and log hooks.
- JNI wrappers keyed by the method's shorty (the NativeBridge entry ignores it today), a guest JNIEnv of guest-callable stubs, and stubs for host function pointers handed out by `eglGetProcAddress`, OpenSL ES and `dlsym`.
- Calling-convention conversion AAPCS64 to SysV (x0-x7 to rdi..r9 plus stack, v0-v7 to xmm0-7, x8 indirect result, explicit i8/i16 extension, 128-bit `long double`), and `va_list` conversion for the printf family and JNI `Call*MethodV`.
- Guest TLS in a per-thread guest block (TPIDR_EL0), TLSDESC in the guest loader, signals delivered at region boundaries with a guest ucontext, faults mapped back to the guest PC, `AT_HWCAP` reporting ARMv8.0 FP and ASIMD only.

## First spike

An arm64 NDK test library with `JNI_OnLoad`, `RegisterNatives`, `__android_log_print`, `pthread_create` with a callback, `qsort`, a C++ exception and `thread_local`, plus a compute kernel (zlib deflate and SHA-256 over 64 MiB). Goals: every case passes under `benches/translation/`, compute speed as a percentage of the native x86_64 build with QEMU user as a measured competitor, translation time and memory reported. RustDesk's arm64 build is the second step: Flutter's engine and Dart AOT code all run translated, so performance is the main risk.
