# ART benchmark

Compares uxdroid's ART with `art_standalone`, the runtime of Android Translation Layer, in the same container. It measures fresh processes from start to the first line on stdout.

## Run it

```sh
docker compose --profile art run --rm -T art-builder bash /work/runtime/art/sync.sh
docker compose --profile art run --rm -T art-builder bash /work/runtime/art/build.sh
docker compose --profile art run --rm -T art-builder bash /work/runtime/art/package.sh
docker compose --profile art run --rm -T art-builder bash /work/runtime/art/compile.sh
docker compose --profile art run --rm -T art-runtime bash /work/runtime/art/smoke.sh

docker compose --profile art run --rm -T art-competitor-builder bash /work/benches/art/build_competitor.sh
docker compose --profile art run --rm -T art-bench bash /work/benches/art/run.sh
```

## Last result

2026-11-02, 50 runs each, measured twice on one idle x86_64 machine with 12 cores. Median time to first output, and the highest peak memory across runs. Both runtimes get the same `-Xnorelocate` and heap options.

| Program | `uxdroid` | `uxdroid`, no JIT | `art_standalone` | `art_standalone`, no JIT |
|---------|-----------|-------------------|------------------|--------------------------|
| `Hello` | 20.3 ms (37.4 MiB) | 20.5 ms (37.3 MiB) | 19.4 ms (34.0 MiB) | **16.1 ms (31.9 MiB)** |
| `Compute` | 71.4 ms (47.3 MiB) | **71.2 ms** (46.9 MiB) | 75.2 ms (40.5 MiB) | 72.2 ms (**38.1 MiB**) |
| Installed | 172 MiB | | 115 MiB, including its 68 MiB generated boot image | |

**`art_standalone` still wins this comparison**: 27% faster on `Hello` startup, 15 to 19% less peak memory, and a smaller installation. uxdroid is about level on `Compute` time.

### Where the difference is

From `/proc/self/smaps` on a small probe, without JIT:

| Mapping | uxdroid | `art_standalone` |
|---------|---------|------------------|
| `libart.so` resident | 12.2 MiB | 6.1 MiB |
| `boot.art` dirty | 1.3 MiB | 0.1 MiB |
| Heap | 5.6 MiB | 4.7 MiB |
| All private dirty | 9.9 MiB | 6.6 MiB |

The next memory work is a smaller `libart.so`, and grouping the boot image's written objects with `--dirty-image-objects`.

### What already helped

| Change | Effect, measured from the same storage |
|--------|----------------------------------------|
| `-Xnorelocate` | `Hello` 26.8 to 20.2 ms; private dirty 19.9 to 11.7 MiB |
| Heap limits | `Compute` peak 72 to about 50 MB |
| Boot image from a profile | Image 33 to 9.7 MiB; peak 13% less on `Hello` and 12% on `Compute`, at equal time |

The profile comes from one small program. A real app uses other library code, which then runs through the JIT, and that cost has not been measured.

## Limits

- The time to first output leaves out shutdown. `art_standalone` aborts at exit in this setup with `JavaVM::GetEnv() failed`, and it was not investigated whether that comes from the project or this environment.
- An earlier version of this benchmark leaked `ANDROID_ROOT` to the competitor, which disabled its compiled code and produced a false 4.3 times win. An independent verifier caught it.
- It needed changes to build here: wolfSSL options renamed, `libpng-dev` added, and two small C fixes in `fake_log_device.c`. It also needs a passwd entry and `dex2oat` on `PATH`.
- Its compiler settings stayed at their defaults, so a tuned build could be faster. Its first run also compiles its boot image, which took about 3 s and is not counted. uxdroid's boot image is compiled ahead of time by `compile.sh`.
- Not re-verified from scratch: `sync.sh` from an empty volume, which downloads 15.7 GB.
- Peak memory depends on storage: the same image read about 3 MB lower from `tmpfs` than from disk. An earlier claim of beating the competitor on memory came from comparing across them, and was wrong.
- These are two small programs. They say nothing about real apps, which need milestone 3.
- Waydroid and the Android emulator are not measured: they need kernel modules or a VM.
- The competitor stays in a Docker volume, because its license is undetermined.
