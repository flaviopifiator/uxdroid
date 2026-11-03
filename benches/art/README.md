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

2026-11-03, 50 runs each, three rounds on one idle x86_64 machine with 12 cores; the table shows the middle round. Median time to first output, and the highest peak memory across runs. Both runtimes get the same options, and `run.sh` evicts both from the page cache first. Peak memory for `Compute` varies by a few MiB between sets of runs, so differences that small mean little.

| Program | `uxdroid` | `uxdroid`, no JIT | `art_standalone` | `art_standalone`, no JIT | `art_standalone`, no JIT, `ulimit -s 256` |
|---------|-----------|-------------------|------------------|--------------------------|-------------------------------------------|
| `Hello` | 12.1 ms (26.6 MiB) | **11.9 ms (26.4 MiB)** | 18.9 ms (32.0 MiB) | 15.8 ms (30.1 MiB) | 10.8 ms (30.1 MiB) |
| `Compute` | 36.9 ms (34.9 MiB) | 36.6 ms (34.0 MiB) | 42.2 ms (38.2 MiB) | 39.5 ms (35.4 MiB) | 33.6 ms (34.7 MiB) |
| Installed | 147 MiB | | 115 MiB, including its 68 MiB generated boot image | | |

**uxdroid now wins with the same options**: 25% less time and 12% less peak memory on `Hello`, and 7% less time on `Compute`, whose memory is level within its noise. **The competitor can still win on time with a smaller stack limit**: its launcher has the same stack cost that uxdroid removed, and with a 256 KiB main thread it starts 10% faster than uxdroid, using 14% more memory on `Hello`. Its installation is smaller.

`uxdroid run` shows its first frame in a median of 15.3 ms, from `first_frame.sh`, down from 24.4 ms. There is no competitor figure for that: Android Translation Layer's own launcher was not built.

### Where the time went

On a 21 ms start, about 7 ms was page faults. ART protects the low end of each thread's stack. The process's first thread has a stack the kernel grows on demand, so ART touches all 8 MiB of it, faulting twice per page, and then returns it. `perf` counted 6011 faults per run, and 1983 without this.

`patches/art-0001-dalvikvm-main-thread-stack.patch` runs the runtime on a thread with a mapped 8 MiB stack. It needs care: if the process already has a second thread, each `membarrier` registration waits for an RCU grace period of several milliseconds, which at first cancelled the whole gain. The launcher therefore registers before it creates the thread.

### Memory

From `/proc/self/smaps` on a small probe, without JIT. Most of the resident size of a library is shared file pages, which `ru_maxrss` counts.

| Mapping | Before | Now | `art_standalone` |
|---------|--------|-----|------------------|
| `libart.so` file | 17.9 MB | 13.0 MB | 9.1 MB, plus 4.3 MB of separate compiler |
| `libart.so` resident | 8.4 MiB | 7.1 MiB | 6.2 MiB |
| `boot.art` dirty | 1.3 MiB | 1.3 MiB | 0.2 MiB |

`libart.so` includes the JIT compiler, and it no longer includes the ARM and RISC-V code generators.

### What helped in this round

Each change measured against the previous one, three rounds, same storage, page cache evicted.

| Change | `Hello` | `Compute` | Cost |
|--------|---------|-----------|------|
| Mapped stack in the launcher | 20.8 to 13.6 ms; peak equal, since the runtime reaches its peak later | 45.7 to 38.4 ms | None measured. Stack depth 65378 to 65365 frames |
| `ART_HOST_CODEGEN_ARCHS="x86 x86_64"` | 13.9 to 13.4 ms; 31.4 to 29.9 MiB | Within noise | `dex2oat` can no longer compile for ARM |
| ICU, XML, TLS and crypto libraries out of the boot profile | 13.4 to 12.4 ms; 29.9 to 27.2 MiB | About 1.5 ms less | A program using those libraries takes 7% longer than it otherwise would, and uses 2.4 MiB less |
| All three together | 20.9 to 12.1 ms; 32.1 to 26.9 MiB | 45.5 to 36.8 ms; about 40 to 34 MiB | The same program still ends up faster: 56.9 to 52.6 ms, 44.5 to 39.8 MiB |

### What did not help

| Tried | Result |
|-------|--------|
| `-Os` for ART | 0.5 MiB less, and 2% slower on the library program. Not adopted: the runtime's speed on long-running programs is unmeasured |
| `--single-image` boot image | 1.2 ms and 1 MiB less, but on host `dex2oat` demands `--force-allow-oj-inlines`, and states that binaries built with it must not be distributed. Not adopted |
| `x86_64` as the only code generator | Does not compile: it shares nodes with `x86` |
| `--dirty-image-objects` | Not tried. `ClassLinker::AddImageSpace` writes to every boot image method that has code, to set its code pointer, which accounts for most of the faults there and which that option does not address |

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
- Peak memory also depends on how the page cache holds a file. Identical bytes read 2 to 6 MiB higher when freshly written than when read back, so `run.sh` evicts both runtimes before measuring. Numbers from before this, in the tables above and in the project log, are not directly comparable.
- These are two small programs. They say nothing about real apps, which do not run yet.
- Waydroid and the Android emulator are not measured: they need kernel modules or a VM.
- The competitor stays in a Docker volume, because its license is undetermined.
