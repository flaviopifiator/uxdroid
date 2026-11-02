# uxdroid architecture

uxdroid runs Android APKs as ordinary Linux processes: no container, no VM, no Android system image. It turns an APK into a host executable backed by a shared runtime, and implements the Android platform as libraries on top of Linux.

## The four layers

| Layer | Responsibility | Language |
|-------|----------------|----------|
| APK tooling | Parse the APK, plan execution, compile DEX ahead of time, generate the launcher | Rust |
| Native bridge | Load bionic-linked `.so` files under glibc, translate foreign ABIs | Rust |
| Managed runtime | Execute DEX as machine code | ART (C++, reused) |
| Platform | `android.*` API: Java classes on the boot classpath, native methods over JNI, backed by Wayland, Vulkan/EGL and PipeWire | Java/Kotlin + Rust |

## What "native executable" means

An APK holds two kinds of code, and they take different paths:

| Code | Path | CPU translation |
|------|------|-----------------|
| DEX bytecode | `dex2oat` compiles it to x86_64 at install time | None |
| `.so` for the host ABI | Loaded directly by the bridge | None |
| `.so` for another ABI (usually `arm64-v8a`) | Runs through a binary translator | Yes, this is the only emulated path |

The result is a launcher ELF plus compiled artifacts. It still needs the shared runtime and platform libraries, the same way a Java or Python program needs its runtime.

## Decisions

### Rust for everything we write natively

Go is rejected for this layer. ART installs signal handlers and relies on `SIGSEGV` for implicit null checks, and the bridge must control TLS, stacks and the calling convention. Go's runtime owns signals and goroutine stacks, adds cgo cost on every JNI crossing, and adds a garbage collector next to ART's. Rust has no runtime to conflict with and calls C at zero cost.

### Reuse ART, do not rewrite it

A competitive DEX runtime means a compiler, a garbage collector and years of tuning. ART is Apache-2.0 and already does this. Performance work goes where Android-on-Linux loses time: startup, IPC and graphics paths.

### No Binder in the app process path

Inside one app, a platform service call becomes a direct in-process call instead of an IPC round trip. This is the main performance advantage over a container and removes the kernel module requirement. Cross-app features need a separate design when we reach them.

### Reference code stays reference

Android Translation Layer (ATL) is GPL-3.0-or-later. We study its approach but copy no code from it, so uxdroid's license stays an open choice. AOSP code is Apache-2.0 and can be reused with attribution.

### The loader binds to glibc only what was audited

bionic and glibc share most symbol names but not their ABI. Probes compiled against both measured `sigset_t` at 8 and 128 bytes, `jmp_buf` at 88 and 200, `regex_t` at 32 and 64, different `sysconf`, `pathconf` and `EAI_*` constants, and `addrinfo` with two fields swapped. Passing those calls to glibc corrupts memory with no error, and a list of known bad symbols can never be complete. So the default is to refuse:

| Symbol | Result |
|--------|--------|
| Listed in `crates/bridge/src/glibc_compatible.txt` | Binds directly, with no added cost |
| Different ABI, translatable | Binds to a shim in `crates/bridge/src/shims.rs` |
| A function in glibc that nobody audited | Loads, and stops the program naming the symbol if it is called |
| Data in glibc that nobody audited | The load fails, because data cannot trap |
| Found nowhere | The load fails, listing every missing name |

Printing a `long double` also stops by name, because the `printf` family checks its format. To support a new symbol, compare its signature, structs and constants against the NDK headers, then add it to the list. The audit covers signatures, measured layouts and constants, and individual behavior differences can remain, such as `sysconf` returning `EINVAL` for names not yet mapped.

Not translated yet: static initializers with non-zero bionic values, such as `PTHREAD_RECURSIVE_MUTEX_INITIALIZER`. Libraries using ELF TLS, which means a minimum SDK of 29 or higher with `thread_local`, are rejected.

### ART is built for the host, not borrowed from a device

ART's host build links against a glibc 2.17 sysroot and needs no bionic, so `dalvikvm` is an ordinary Linux program. A device APEX was rejected: it writes bionic's TLS slots, which overlap glibc's thread control block. `master-art` builds in Docker with `runtime/art/build.sh`, and the result runs in a plain Ubuntu with no Android tree.

## Roadmap

Each milestone removes the largest remaining risk first.

| # | Milestone | Proves |
|---|-----------|--------|
| 0 | `uxdroid inspect` | We can read real APKs and plan their execution |
| 1 | Bionic loader in Rust | An NDK-built x86_64 `.so` runs under glibc |
| 2 | ART boots on the host | A DEX `main()` runs as compiled code |
| 3 | Launcher and first `Activity` | An APK opens a Wayland window |
| 4 | `arm64-v8a` translation | ARM-only native apps run on x86_64 |
| 5 | Google Play | See below |
| 6 | macOS | Once the product is complete and optimized on Linux. iOS is out of scope. |

## Known limit: Google Play

Installing apps through the Play Store requires Google Play Services, which is proprietary and designed for certified devices. Apps that use the [Play Integrity API](https://developer.android.com/google/play/integrity/overview) can check for a certified device, and uxdroid will not be one. Expect those apps to refuse to run, whatever the quality of this layer.
