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
| Exported by an Android system library the library needs, such as `libandroid.so` | Binds to that library's signature table, below |
| Listed in `crates/bridge/src/glibc_compatible.txt` | Binds directly, with no added cost |
| Different ABI, translatable | Binds to a shim in `crates/bridge/src/shims.rs` |
| A function in glibc that nobody audited | Loads, and stops the program naming the symbol if it is called |
| Data in glibc that nobody audited | The load fails, because data cannot trap |
| Found nowhere | The load fails, listing every missing name |

Printing a `long double` also stops by name, because the `printf` family checks its format. To support a new symbol, compare its signature, structs and constants against the NDK headers, then add it to the list. The audit covers signatures, measured layouts and constants, and individual behavior differences can remain, such as `sysconf` returning `EINVAL` for names not yet mapped.

One way around this rule remains: ART calls glibc's `dlopen` first, so a library with no `DT_NEEDED` entries loads through glibc and binds without the audit. Checking the bridge first needs a small patch to `libnativeloader`.

App code names files of Android's system partition by absolute path, such as Skia reading `/system/etc/fonts.xml`. The functions that take a path, such as `open`, `fopen`, `stat` and `opendir`, are shims that move `/system` to the runtime root `ANDROID_ROOT` names, as the framework's rewritten paths do; every other path reaches glibc unchanged. A segment whose file cannot be mapped executable, because its mount is `noexec` like the sandbox's `/tmp`, is read into anonymous memory instead, which costs its sharing between processes.

Not translated yet: static initializers with non-zero bionic values, such as `PTHREAD_RECURSIVE_MUTEX_INITIALIZER`. Libraries using ELF TLS, which means a minimum SDK of 29 or higher with `thread_local`, are rejected.

### ART is built for the host, not borrowed from a device

ART's host build links against a glibc 2.17 sysroot and needs no bionic, so `dalvikvm` is an ordinary Linux program. A device APEX was rejected: it writes bionic's TLS slots, which overlap glibc's thread control block. The pinned `master-art` tree downloads with `runtime/art/sync.sh` and builds in Docker with `runtime/art/build.sh`, and the result runs in a plain Ubuntu with no Android tree.

### One process, joined through NativeBridge

`uxdroid run` replaces itself with `dalvikvm`. ART tries glibc's `dlopen` on an app's native library, which fails for anything linked to bionic's `libc.so`, then falls back to `crates/nativebridge`, a Rust library that loads it with the bridge. CPU and calling convention match, so a JNI call goes straight to the function's address with no translation. This same hook is where `arm64-v8a` translation will attach. `crates/platform` holds the native half of the `android.*` classes in `platform/java`. The classes are on the class path for now, and will move to the boot class path when there are enough to be worth compiling into the image.

The window, audio and GPU stay behind this layer, so a later macOS port does not reach into the rest.

### Android's system libraries are signature tables

App libraries name `liblog.so`, `libandroid.so`, `libjnigraphics.so`, `libEGL.so`, `libGLESv2.so`, `libGLESv3.so`, `libOpenSLES.so` and `libaaudio.so` in `DT_NEEDED`, or open them with `dlopen`, as oboe opens `libaaudio.so`. None of them is a file. Each is a table of `uxdroid_bridge::table`: every export with its host address and its signature in a closed vocabulary, such as `i32(i32,ptr,ptr,...)` for `__android_log_print` or `ptr` for an object like `SL_IID_ENGINE`. `liblog`'s table is in the bridge, next to the printf machinery it shares with bionic's libc, and every library sees it, as on Android, where `app_process` links it. The others are in `crates/ndk`, and `crates/nativebridge` hands them to the loader before the first app library loads. A library searches its loaded dependencies first, then the tables it needs, then the shims, and a name its table lacks is reported missing like any other. The EGL and GLES tables are generated from the NDK's headers and stubs by `crates/ndk/generate.sh`.

