# uxdroid

Run Android APKs as native Linux processes, with no emulator, no container and no Android system image.

**Status: milestone 3.** `uxdroid run` opens the launcher Activity of a simple test APK in a Wayland window, joining its DEX and its bionic libraries in one process. Real apps do not run yet: they need far more of the `android.*` API than the few classes that exist. See [the architecture](docs/architecture.md) for the design and roadmap.

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

Needs the runtime built under `runtime/art`, and a Wayland compositor. Only an x86_64 glibc host is supported.

```bash
uxdroid run app.apk --runtime /path/to/runtime/art
```

| Option | Meaning |
|--------|---------|
| `--runtime <dir>` | Where the runtime is. Otherwise `UXDROID_RUNTIME`, then `$XDG_DATA_HOME/uxdroid/runtime` |
| `--exit-after-first-frame` | Exit once a frame is shown, for tests |

On first run the APK's DEX compiles to machine code and its native libraries extract under `$XDG_CACHE_HOME/uxdroid/<package>`. APKs with native code only for another CPU are refused, because translation does not exist yet. Exit codes: `1` it cannot start, `2` wrong usage. Once the app starts, the process is the app.

## Performance

The performance rule in [AGENTS.md](AGENTS.md) applies to every milestone. [benches/loader](benches/loader/README.md) compares load times with other loaders, and [benches/art](benches/art/README.md) compares runtime startup.
