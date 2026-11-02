# ART benchmark

Compares uxdroid's ART with `art_standalone`, the runtime of Android Translation Layer, in the same container. It measures fresh processes from start to the first line on stdout.

## Run it

```sh
docker compose --profile art run --rm -T art-builder bash /work/runtime/art/build.sh
docker compose --profile art run --rm -T art-builder bash /work/runtime/art/package.sh
docker compose --profile art run --rm -T art-builder bash /work/runtime/art/compile.sh

docker run --rm --memory 22g --memory-swap 22g --cpus 10 \
  -v uxdroid-art_art_competitor:/competitor -v "$PWD/benches/art:/script:ro" \
  debian:trixie-slim bash /script/build_competitor.sh

docker compose --profile art run --rm -T art-bench bash /work/benches/art/run.sh
```

## Last result

2026-11-02, 30 runs each, on one idle x86_64 machine with 12 cores. Median time, with peak memory in parentheses.

| Program | uxdroid | `art_standalone` |
|---------|---------|------------------|
| `Hello` | 28.18 ms (37.8 MiB) | 122.17 ms (34.4 MiB) |
| `Compute` | 76.89 ms (64.9 MiB) | 194.24 ms (42.1 MiB) |
| Installed size | 227 MiB | 424 MiB |

uxdroid was 4.3 and 2.5 times faster and half the size. **It used more memory: 10% on `Hello` and 54% on `Compute`.** Reducing that is open work.

## Limits

- The time to first output leaves out shutdown. `art_standalone` aborts at exit in this setup with `JavaVM::GetEnv() failed`, and it was not investigated whether that comes from the project or this environment.
- It needed changes to build here: wolfSSL options renamed, `libpng-dev` added, and two small C fixes in `fake_log_device.c`. It also needs a passwd entry and `dex2oat` on `PATH`.
- Its compiler settings stayed at their defaults, so a tuned build could be faster. Its first run also compiles its boot image, which took about 3 s and is not counted. uxdroid's boot image is compiled ahead of time by `compile.sh`.
- These are two small programs. They say nothing about real apps, which need milestone 3.
- Waydroid and the Android emulator are not measured: they need kernel modules or a VM.
- The competitor stays in a Docker volume, because its license is undetermined.