| Library | Served by |
|---------|-----------|
| `liblog` | stderr, with the minimum priority, default tag, logger and aborter an app may set |
| `libandroid` assets | The framework's `AssetManager`: each asset is read into memory when it opens, and stored entries also open as file descriptors |
| `libandroid` looper | One epoll set per thread. On a thread with a Java `Looper`, `MessageQueue.addOnFileDescriptorEventListener` watches that set, so the app's callbacks run inside `Looper.loop` |
| `libandroid` native window | The Java `Surface`: a lock hands out a buffer, and posting copies it into a `Bitmap` and draws it through `Surface.lockCanvas` |
| `libjnigraphics` | `Bitmap.getPixels` and `setPixels`, converted to Android's memory layout |
| `libEGL` | Mesa's surfaceless platform, with llvmpipe when there is no GPU. A window surface is a pbuffer that each swap reads back into its native window. Configs report `EGL_WINDOW_BIT` and Android's visual IDs |
| `libGLESv2`, `libGLESv3` | Mesa's own functions, at no added cost |
| `libOpenSLES` | An engine, output mixes, PCM buffer-queue audio players and recorders over `crates/audio` streams. Interface pointers are resolved back to their object and checked as live. Players of URIs and file descriptors, which need a decoder, are refused. The interface IDs are data, bound on purpose: each is a pointer to a GUID of ours, and apps compare the pointers |
| `libaaudio` | AAudio streams over `crates/audio`: data callbacks on the stream's thread, adapted to the size the app asks for, and blocking reads and writes with timeouts |

Pixels cross into Java as `int` arrays, because AOSP's host graphics library cannot take NIO buffers on ART: it looks up OpenJDK's `FileDescriptor.fd`. A native window frame is therefore copied three times before it reaches Wayland, and an EGL frame once more for the readback. A buffer shared with the Wayland window, and an EGL window on its `wl_surface`, would remove those copies.

A translator needs more than the tables: callbacks into app code, such as `ALooper` callbacks and an app's logger, and host functions handed out as pointers, such as `eglGetProcAddress` results and OpenSL ES method tables, cross the same boundary. OpenSL ES's method tables and the audio callbacks, OpenSL ES buffer queues' and AAudio's, already have signatures, in `uxdroid_ndk::opensles_interfaces`.

Not yet: `AChoreographer`, `AHardwareBuffer` and Android image extensions for EGL, OpenSL ES players of encoded data and its event callbacks, and the rest of `libandroid`, such as sensors, input queues and configuration.

### The platform is AOSP's own framework

Writing the `android.*` API ourselves would never end. A spike ran AOSP's real Android 16 framework on our host ART instead: a framework `TextView` measured itself and drew text through the real Skia and Minikin, reading the real `framework-res.apk`. What the framework expects from below is much narrower than its public API:

| Below the framework | Who provides it |
|---------------------|-----------------|
| Graphics, text, resources, input events, `MessageQueue` | AOSP's host `libandroid_runtime`, built for glibc |
| Natives nobody implemented yet | A default body that returns the type's zero, bound at install time and logged when called |
| Logging, properties, clocks, local `Binder` objects | Our natives |
| `SurfaceControl`, `BLASTBufferQueue`, `Surface`, `DisplayEventReceiver` | Our natives, over Wayland and a timer |
| System services | In-process proxies of their AIDL interfaces, with no IPC; unimplemented calls return defaults and are counted |

`crates/system` holds our natives and the default bodies: install time lists every framework native in `framework/natives.txt`, and at startup each gets a 32-byte stub that reports its first call and returns zero. `dev.uxdroid.system.Boot` runs the startup order. `dev.uxdroid.system.Services` serves each system service as a proxy of its AIDL interface and reports the first call of every method nobody handles, and `AppContext` extends a `DefaultContext` generated from `android.content.Context` at install time. A default body that matches the native-object pattern returns more than zero: in a class whose native hands `NativeAllocationRegistry` a free function, the natives that create objects return a fresh small allocation and that function frees it, because the registry refuses null. Libraries the framework loads by name only for natives that already have default bodies, such as `libstats_jni.so`, are installed empty.

