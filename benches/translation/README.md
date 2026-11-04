# Translation benchmark

Measures how fast arm64 code runs through the bridge's translator, against the same code built for x86_64 and against QEMU user, the competitor that can run here.

## Run it

```sh
docker compose build dev
docker compose build translation
docker compose run --rm translation benches/translation/run.sh 64 5   # MiB of input, runs
```

## What is compared

The kernel is `kernel.c`: SHA-256 (FIPS 180-4) and zlib 1.3.2 deflate at level 6 over a buffer of deterministic, compressible text. It is freestanding, with its own `memcpy` and kin and zlib's state in a static arena, so every configuration runs the same C code and no host library call helps one of them. Each build uses the NDK r30 clang at `-O2 -fPIC`.

| Label | What runs |
|-------|-----------|
| native x86_64 | The x86_64 build, a `.so` loaded through the bridge and called directly |
| translated (uxdroid, Dynarmic) | The arm64-v8a build, a `.so` loaded through the bridge and run by its translator: Dynarmic's A64 JIT with guest memory identity-mapped (`crates/translate`) |
| QEMU user | The same arm64 code linked into a static program with `qemu_main.c`, under `qemu-aarch64` 10.0.13 (Debian 1:10.0.13+ds-0+deb13u1) |

Each phase runs twice in one process. Speed is the second run, so it excludes translation; *first-run extra* is the first run's extra time, which estimates translation time and includes noise (the native row, which translates nothing, shows its size). Peak RSS is `VmHWM` and private dirty memory is `Private_Dirty` of `smaps_rollup`, both read by the process itself after the kernel, so they include the 64 MiB input and the touched part of the output buffer. `report.py` checks that every run of every configuration produced the same digest and compressed size.

QEMU runs in its best configuration for this workload as far as tested: user mode, its default TCG settings, a static binary so no dynamic linking is emulated. It could not run the bridge's `.so` itself, since that needs bionic's loader for arm64, which is not in this environment.

## Last result

2026-11-03, AMD Ryzen 5 5600X, 12 threads, in one Docker container on a machine shared with other builds (load average about 3). Medians of 5 runs.

| Configuration (64 MiB) | SHA-256 ms | deflate ms | Kernel ms | % of native speed | First-run extra ms | Peak RSS MiB | Private dirty MiB |
|---|---|---|---|---|---|---|---|
| native x86_64 | 215 | 2239 | 2454 | 100.0% | 8.2 | 78.3 | 75.9 |
| translated (uxdroid, Dynarmic) | 577 | 5498 | 6075 | 40.4% | 27.7 | 88.2 | 83.2 |
| QEMU user | 625 | 6162 | 6787 | 36.2% | 20.7 | 86.9 | 80.9 |

Every configuration produced SHA-256 `91ad201e806745412d7703b38a3f9311ccf1e7599d0c954f844e69da18f871ec` and 12057555 compressed bytes.

The translator ran this kernel 11.7% faster than QEMU user and at 40.4% of native speed. It used 9.9 MiB more peak memory than native and 1.3 MiB more than QEMU; of that, about 4 MiB is translated code in Dynarmic's code cache and 1 MiB is its dispatch table, which `docker/translation/dynarmic.patch` shrinks from 16 MiB. Translation time here is within the noise of one run (tens of milliseconds for both translators).

## RustDesk to its first frame

`rustdesk_start.sh [runs]` starts RustDesk 1.5.1 to its first Flutter frame twice per run, from its x86_64 build (native) and from its arm64-v8a build alone (translated), each from a fresh copy of one install, and reads, when Flutter posts its first frame: the time since the VM started, `Private_Dirty` of the process, its user plus system CPU time, and the resident part of its anonymous `rwx` mappings, which hold Dynarmic's translated code, one per guest CPU. RustDesk is untrusted, so it runs only in a sandbox with the engine, mounted as `runtime/framework/rustdesk_arm64_smoke.sh` describes, with both APKs in `/samples`.

2026-11-04, same machine, load average about 1, medians of 5 alternating runs. The arm64 build reached its first frame only with nine GL extension functions added to `crates/ndk`'s GLES table, a change not committed (see docs/architecture.md, "Not yet"); without them it stops at Skia's GL setup.

| Build | First frame ms | Private dirty MiB | CPU ms | Anonymous rwx MiB | rwx mappings |
|---|---|---|---|---|---|
| native x86_64 | 254 | 149.0 | 320 | 0.0 | 1 |
| translated arm64-v8a | 2426 | 603.9 | 4930 | 188.5 | 41 |

The translated build took 9.6 times as long to its first frame and 455 MiB more private dirty memory, and spent 4.6 s more CPU time, which is translation and translated execution together. About 190 MiB of that memory is translated code, 4.5 MiB in each of 41 code caches, and anonymous writable memory grew by 282 MiB, which these runs do not attribute further. Every guest thread and nesting level has its own Dynarmic JIT with its own code cache and 1 MiB dispatch table, so code one thread translated is translated again on the next; that is the likely main part. Installing took 398 ms for arm64-v8a, translation plan included, and 353 ms for x86_64.

## Not measured

- Berberis and its Digitalis fork, which build only inside an AOSP tree for the emulator.
- Houdini and other closed translators, which cannot run here.
- Workloads other than this integer kernel: floating point and SIMD heavy code, code that calls the host often (each host call moves registers by signature), many threads, and an app's start under QEMU user, which cannot run the bridge's libraries.
- A GPU or another CPU; one machine, shared, so differences of a few percent are noise.
- How the CPU time of RustDesk's start splits between translating and running translated code: the engine has no counters for it yet.
- RustDesk's translated start on the committed tables, which stops before its first frame.
