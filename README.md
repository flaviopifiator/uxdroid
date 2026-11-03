# uxdroid

Run Android APKs as native Linux processes, with no emulator, no container and no Android system image.

**Status: milestone 3.** `uxdroid run` opens the launcher Activity of a simple test APK in a Wayland window, joining its DEX and its bionic libraries in one process. Milestone 4, AOSP's own framework below real apps, is in progress: RustDesk's x86_64 build shows its first Flutter screen through `dev.uxdroid.system.Launcher`, with no input yet. See [the architecture](docs/architecture.md) for the design and roadmap.

## Quick path

All builds and tests run in Docker. The only host requirement is Docker with Compose.

```bash
docker compose run --rm dev cargo test --workspace
docker compose run --rm dev cargo build
```

Inspect APKs in the sandbox, which has no network and mounts everything read-only:

```bash
UXDROID_SAMPLES=/path/to/apks docker compose run --rm sandbox \
  /cache/target/debug/uxdroid inspect /samples/app.apk
```

```text
package: org.schabi.newpipe
version: 0.29.1 (1015)
sdk: min 23, target 35
launcher: org.schabi.newpipe.MainActivity
dex: 3 files, 10671 classes, 75742 methods
abis: arm64-v8a (1), armeabi-v7a (1), x86 (1), x86_64 (1)
execution: direct
```

## Reading the result

| `execution` | Meaning |
|-------------|---------|
| `direct` | The APK has no native libraries, or has them for this machine's ABI |
| `needs-abi-bridge (...)` | Native libraries exist only for the listed ABIs and need translation |

Exit codes: `0` success, `1` unreadable or malformed APK, `2` wrong usage.

## Load a native library

`uxdroid load` loads an x86_64 `.so` built for Android, with its dependencies, and runs its initializers. Only an x86_64 glibc host is supported. Loading runs the library's code, so use the sandbox for anything you did not build:

```bash
UXDROID_SAMPLES=/path/to/libs docker compose run --rm sandbox \
  /cache/target/debug/uxdroid load /samples/libfoo.so --search-dir /samples --call answer
```

```text
loaded: libfoo.so
needs: libc.so, libm.so
call answer: 42
```

| Option | Meaning |
|--------|---------|
| `--search-dir <dir>` | Also look for dependencies here, besides the library's own directory. Can repeat. |
| `--call <symbol>` | Call the exported function `int symbol(void)` and print its result. At most once. |

Exit codes: `0` success, `1` the library or a dependency cannot be loaded, or the symbol does not exist, `2` wrong usage. A load that fails names every missing library or symbol it knows of. A function from glibc that has not been audited for bionic compatibility, such as `regcomp`, stops the program with its name if called. See [the architecture](docs/architecture.md).

## Run an APK

`uxdroid run` needs ART built for the host, which takes about 16 GB of download and an hour of build. Only an x86_64 glibc host is supported.

```bash
# Build the runtime in Docker, once. Volumes keep the result.
docker compose --profile art run --rm -T art-builder bash /work/runtime/art/sync.sh
docker compose --profile art run --rm -T art-builder bash /work/runtime/art/build.sh
docker compose --profile art run --rm -T art-builder bash /work/runtime/art/package.sh
docker compose --profile art run --rm -T art-builder bash /work/runtime/art/compile.sh
docker compose --profile art run --rm -T art-builder bash /work/runtime/art/build_platform.sh
# AOSP's Android 16 framework, pinned by checksum: 355 MB of download, 205 MB installed.
docker compose --profile art run --rm -T art-builder bash /work/runtime/framework/fetch.sh
docker compose --profile art run --rm -T art-builder bash /work/runtime/framework/build.sh
docker compose --profile art run --rm -T art-builder bash /work/runtime/framework/build_system.sh
# Check it: the framework draws, writes text and reads its resources.
docker compose run --rm -T dev bash /work/runtime/framework/smoke.sh
# An SDK-built APK's Activity, launched by AOSP's own ActivityThread, with no window.
docker compose run --rm -T dev bash /work/runtime/framework/app_smoke.sh
# The same Activity resumed and drawn by AOSP's ViewRootImpl into a Wayland window.
docker compose run --rm -T dev bash /work/runtime/framework/window_smoke.sh
# An NDK-built library using liblog, libandroid, libjnigraphics, EGL, GLES and OpenSL ES.
docker compose run --rm -T dev bash /work/runtime/framework/ndk_smoke.sh
# RustDesk 1.5.1's x86_64 build (Flutter) to its first frame. Its code is untrusted: sandbox only.
docker compose run --rm dev cargo build --release -p uxdroid-system -p uxdroid-nativebridge -p uxdroid-cli
UXDROID_SAMPLES=/path/to/apks docker compose run --rm -T sandbox bash /work/runtime/framework/rustdesk_smoke.sh

# Build uxdroid, then run an APK with a Wayland compositor available.
docker compose run --rm dev cargo build --release -p uxdroid-cli -p uxdroid-nativebridge -p uxdroid-platform
uxdroid run app.apk --runtime /runtime/art
```