`dev.uxdroid.system.Launcher` starts an app the way Android does, after `uxdroid install` has done the installer's part: the APK's native libraries for the chosen ABI are extracted into `ANDROID_DATA/data/<package>/lib`, where `ApplicationInfo.nativeLibraryDir` points, and the ABI is recorded beside them. It parses the APK with the framework's `PackageParser` and calls `ActivityThread.main`, which attaches to `IActivityManager`. That service is a handler in the same process: it calls `bindApplication` and schedules a `ClientTransaction` with a `LaunchActivityItem`, so the framework itself makes the `LoadedApk`, the `Application`, the `Activity` through `Instrumentation`, its `ContextImpl` and its `PhoneWindow`. The launcher plays `system_server` for what an app receives at bind: the memory of `ApplicationSharedMemory` (natives in `crates/system`), one display, one unlocked user, and system content providers such as Settings as `IContentProvider` proxies. One difference from Android remains: the framework is on the system class path, not the boot class path, so the launcher caches the app's `LoadedApk` with the system class loader as parent before the bind runs.

Without `--once`, the launch transaction also holds a `ResumeActivityItem`, as `ActivityTaskManager` sends for an Activity in front, so `handleResumeActivity` adds the decor view through `WindowManagerGlobal` and the framework's `ViewRootImpl`. `WindowService` answers `IWindowManager.openSession` with an in-process `IWindowSession`: `addToDisplayAsUser` reports a window with no system bars, and `relayout` gives each window a layer the size of the display (400x700 dp at 160 dpi) through `SurfaceControl.Builder`. Below it, `crates/system` implements the natives that `system_server` and SurfaceFlinger back on Android:

- `SurfaceControl` and `BLASTBufferQueue` only name a layer and its size. Handles are ids in a registry, never addresses, so the free functions `NativeAllocationRegistry` runs on its own thread cannot touch freed memory.
- `Surface.lockCanvas` points the framework's own `Canvas` at a `Bitmap` the size of the window, and `unlockCanvasAndPost` copies its pixels into the shared-memory buffer of a Wayland toplevel, opened on the first lock with milestone 3's `Window` from `crates/platform`. Drawing is software: the launcher clears `ThreadedRenderer.sRendererEnabled`, the framework's own switch for devices without GPU rendering, so `ViewRootImpl.drawSoftware` runs. `Bitmap.getPixels` yields 0xAARRGGBB, which is Wayland's XRGB8888 whatever Skia's byte order.
- A layer with a parent, such as a `SurfaceView`'s, opens no window. Its frames are kept and composed in software into its root layer's window, at the position its transactions set, below the window's pixels when a layer on its way to the root has a negative z, as SurfaceView's has, so it shows through the hole the window leaves transparent, and above them otherwise. Transactions apply at once, not when the framework applies them; crop, matrix and alpha are ignored.
- `DisplayEventReceiver` paces `Choreographer` with a timer fd on the receiver's Looper, registered through the framework's `MessageQueue.addOnFileDescriptorEventListener`. Each scheduled vsync fires on the next tick of a 60 Hz clock on `CLOCK_MONOTONIC` and calls `dispatchVsync`, as Android's receiver does. Wayland frame callbacks were not used: `Choreographer` runs before any window exists, and compositors stop them for hidden windows.

A window also asks for the input method service, answered by defaults, and for the virtual keyboard's key map, which `InputService` builds with `KeyCharacterMap.obtainEmptyMap`. `Process.readProcLines` is ours too, because the framework reads `/proc/self/status` after each garbage collection and stops the process when that fails.

Real apps ask for more. `content`, `clipboard` and parts of other services are answered by defaults. Storage is one emulated shared volume under `ANDROID_DATA/media/0`, the user is unlocked so credential-protected storage opens, the serial number is `Build.UNKNOWN` as for any app without the privileged permission, and the app has an install source. `Activity.reportFullyDrawn` is printed, because Flutter calls it after its first frame. RustDesk's x86_64 build, a Flutter app, reaches its first frame this way about 380 ms after the framework starts, checked by `runtime/framework/rustdesk_smoke.sh` in the sandbox.

