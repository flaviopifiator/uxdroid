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

2026-11-02, 50 runs each, measured twice on one idle x86_64 machine with 12 cores. Median time to first output, and the highest peak memory across runs. Both runtimes get the same options. Peak memory for `Compute` varies by a few MiB between sets of runs, so differences that small mean little.

| Program | `uxdroid` | `uxdroid`, no JIT | `art_standalone` | `art_standalone`, no JIT |
|---------|-----------|-------------------|------------------|--------------------------|
| `Hello` | 21.5 ms (33.5 MiB) | 21.4 ms (33.4 MiB) | 19.1 ms (34.6 MiB) | **16.3 ms (32.4 MiB)** |
| `Compute` | 46.4 ms (43.7 MiB) | 46.2 ms (44.2 MiB) | 42.6 ms (38.6 MiB) | **39.6 ms (37.8 MiB)** |
| Installed | 172 MiB | | 115 MiB, including its 68 MiB generated boot image | |

**`art_standalone` still wins this comparison.** Against its best setting, no JIT, uxdroid takes 31% more time on `Hello` and 17% more on `Compute`, with 3% more peak memory on `Hello` and about 17% more on `Compute`. With the JIT on, which is how `uxdroid run` works, uxdroid's `Hello` uses 3% less peak memory and takes 13% more time. Its installation is larger.

`uxdroid run` shows its first frame in a median of 25.3 ms, from `first_frame.sh`. There is no competitor figure for that: Android Translation Layer's own launcher was not built.

### Where the difference is

From `/proc/self/smaps` on a small probe, without JIT, reproduced independently:

| Mapping | uxdroid | `art_standalone` |
|---------|---------|------------------|
| `libart.so` resident | 12.2 MiB | 6.1 MiB |
| `boot.art` dirty | 1.3 MiB | 0.1 MiB |

Heap and total private dirty memory also favor the competitor, by amounts that vary between probes. The next memory work is a smaller `libart.so`, and grouping the boot image's written objects with `--dirty-image-objects`.

### What already helped

| Change | Effect, measured from the same storage |
|--------|----------------------------------------|
| `-Xnorelocate` | `Hello` 26.8 to 20.2 ms; private dirty 19.9 to 11.7 MiB |
| Heap limits | `Compute` peak 72 to about 50 MB |
| No large object space, heap growth of 1 | `Compute` 73 to 46 ms. The competitor gains from it too, 73 to 39 ms |
| Boot image from a profile | Image 33 to 9.7 MiB; peak about 12% less on `Hello` and 7 to 12% on `Compute`, at equal time |

The profile comes from one small program. A real app uses other library code, which then runs through the JIT, and that cost has not been measured.

## Limits

- The time to first output leaves out shutdown. `art_standalone` aborts at exit in this setup with `JavaVM::GetEnv() failed`, and it was not investigated whether that comes from the project or this environment.
- An earlier version of this benchmark leaked `ANDROID_ROOT` to the competitor, which disabled its compiled code and produced a false 4.3 times win. An independent verifier caught it.
- It needed changes to build here: wolfSSL options renamed, `libpng-dev` added, and two small C fixes in `fake_log_device.c`. It also needs a passwd entry and `dex2oat` on `PATH`.
- Its compiler settings stayed at their defaults, so a tuned build could be faster. Its first run also compiles its boot image, which took about 3 s and is not counted. uxdroid's boot image is compiled ahead of time by `compile.sh`.
- Not re-verified from scratch: `sync.sh` from an empty volume, which downloads 15.7 GB.
- Docker does not explain the results. With identical files on the same storage, `Hello` took 25.0 ms in the container and 32.6 ms directly on the host, with peak memory within 1%. An empty process cost the same on both, and the host's environment variables, timezone and locale made no difference. The cause of the extra host time was not found; its glibc differs from the container's. Absolute times on another system will differ.
- Peak memory depends on storage: the same image read about 3 MB lower from `tmpfs` than from disk. An earlier claim of beating the competitor on memory came from comparing across them, and was wrong.
- These are two small programs. They say nothing about real apps, which do not run yet.
- Waydroid and the Android emulator are not measured: they need kernel modules or a VM.
- The competitor stays in a Docker volume, because its license is undetermined.
