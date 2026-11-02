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

2026-11-02, 50 runs each, on one x86_64 machine with 12 cores. Median time to first output, and the highest peak memory across runs. Peak memory for `Compute` varies by about 5 MiB between sets of runs.

| Program | `uxdroid` | `uxdroid`, no JIT | `art_standalone` | `art_standalone`, no JIT |
|---------|-----------|-------------------|------------------|--------------------------|
| `Hello` | 21.62 ms (35.3 MiB) | 20.96 ms (35.2 MiB) | 21.24 ms (35.3 MiB) | **17.76 ms (33.2 MiB)** |
| `Compute` | 73.18 ms (43.5 MiB) | **73.02 ms** (41.6 MiB) | 76.94 ms (41.6 MiB) | 74.49 ms (**38.2 MiB**) |
| Installed | 194 MiB | | 115 MiB, including its 68 MiB generated boot image | |

uxdroid is level with `art_standalone` when both keep their default JIT setting. **The best competitor setting still wins** by 15% on `Hello` startup, 6 to 8% on peak memory, and has a smaller installation. Private dirty memory on a small probe was 11.7 MiB for uxdroid and 10.6 MiB for the competitor.

The next planned improvement is a profile-guided boot image built from a real app in milestone 3, which should shrink both the image and the memory it touches.

## Limits

- The time to first output leaves out shutdown. `art_standalone` aborts at exit in this setup with `JavaVM::GetEnv() failed`, and it was not investigated whether that comes from the project or this environment.
- An earlier version of this benchmark leaked `ANDROID_ROOT` to the competitor, which disabled its compiled code and produced a false 4.3 times win. An independent verifier caught it.
- It needed changes to build here: wolfSSL options renamed, `libpng-dev` added, and two small C fixes in `fake_log_device.c`. It also needs a passwd entry and `dex2oat` on `PATH`.
- Its compiler settings stayed at their defaults, so a tuned build could be faster. Its first run also compiles its boot image, which took about 3 s and is not counted. uxdroid's boot image is compiled ahead of time by `compile.sh`.
- Not re-verified from scratch: `sync.sh` from an empty volume, which downloads 15.7 GB.
- These are two small programs. They say nothing about real apps, which need milestone 3.
- Waydroid and the Android emulator are not measured: they need kernel modules or a VM.
- The competitor stays in a Docker volume, because its license is undetermined.
