# uxdroid architecture

uxdroid runs Android APKs as ordinary Linux processes: no container, no VM, no Android system image. It turns an APK into a host executable backed by a shared runtime, and implements the Android platform as libraries on top of Linux.

## The four layers

| Layer | Responsibility | Language |
|-------|----------------|----------|
| APK tooling | Parse the APK, plan execution, compile DEX ahead of time, generate the launcher | Rust |
| Native bridge | Load bionic-linked `.so` files under glibc, translate foreign ABIs | Rust |
| Managed runtime | Execute DEX as machine code | ART (C++, reused) |
| Platform | `android.*` API: AOSP's framework, with the natives and system services below it implemented over Wayland, Vulkan/EGL and PipeWire | Java/Kotlin + Rust |

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

One way around this rule remains: ART calls glibc's `dlopen` first, so a library with no `DT_NEEDED` entries loads through glibc and binds without the audit. Checking the bridge first needs a small patch to `libnativeloader`.

Not translated yet: static initializers with non-zero bionic values, such as `PTHREAD_RECURSIVE_MUTEX_INITIALIZER`. Libraries using ELF TLS, which means a minimum SDK of 29 or higher with `thread_local`, are rejected.

### ART is built for the host, not borrowed from a device

ART's host build links against a glibc 2.17 sysroot and needs no bionic, so `dalvikvm` is an ordinary Linux program. A device APEX was rejected: it writes bionic's TLS slots, which overlap glibc's thread control block. The pinned `master-art` tree downloads with `runtime/art/sync.sh` and builds in Docker with `runtime/art/build.sh`, and the result runs in a plain Ubuntu with no Android tree.

### One process, joined through NativeBridge

`uxdroid run` replaces itself with `dalvikvm`. ART tries glibc's `dlopen` on an app's native library, which fails for anything linked to bionic's `libc.so`, then falls back to `crates/nativebridge`, a Rust library that loads it with the bridge. CPU and calling convention match, so a JNI call goes straight to the function's address with no translation. This same hook is where `arm64-v8a` translation will attach. `crates/platform` holds the native half of the `android.*` classes in `platform/java`. The classes are on the class path for now, and will move to the boot class path when there are enough to be worth compiling into the image.

The window, audio and GPU stay behind this layer, so a later macOS port does not reach into the rest.

### The platform is AOSP's own framework

Writing the `android.*` API ourselves would never end. A spike ran AOSP's real Android 16 framework on our host ART instead: a framework `TextView` measured itself and drew text through the real Skia and Minikin, reading the real `framework-res.apk`. What the framework expects from below is much narrower than its public API:

| Below the framework | Who provides it |
|---------------------|-----------------|
| Graphics, text, resources, input events, `MessageQueue` | AOSP's host `libandroid_runtime`, built for glibc |
| Natives nobody implemented yet | A default body that returns the type's zero, bound at install time and logged when called |
| Logging, properties, clocks, local `Binder` objects | Our natives |
| System services | In-process proxies of their AIDL interfaces, with no IPC; unimplemented calls return defaults and are counted |

`crates/system` holds our natives and the default bodies: install time lists every framework native in `framework/natives.txt`, and at startup each gets a 32-byte stub that reports its first call and returns zero. `dev.uxdroid.system.Boot` runs the startup order. `dev.uxdroid.system.Services` serves each system service as a proxy of its AIDL interface and reports the first call of every method nobody handles, and `AppContext` extends a `DefaultContext` generated from `android.content.Context` at install time. A default body that matches the native-object pattern returns more than zero: in a class whose native hands `NativeAllocationRegistry` a free function, the natives that create objects return a fresh small allocation and that function frees it, because the registry refuses null. Libraries the framework loads by name only for natives that already have default bodies, such as `libstats_jni.so`, are installed empty.

`dev.uxdroid.system.Launcher` starts an app the way Android does. It parses the APK with the framework's `PackageParser` and calls `ActivityThread.main`, which attaches to `IActivityManager`. That service is a handler in the same process: it calls `bindApplication` and schedules a `ClientTransaction` with a `LaunchActivityItem`, so the framework itself makes the `LoadedApk`, the `Application`, the `Activity` through `Instrumentation`, its `ContextImpl` and its `PhoneWindow`. The launcher plays `system_server` for what an app receives at bind: the memory of `ApplicationSharedMemory` (natives in `crates/system`), one display, one unlocked user, and system content providers such as Settings as `IContentProvider` proxies. One difference from Android remains: the framework is on the system class path, not the boot class path, so the launcher caches the app's `LoadedApk` with the system class loader as parent before the bind runs.

The spike measured 28 MiB of private dirty memory per process. Most of the rest was clean framework pages, shared between processes.

### Guest architecture stays inside the native bridge

The platform is built once, for the host. Only an app's own `.so` files have a guest ABI, and only the bridge knows which one. These rules let `arm64-v8a`, and later `armeabi-v7a`, arrive without changes elsewhere:

1. ART, the framework, the services and our natives have a single host build. Nothing outside `crates/bridge` and `crates/nativebridge` depends on the app's ABI.
2. Every function we expose to app native code, such as `libandroid`, `liblog`, EGL or OpenSL ES, is declared in a machine-readable signature table next to its implementation. Direct loading binds the table's addresses. A translator generates guest-to-host trampolines from the same table.
3. App native code reaches the platform only through JNI and those functions. ART passes each JNI method's shorty to `get_trampoline`, which is where a translator wraps guest code.
4. Calling-convention shortcuts stay on the host side. The default bodies above are called only by host ART, never by guest code.
5. Install time picks the ABI: the host's when the APK has it, otherwise a guest ABI the translator supports. The choice is recorded with the install.

`armeabi-v7a` also needs pointer and struct conversion between 32-bit guest code and the 64-bit host at every crossing. That is the translator's job and is not designed yet.

## Roadmap

Each milestone removes the largest remaining risk first.

| # | Milestone | Proves |
|---|-----------|--------|
| 0 | `uxdroid inspect` | We can read real APKs and plan their execution |
| 1 | Bionic loader in Rust | An NDK-built x86_64 `.so` runs under glibc |
| 2 | ART boots on the host | A DEX `main()` runs as compiled code |
| 3 | Launcher and first `Activity` | An APK opens a Wayland window |
| 4 | AOSP's framework | A real app, RustDesk's `x86_64` build, opens its screen |
| 5 | `arm64-v8a` translation | ARM-only native apps run on x86_64 |
| 6 | Google Play | See below |
| 7 | macOS | Once the product is complete and optimized on Linux. iOS is out of scope. |

## Known limit: Google Play

Installing apps through the Play Store requires Google Play Services, which is proprietary and designed for certified devices. Apps that use the [Play Integrity API](https://developer.android.com/google/play/integrity/overview) can check for a certified device, and uxdroid will not be one. Expect those apps to refuse to run, whatever the quality of this layer.
