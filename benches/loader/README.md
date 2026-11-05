# Loader benchmark

Measures how long each loader takes to load a real library and exit, in a fresh process with a warm page cache.

## Run it

```sh
docker compose build dev
docker compose run --rm dev cargo build --release -p uxdroid-cli
docker compose --profile bench build bench

# Extract the x86_64 libraries of RustDesk 1.5.1 outside the repository.
mkdir -p "$SAMPLES/rustdesk"
unzip -oj rustdesk-1.5.1-universal-signed.apk 'lib/x86_64/*.so' -d "$SAMPLES/rustdesk"

UXDROID_SAMPLES="$SAMPLES" docker compose --profile bench run --rm bench sh /work/benches/loader/run.sh
```

## What is compared

| Label | Loader |
|-------|--------|
| `uxdroid` | `uxdroid load`, release build |
| `bionic_translation-no-logs` | The loader used by Android Translation Layer, with debug logging compiled out |
| `bionic_translation` | The same loader as its build ships, with `LINKER_DEBUG=1` |
| `glibc-ld.so` | The system linker, only for `libapp.so`, the one library with no imports |

`bionic_translation` is built from a pinned commit in `Dockerfile` and is never copied into uxdroid.

## Last result

2026-11-02, RustDesk 1.5.1 x86_64 libraries, 50 runs each, in one Docker container on one idle x86_64 machine. Median wall time, with peak memory in parentheses.

| Library | `uxdroid` | `bionic_translation-no-logs` | `bionic_translation` | `glibc-ld.so` |
|---------|-----------|------------------------------|----------------------|---------------|
| `libapp.so`, 12 MiB | 0.83 ms (2.5 MiB) | 10.70 ms (14.6 MiB) | 10.74 ms (14.8 MiB) | 0.54 ms (1.2 MiB) |
| `libc++_shared.so`, 1.5 MiB | 1.44 ms (3.8 MiB) | 5.08 ms (5.8 MiB) | 22.22 ms (5.8 MiB) | Cannot load it |
| `libflutter.so`, 11 MiB | 1.97 ms (5.9 MiB) | 15.69 ms (17.7 MiB) | 71.83 ms (17.9 MiB) | Cannot load it |
| `librustdesk.so`, 40 MiB | 4.30 ms (10.4 MiB) | 47.17 ms (52.3 MiB) | 279.52 ms (52.3 MiB) | Cannot load it |

uxdroid was 3.5 to 13 times faster than `bionic_translation-no-logs`, and used less memory, on these libraries. The system linker was still 0.29 ms faster on the one library it can load.

The audited symbol list and the UTF-8 startup made uxdroid about 10% slower than the first measurement, which had no such protection (0.75, 1.29, 1.84 and 3.96 ms). That was accepted as the cost of not corrupting memory silently.

## Limits

- Platform libraries are stubs, the same ones for every loader, until milestone 3. Constructors run, and no app function is called.
- Load time is one workload. It says nothing about app startup or graphics.
- Waydroid and the Android emulator are not measured: they need kernel modules or a VM, and they boot a whole system.
