# ART benchmark

Compares uxdroid's ART with `art_standalone`, the runtime of Android Translation Layer, in the same container. It measures fresh processes from start to the first line on stdout.

## Run it

```sh
docker compose --profile art run --rm -T art-builder bash /work/runtime/art/sync.sh
docker compose --profile art run --rm -T art-builder bash /work/runtime/art/build.sh
docker compose --profile art run --rm -T art-builder bash /work/runtime/art/package.sh
docker compose --profile art run --rm -T art-builder bash /work/runtime/art/compile.sh
docker compose --profile art run --rm -T art-builder bash /work/runtime/art/build_platform.sh
docker compose --profile art run --rm -T art-runtime bash /work/runtime/art/smoke.sh

docker compose --profile art run --rm -T art-competitor-builder bash /work/benches/art/build_competitor.sh
docker compose --profile art run --rm -T art-bench bash /work/benches/art/run.sh
docker compose run --rm -T dev bash /work/benches/art/first_frame.sh
```

## Last result

2026-11-03, 30 runs each, measured twice on one idle x86_64 machine with 12 cores, with both runtimes evicted from the page cache first. Median time to first output, and the highest peak memory across runs. Both runtimes get the same options, and every row printed the expected output.

| Program | `uxdroid` | `uxdroid`, no JIT | `art_standalone` | `art_standalone`, no JIT | `art_standalone`, no JIT, `ulimit -s 256` |
|---------|-----------|-------------------|------------------|--------------------------|-------------------------------------------|
| `Hello` | 12.7 ms (27.9 MiB) | 12.3 ms (27.6 MiB) | 19.0 ms (32.1 MiB) | 16.0 ms (30.1 MiB) | 10.6 ms (30.1 MiB) |
| `Compute` | 37.5 ms (36.5 MiB) | 36.9 ms (37.7 MiB) | 42.4 ms (35.6 MiB) | 39.1 ms (33.9 MiB) | 34.1 ms (33.9 MiB) |
| `Workload` | 38.7 ms (46.5 MiB) | 38.7 ms (46.6 MiB) | 90.5 ms (61.8 MiB) | 80.8 ms (60.1 MiB) | 75.5 ms (60.1 MiB) |
| `Holdout` | 31.7 ms (42.7 MiB) | 32.8 ms (41.7 MiB) | 43.0 ms (52.7 MiB) | 37.1 ms (50.6 MiB) | 31.8 ms (50.9 MiB) |
| Installed | 150 MiB | | 115 MiB, including its 68 MiB generated boot image | | |

`Workload` runs library code a real app reaches at startup, and the boot profile comes from it, so its result is flattering. `Holdout` uses library code the profile never saw, and is the fairer test.

**uxdroid wins three programs and loses `Compute` on memory.** With the JIT on, as `uxdroid run` works, it takes 33% less time on `Hello`, 12% on `Compute`, 57% on `Workload` and 26% on `Holdout`. It uses less peak memory on `Hello`, `Workload` and `Holdout`, by 13 to 25%, and 3% more on `Compute`, which varies by a few MiB.

**Where the competitor still wins:**

- With a 256 KiB main thread and no JIT, it starts `Hello` 14% faster than uxdroid without JIT, and is faster on `Compute`. `Holdout` ties. That main thread has about 1/34 of the usual depth.
- Without JIT and at the default stack, it uses 10% less memory on `Compute`.
- Its installation is smaller.
- On private dirty memory, `Hello` favors it: 3.3 MiB against 2.2 MiB, without JIT, in an independent measurement. RSS and PSS favor uxdroid.

Most of the win on `Hello` is the stack patch. The competitor could apply the same idea.

`uxdroid run` shows its first frame in a median of 14.9 ms, from `first_frame.sh`, down from 24.4 ms. There is no competitor figure for that, because Android Translation Layer's own launcher was not built.

### What helped, and what it cost

| Change | Effect | Cost |
|--------|--------|------|
| Runtime on a mapped stack | About 7 ms off startup: ART touched the first thread's whole 8 MiB stack | `ulimit -s` no longer raises the main stack; one more thread; `ulimit -v` near 700 MB now hangs where it used to abort, and lower values hang either way |
| `-Xnorelocate` | Private dirty memory 19.9 to 11.7 MiB, and less startup | None measured; the competitor gains too |
| No large object space, heap growth of 1 | `Compute` 73 to 46 ms | Unmeasured on apps with large objects; the competitor gains too |
| Boot image from a realistic profile | Image 33 to 9 MiB; `Holdout` 54.6 to 31.2 ms against the smaller first profile, 42.8 against 53.9 MiB for a full image | A full image is about 6 ms faster on `Holdout`, and uses 11 MiB more |
| `ART_HOST_CODEGEN_ARCHS="x86 x86_64"` | Smaller runtime | `dex2oat` cannot compile for ARM. Milestone 4 translates native libraries and not DEX, so it does not need that |

Boot image dirty pages remain 1.3 MiB against 0.2 MiB for the competitor, and `libart.so` resident 7.1 against 6.2 MiB. `-Os` saved 0.5 MiB at 2% slower speed and was rejected. `--single-image` was rejected because `dex2oat` warns against distributing what it builds.

## Limits

- The time to first output leaves out shutdown. `art_standalone` aborts at exit in this setup with `JavaVM::GetEnv() failed`, and it was not investigated whether that comes from the project or this environment.
- An earlier version of this benchmark leaked `ANDROID_ROOT` to the competitor, which disabled its compiled code and produced a false 4.3 times win. An independent verifier caught it.
- It needed changes to build here: wolfSSL options renamed, `libpng-dev` added, and two small C fixes in `fake_log_device.c`. It also needs a passwd entry and `dex2oat` on `PATH`.
- Its compiler settings stayed at their defaults, so a tuned build could be faster. Its first run also compiles its boot image, which took about 3 s and is not counted. uxdroid's boot image is compiled ahead of time by `compile.sh`.
- Not re-verified from scratch: `sync.sh` from an empty volume, which downloads 15.7 GB.
- Docker does not explain the results. With identical files on the same storage, `Hello` took 25.0 ms in the container and 32.6 ms directly on the host, with peak memory within 1%. An empty process cost the same on both, and the host's environment variables, timezone and locale made no difference. The cause of the extra host time was not found; its glibc differs from the container's. Absolute times on another system will differ.
- Peak memory depends on storage: the same image read about 3 MB lower from `tmpfs` than from disk. An earlier claim of beating the competitor on memory came from comparing across them, and was wrong.
- Peak memory also depends on how the page cache holds a file. Identical bytes read 2 to 6 MiB higher when freshly written than when read back, so `run.sh` evicts both runtimes before measuring. Numbers from before this, in the tables above and in the project log, are not directly comparable.
- These are small programs. They say nothing about real apps, which do not run yet.
- An earlier version of this page claimed a general win from `Hello` and `Compute` alone. A verifier found uxdroid 39% slower on a library-heavy program, because the first profile compiled only 1182 methods. `Workload` and `Holdout` joined the benchmark as a result.
- Waydroid and the Android emulator are not measured: they need kernel modules or a VM.
- The competitor stays in a Docker volume, because its license is undetermined.
