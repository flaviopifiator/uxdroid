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

2026-11-04, AMD Ryzen 5 5600X, 12 threads, in one Docker container on a machine shared with other builds (load average about 1.4), with the shared code cache. Medians of 5 runs.

| Configuration (64 MiB) | SHA-256 ms | deflate ms | Kernel ms | % of native speed | First-run extra ms | Peak RSS MiB | Private dirty MiB |
|---|---|---|---|---|---|---|---|
| native x86_64 | 205 | 2206 | 2411 | 100.0% | 3.1 | 78.5 | 76.0 |
| translated (uxdroid, Dynarmic) | 559 | 5385 | 5944 | 40.6% | 47.5 | 87.7 | 82.5 |
| QEMU user | 616 | 6051 | 6666 | 36.2% | 32.9 | 86.9 | 80.9 |

Every configuration produced SHA-256 `91ad201e806745412d7703b38a3f9311ccf1e7599d0c954f844e69da18f871ec` and 12057555 compressed bytes.

The translator ran this kernel 12.2% faster than QEMU user and at 40.6% of native speed; before the shared cache, the same day, it ran at 40.2% (QEMU 35.9%). It used 9.2 MiB more peak memory than native and 0.8 MiB more than QEMU. First-run extra is within the noise of one run for both translators.

## RustDesk to its first frame

`rustdesk_start.sh [runs]` starts RustDesk 1.5.1 to its first Flutter frame three times per round: from its x86_64 build (native), from its arm64-v8a build alone without a translation profile, as on a first start, and from the arm64-v8a build with the profile an earlier start saved, each from a fresh copy of one install. That earlier start runs once before the rounds, until it saves its profile. A run reads, when Flutter posts its first frame: the time since the VM started, `Private_Dirty` of the process, its user plus system CPU time, and the resident part of its anonymous `rwx` mappings, which hold Dynarmic's translated code. RustDesk is untrusted, so it runs only in a sandbox with the engine, mounted as `runtime/framework/rustdesk_arm64_smoke.sh` describes, with both APKs in `/samples`.

2026-11-04, same machine, load average about 1.4, medians of 5 alternating rounds:

| Build | First frame ms | Private dirty MiB | CPU ms | Anonymous rwx MiB | rwx mappings |
|---|---|---|---|---|---|
| native x86_64 | 265 | 149.2 | 320 | 0.0 | 1 |
| translated arm64-v8a, no profile | 2157 | 354.4 | 3090 | 33.7 | 3 |
| translated arm64-v8a, with profile | 1649 | 334.4 | 3290 | 36.3 | 3 |

Before the shared code cache, on the same machine that day: native 265 ms and 149.2 MiB, translated 2447 ms, 600.2 MiB, 4950 ms of CPU and 188.2 MiB of translated code in 41 caches, one per guest thread and nesting level. Sharing one cache cut private dirty memory by 246 MiB, translated code by 154 MiB, CPU time by 1.9 s and the time to the first frame by 290 ms; the profile cut 508 ms more and 20 MiB. The translated build still takes 6.2 times as long as the native one and 185 MiB more memory.

What the engine's settings changed, medians of 3 rounds each:

| Setting | First frame ms | Private dirty MiB | CPU ms | rwx MiB |
|---|---|---|---|---|
| a cache per guest CPU, as before (`UXDROID_SHARED_CODE=0`) | 2439 | 533.8 | 4470 | 165.6 |
| one emitter per cache (`UXDROID_CODE_EMITTERS=1`) | 2680 | 342.1 | 3400 | 35.1 |
| two emitters, the default | 2179 | 356.6 | 3120 | 37.8 |
| four emitters | 2145 | 384.4 | 3100 | 45.3 |
| eight emitters, 16 MiB each, which fill and are replaced | 2896 | 499.7 | 4480 | 133.8 |
| profile translated ahead by one thread, the default | 1641 | 334.1 | 3270 | 36.1 |
| profile translated ahead by two threads | 1559 | 362.7 | 3650 | 35.4 |

The emitter and per-CPU rows were taken before the constant pool stopped being cleared, which saves 2 MiB per emitter. An instrumented build of the shared cache with one emitter counted, up to the first frame, about 150,000 blocks translated, 0.7 s of CPU decoding guest code into IR and 1.8 s emitting it under the cache's lock, so translation is most of the CPU time; a cache per guest CPU translated 193,000 blocks, so most of the old memory was per-cache overhead rather than code translated twice. Installing took 403 ms for arm64-v8a, translation plan included, and 355 ms for x86_64.

## Game kernel

`games.sh` runs `games.c`, the work a game's native code does every frame, natively, translated and under QEMU user:

```sh
docker compose run --rm translation benches/translation/games.sh 5   # runs
```

| Phase | What it does |
|-------|--------------|
| particles | Float physics over 4,096 particles for 20,000 steps, vectorized by the compiler (NEON on arm64) |
| matrices | 20,000 rounds of a chain of 256 4x4 float matrix products, as a skeleton's bones |
| host_calls | 2,000,000 rounds of `strlen` and `sinf`, both host libc functions: 4,000,000 guest-to-host calls |
| tick | 2,000,000 host calls of one tiny exported guest function, as Java calls a game's JNI methods |

Floating point is built with `-ffp-contract=off`, so no CPU fuses a multiply and an add, and `games_report.py` checks that every configuration computes the same checksum for every phase. QEMU user runs only particles and matrices: its guest libc is guest code too, so it has no host crossing to compare.

2026-11-07, AMD Ryzen 5 5600X, same shared machine, medians of 5 runs. *Before* is `main` at f6d6cce with exact floating point (`UXDROID_FAST_FP=0`), *after* is this branch's defaults:

| Configuration | particles ms | matrices ms | host_calls ms | tick ms |
|---|---|---|---|---|
| native x86_64 | 33-43 | 49-50 | 11 | 4 |
| translated, before | 146 | 102 | 322 | 215 |
| translated, after | 138 | 69 | 148 | 119 |
| QEMU user | 3694-4014 | 2173-2340 | - | - |

Matrices run 1.48 times as fast, from floating point on the host's fast path; guest-to-host calls 2.2 times and host-to-guest calls 1.8 times, from argument layouts worked out once and no heap work per call. Particles did not change beyond noise. A crossing still costs about 35 ns guest to host and 60 ns host to guest, against about 1 ns for a native call. QEMU user's floating point is emulated in software, about 30 times slower than the translator here.

## Not measured

- Berberis and its Digitalis fork, which build only inside an AOSP tree for the emulator.
- Houdini and other closed translators, which cannot run here.
- Workloads other than the two kernels: many threads, real games' frame times, and an app's start under QEMU user, which cannot run the bridge's libraries.
- A GPU or another CPU; one machine, shared, so differences of a few percent are noise.
- How the CPU time of RustDesk's start splits between translating and running translated code, in the shipped engine: only the instrumented build above counted it, once.
- RustDesk's start with a profile saved later than 3 s, or kept over many starts, and apps other than RustDesk.
- RustDesk's translated start on the committed tables, which stops before its first frame.