`libuxdroid_nativebridge.so` and `libuxdroid_platform.so` must sit next to the `uxdroid` program. Those Rust libraries need the glibc they were built against, so today this runs in the `dev` image.

| Option | Meaning |
|--------|---------|
| `--runtime <dir>` | The packaged runtime. Otherwise `UXDROID_RUNTIME`, then `$XDG_DATA_HOME/uxdroid/runtime` |
| `--exit-after-first-frame` | Exit once a frame is shown, for tests |

The first run of an APK compiles its DEX and extracts its native libraries into `$XDG_CACHE_HOME/uxdroid/<package>/<id>`. The id comes from the APK's archive directory and the runtime, so a changed APK installs fresh and an older one does not run a newer one's code. Only a crafted APK that copies another's CRC values could match it. A version in use keeps its files until it exits. The compiled runtime must stay at the path where it was compiled; a copy elsewhere works, but without compiled code. The app's current directory is `$XDG_CACHE_HOME/uxdroid/<package>/data`. APKs with native code only for another CPU are refused, because translation does not exist yet.

Exit codes: `1` it cannot start, `2` wrong usage. Once started, the process is the app and exits with the app's status.

### Install for the framework launcher

`dev.uxdroid.system.Launcher`, which starts an APK on AOSP's framework, reads app files where Android's installer leaves them. `uxdroid install` does the installer's part before the first start: it extracts the APK's native libraries for this CPU into `<data>/data/<package>/lib`, the directory `ApplicationInfo.nativeLibraryDir` names, and records the chosen ABI in `<data>/data/<package>/abi`. `<data>` is the absolute directory the launcher gets as `ANDROID_DATA`. Installing again replaces the libraries, and APKs with native code only for another CPU are refused, as are archives that name an entry twice, which Android refuses too. Installs of one package into one data directory take turns, through a lock on `<data>/app/.<package>.lock`, and clear what an interrupted install left.

```sh
UXDROID_RUNTIME=/runtime/art uxdroid install app.apk --data /path/to/android-data
```

With the framework's runtime, found as for `uxdroid run` through `UXDROID_RUNTIME`, then `$XDG_DATA_HOME/uxdroid/runtime`, the install also prepares everything a start would otherwise redo, in the app's code directory `<data>/app/<package>`, as Android's installer does:

| File | What a start does with it instead of the work |
|------|-----------------------------------------------|
| `base.apk` | A copy of the APK, which the launcher is given: `Launcher <runtime> <libuxdroid_system.so> <data>/app/<package>/base.apk` |
| `oat/x86_64/base.odex`, `base.vdex` | The DEX compiled by `dex2oat` with the `verify` filter, against the boot image and the framework's class loader, which ART maps instead of verifying the code at each start |
| `package.record` | The framework's own `PackageParser` result: `ApplicationInfo`, the launcher `ActivityInfo` and `PackageInfo`, read back instead of parsing the manifest |
| `defaults.so` | The default bodies of every framework native, bound by ART by name on first call, instead of a stub per native made at each start |

The report then has a second line:

```
installed com.example.app: 2 native libraries for x86_64 in /data/data/com.example.app/lib
prepared com.example.app in /data/app/com.example.app: code compiled (verify), manifest recorded with launcher com.example.app.MainActivity, 5207 default bodies
```

The `verify` filter was chosen by measurement, in [benches/framework](benches/framework/README.md). The record and the library are valid only for the runtime and the `base.apk` they were made with: if either changes, the launcher says so on stderr and parses the APK and makes the default bodies itself, and ART ignores compiled code that no longer matches. On a file system mounted `noexec`, `defaults.so` cannot be mapped and the bodies are made at start. If preparing fails, the install fails with `error: cannot prepare <package> for the framework: ...` and the previous install stays. Without a framework, only the libraries are installed, an older preparation is removed, and stderr says `note: no framework in <runtime>, so <package> is not prepared: ...`; the launcher then works from the APK as before. The install runs the framework's parser on the manifest, not the app's code.

## Tests

`cargo test` covers parsers, the loader and command errors. It needs the NDK but not ART, so it never runs an app. These scripts do, with the runtime built:

| Script, in `runtime/art` | Proves |
|--------------------------|--------|
| `app_smoke.sh` | An APK paints a Wayland window, read back as pixels |
| `jni_smoke.sh` | DEX reaches a bionic library over JNI |
| `cache_smoke.sh` | Each APK runs its own code, and failed installs stay failed |
| `window_events_smoke.sh` | Close and resize work, including during a frame |
| `smoke.sh` | ART runs compiled code in a clean system |

Run the first four with `docker compose run --rm -T dev bash /work/runtime/art/<script>`, and `smoke.sh` in the `art-runtime` service. There is no CI yet.

## Performance

The performance rule in [AGENTS.md](AGENTS.md) applies to every milestone. [benches/loader](benches/loader/README.md) compares load times with other loaders, and [benches/art](benches/art/README.md) compares runtime startup. [benches/framework](benches/framework/README.md) measures startup and memory on AOSP's framework, RustDesk included, against milestone 3's own platform; no competitor could run those workloads here.