Not yet: input events, resizing a window after it opens, placing dialogs and popups relative to their parent (each window is its own toplevel), and hardware rendering.

The spike measured 28 MiB of private dirty memory per process. Most of the rest was clean framework pages, shared between processes.

### Audio goes to the host's sound server over the PulseAudio protocol

`AudioTrack`, `AudioRecord` and the `AudioSystem` values they read are natives in `crates/system`, so a Java app's sound reaches the server with no `audioserver` and no Binder in between; `IAudioService` gives players ids, full volume and audio focus. Recording, through any API, needs the manifest's `RECORD_AUDIO`: the launcher records the requested permissions in `dev.uxdroid.system.Permissions`, and a desktop has no dialog, so a requested permission counts as granted.

Every Android audio API an app uses becomes one stream of `crates/audio`: interleaved `f32` frames at the app's own rate and channel count, converted from the app's sample format at the API's edge. Each stream has one thread that moves a 10 ms period, as Android's mixer does, between the stream and the device, whose clock paces it. A push stream, such as an `AudioTrack` in streaming mode, buffers what the app writes and plays silence when the buffer runs dry, counting each gap as one underrun. A callback stream, such as AAudio's data callback or an OpenSL ES buffer queue, asks the app for each period on that thread. `AudioTrack.setPlaybackRate` resamples linearly to the stream's rate. Nothing is mixed in the process: each stream is its own client of the server, which mixes and resamples to the hardware.

`UXDROID_AUDIO` chooses the backend:

| Value | Backend |
|-------|---------|
| unset | The PulseAudio protocol, else ALSA's `default` device, else silence with a warning; the first that opens is kept for the process |
| `pulse`, `pulse:<server>` | The PulseAudio protocol only |
| `alsa`, `alsa:<device>` | ALSA only |
| `null` | Silence, at the device clock |
| `file:<dir>` | Each output stream writes `output-<n>-<rate>hz-<channels>ch.f32` into the directory, and input streams read `input.f32` from it, then silence; raw little-endian `f32`, at the device clock. Tests read back every sample |

The PulseAudio protocol was chosen over PipeWire's own because it reaches both servers a Linux desktop runs: PipeWire serves it through `pipewire-pulse`, and older systems run PulseAudio itself. Its client library, `libpulse-simple`, is five functions over a blocking stream, against PipeWire's stream API, which needs the format built as SPA pods from inline header code. The latency an app gets is set explicitly: two periods (20 ms) queued on the server, plus the server's own quantum. PipeWire's native API would save the hop through `pipewire-pulse` and could run the callback on PipeWire's real-time thread; that was not measured, because the test environment has no PipeWire server, and is the first thing to try if the measured latency is not good enough. ALSA is the fallback for hosts without a server: two periods of latency, with its own resampler when the device lacks the rate.

The host's sound libraries are opened with `dlopen` when the first stream opens, never linked, so uxdroid adds no build dependency and starts on hosts without them.

Measured on a private PulseAudio server (`benches/framework/README.md`): callbacks every 10.0 ms, 20.4 ms of latency on the server at 48 kHz, and an AAudio stream's first callback within 15 us of `requestStart`. Not yet: marker and period events of `AudioTrack` and `AudioRecord`, `AudioTrack` loop points, AAudio's error callback, shrinking a buffer below its capacity, volume and focus changes from the host, and input and output device selection.

### Guest architecture stays inside the native bridge

The platform is built once, for the host. Only an app's own `.so` files have a guest ABI, and only the bridge knows which one. These rules let `arm64-v8a`, and later `armeabi-v7a`, arrive without changes elsewhere:

1. ART, the framework, the services and our natives have a single host build. Nothing outside `crates/bridge` and `crates/nativebridge` depends on the app's ABI.
2. Every function we expose to app native code, such as `libandroid`, `liblog`, EGL or OpenSL ES, is declared in a machine-readable signature table next to its implementation, as described above. Direct loading binds the table's addresses. A translator generates guest-to-host trampolines from the same table.
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
