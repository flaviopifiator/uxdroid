# Framework benchmark

Measures startup and memory of AOSP's framework on uxdroid's ART, from disk, with milestone 3's own 8-class platform as the baseline. **No competitor could run these workloads in this environment**, so this page compares uxdroid only with itself. See [Not measured](#not-measured).

## Run it

The runtime and framework must be built first (`runtime/art/` and `runtime/framework/build.sh`).

```sh
docker compose run --rm -T dev bash /work/benches/framework/run.sh

docker compose run --rm dev cargo build --release -p uxdroid-system -p uxdroid-nativebridge -p uxdroid-cli
UXDROID_SAMPLES=<directory with the RustDesk APK> docker compose run --rm -T \
    -v uxdroid-framework-bench:/var/tmp sandbox bash /work/benches/framework/rustdesk.sh
```

`RUNS` sets the measured runs per row (default 30), for example `docker compose run --rm -T -e RUNS=10 dev ...`.

## Method

[`measure.py`](measure.py) starts a fresh process for every run and reads its stdout and stderr. At the first line that matches the workload's marker it stops the process with `SIGSTOP`, reads `VmHWM` from `/proc/<pid>/status` (peak RSS up to that moment) and `/proc/<pid>/smaps_rollup` (PSS, private dirty), and kills it, or lets it finish when the row also reports time to exit. Every run must print the workload's expected lines, or the whole benchmark fails.

- **Cold**: before each run, `posix_fadvise(DONTNEED)` drops the page cache of the runtime volume, our binaries, the APK and the app data, as `benches/art` does with `dd iflag=nocache`. The container's own system libraries (glibc, Mesa, Wayland, libstdc++) are not evicted.
- **Warm**: the same runs without eviction, after one discarded warm-up run.
- Each run waits 0.3 s first. Without that wait, the killed previous process still held its mappings for a moment: the next run's PSS read about half (13.6 instead of 29.8 MiB), and some cold runs found files still cached (39 ms instead of 165 ms).
- Time is wall time from `fork` to the parent reading the marker line.
- All data is on disk. `run.sh` refuses to run when its work directory is on tmpfs. In `sandbox`, `/tmp` is tmpfs, so `rustdesk.sh` copies the APK and installs the app into a Docker volume mounted at `/var/tmp`, and prints the file system of each. The runtime is in the `uxdroid-art_art_runtime` volume, on ext4.

| Workload | Marker (first-frame moment) | Expected output |
|----------|-----------------------------|-----------------|
| `FrameworkSmoke`: framework start, text, resources, a framework `TextView` | `view <w>x<h> <ink>`, after the `TextView` drew; also time to exit | `string OK`, `sdk 36 abi x86_64`, `text <n>` |
| Framework sample APK (`runtime/framework/window_smoke.sh`) | `first frame posted` on Wayland | `activity resumed com.example.framework.MainActivity` |
| RustDesk 1.5.1, `sandbox` only | `uxdroid: activity fully drawn: com.carriez.flutter_hbb/.MainActivity` | `I/flutter:` output, and a frame posted on Flutter's `SurfaceView` |
| Milestone 3 baseline: `uxdroid run` with the first sample (`runtime/art/app_smoke.sh`) | `frame presented` | `activity created in com.example.first` |
| `uxdroid install` RustDesk, into an emptied directory each run | `installed com.carriez.flutter_hbb: 4 native libraries ...`, and exit | |

## Last result

2026-11-03, 30 runs per row, one idle machine, all outputs verified. Median, with the interquartile range in brackets. Memory is at the first-frame moment; its interquartile range was under 2 MiB in every row.

| Workload | Cold time | Warm time | Peak RSS | PSS | Private dirty |
|----------|-----------|-----------|----------|-----|---------------|
| Milestone 3 platform, `uxdroid run` to first frame | 170 ms (166-293) | 17.5 ms (17.4-17.7) | 31.6 MiB | 29.8 MiB | 5.0 MiB |
| `FrameworkSmoke`, to the `TextView` | 658 ms (621-1110) | 72.0 ms (71.6-72.9) | 116.1 MiB | 114.0 MiB | 10.9 MiB |
| `FrameworkSmoke`, to exit | 669 ms (632-1121) | 82.2 ms (81.7-82.9) | | | |
| Framework sample APK to first frame | 728 ms (717-856) | 123.8 ms (123.1-138.8) | 146.8 MiB | 133.5 MiB | 19.5 MiB |
| RustDesk 1.5.1 to its first Flutter frame | 972 ms (963-1552) | 250 ms (246-253) | 308.4 MiB | 297.1 MiB | 87.5 MiB |

Memory is from the warm rows; the cold rows read within 3 MiB of them (RustDesk cold: 311.2, 296.4 and 88.6 MiB).

| Install | Value |
|---------|-------|
| AOSP framework added to the runtime | 214 MB in 251 files (framework DEX, `framework-res.apk`, fonts, ICU, `libandroid_runtime.so`, odex); whole runtime 355 MiB |
| RustDesk APK | 76 MiB |
| RustDesk after `uxdroid install`: 4 extracted `x86_64` libraries | 65 MiB, unchanged after the 61 launches |
| `uxdroid install` RustDesk | cold 235 ms (189-243), warm 192 ms (190-194) |

Environment: AMD Ryzen 5 5600X (6 cores, 12 threads), 30 GiB RAM, Linux 7.0.0-38-generic, Docker 29.9.0, ext4 on a SATA SSD (WD Green 480 GB). Containers: `dev` and `sandbox` from the `uxdroid-dev` image.

### What the real framework costs

Against milestone 3's own platform, to a first frame:

- Warm, the framework's sample APK takes 7.1 times as long (123.8 against 17.5 ms), and cold 4.3 times (728 against 170 ms).
- It uses 4.6 times the peak RSS (146.8 against 31.6 MiB) and 3.9 times the private dirty memory (19.5 against 5.0 MiB). Most of its RSS is clean, shared file pages of the framework; private dirty, the memory that no other process can share, is 19.5 MiB.
- The two apps are not the same: milestone 3's sample paints one color and a text with our platform, while the framework's sample goes through `ActivityThread`, `ViewRootImpl` and a decor view. This shows the price of AOSP's framework, not of equal work.
- Against the spike (L2 in `odd/tasks/real-framework.md`, a similar `TextView` program, not the same one): private dirty fell from 28 to 10.9 MiB, RSS from 137 to 116 MiB, warm time from 205 to 82 ms and cold time from 0.9 s to 0.67 s.

RustDesk, a real Flutter app, reaches its first frame in 250 ms warm and under 1 s cold, with 87.5 MiB private dirty; most of that is the Dart VM, Flutter's engine and the software-composed frame buffers, which this benchmark does not break down.

## Audio

[`audio.sh`](audio.sh) measures what audio costs and how fast it is. It runs in `dev`, needs the runtime built by `build_system.sh` at the path it runs from (a copy under another path loses its boot image), and takes `SOURCE` and `ONLY=framework` to measure an older checkout the same way:

```sh
docker compose run --rm -T dev bash /work/benches/framework/audio.sh
```

2026-11-03, 30 runs per row, from ext4, on the machine above while other builds ran (load average about 2.9). The baseline is the commit before audio (1ae8b4e) with the milestone 4 runtime, measured right before, the same way.

| Workload | Cold time | Warm time | Peak RSS | PSS | Private dirty |
|----------|-----------|-----------|----------|-----|---------------|
| Framework sample APK to first frame, before audio | 1044 ms (837-1363) | 138.4 ms (137.8-139.4) | 146.8 MiB | 133.1 MiB | 19.5 MiB |
| Framework sample APK to first frame, with audio | 899 ms (844-1507) | 139.1 ms (137.8-140.0) | 147.0 MiB | 133.3 MiB | 19.5 MiB |
| Audio sample APK through its AudioTrack tone, `UXDROID_AUDIO=null` | 802 ms (769-1300) | 207.6 ms (204.6-214.0) | 146.3 MiB | 132.1 MiB | 18.7 MiB |

- An app that plays nothing pays nothing measurable: warm time moved 0.7 ms, inside the interquartile range, and memory by 0.2 MiB of RSS. The libraries grew: `libuxdroid_system.so` from 1,039,808 to 1,214,600 bytes and `libuxdroid_nativebridge.so` from 917,000 to 1,136,416, which are clean, shared pages. Cold times are bimodal, as below; the two cold medians differ by less than their spread.
- This baseline's warm time, 138.4 ms, is above milestone 4's 123.8 ms for the same row and code: the machine was busier. Compare rows of one table, not across tables.
- The audio sample's marker comes after its 100 ms tone was written to an `AudioTrack`, played and drained, and the track released; warm, it comes 69 ms later than the framework sample's first frame, though the two apps do different work. Its private dirty memory is lower than the framework sample's, since it draws no content view.

Latency and pacing on a private PulseAudio server with null devices (`crates/audio/tests/pulse.rs` and the audio sample, three runs each, all alike):

| Measure | Value |
|---------|-------|
| Callback period, 48 kHz and 32 kHz stereo | 10.00 ms and 9.98 ms |
| Latency the server reports for a playing stream, 48 kHz (32 kHz, resampled) | 20.4 ms (21.6-21.9 ms) |
| AAudio `requestStart` to the first data callback | 6-13 us |
| A recording's half second | read in 0.5 s, at the server's pace |

The latency an app sees is the server's plus what it buffered: the sample's `AudioTrack`, of four minimum buffers, holds up to 80 ms at 48 kHz, and an AAudio stream up to four periods, 40 ms; a blocking write keeps them full. Null devices idle in 2 s blocks and wake for a new stream only at the end of one, which hardware devices do not do, so the script keeps a silent stream on each; without it the first sound of a stream came about 2 s late. Not measured: latency on PipeWire, PipeWire's own protocol, ALSA on real hardware, and a round trip from speaker to microphone. No competitor plays audio here either.

## Limits

- Cold times are bimodal on this machine: most runs are in the low group (for example 963 ms for RustDesk), and a quarter or more take up to about twice as long. The cause was not found; the disk is an SSD and the eviction is the same each run. Compare medians, and read the brackets.
- Without the 0.3 s settle, warm times are about 2 ms lower for milestone 3 (15.5 ms), which matches `benches/art/first_frame.sh`'s 14.9 ms. Back-to-back launches are slightly faster here; the settle is the price of correct memory figures.
- Peak RSS stops at the marker. `FrameworkSmoke`'s peak to exit is not measured, and `uxdroid install` exits before it can be sampled, so its memory is not reported.
- RustDesk's warm-up run is its first start ever, which creates its preferences and Dart state, and is not counted; the measured runs are second starts. A first start was not measured on its own.
- Child processes are not counted. Only `uxdroid run` was checked: it starts none after its first run, which compiles the app with `dex2oat` and is the discarded warm-up.
- The rendering is software on a headless Weston (pixman, 800x600). A real compositor and GPU would change frame times.
- RustDesk's own timing from L18 (about 380 ms "after the framework started") used a different clock and an earlier build; it is not comparable with this table.

## Not measured

No competitor runs these workloads here, so **no claim of being faster or smaller than any competitor is made**.

| Competitor | Why not | Could it run without new host privileges? |
|------------|---------|-------------------------------------------|
| Android Translation Layer (its launcher and Java framework) | Not built; only its runtime, `art_standalone`, is built in the `uxdroid-art_art_competitor` volume for `benches/art` | Probably yes: build it in a Debian container like `art_standalone`, run it on the same headless Weston. Cost: its build dependencies (GTK 4, libportal, FFmpeg, WebKitGTK and others), an untested build, a volume of a few GB kept out of the repository because its license is undetermined, and a check that it can draw the sample APK at all. Whether it runs RustDesk's Flutter `SurfaceView` is unknown. This is the follow-up option. |
| `art_standalone` alone | It is a runtime without a framework; it cannot run these apps, only the plain DEX programs in `benches/art` | Already measured there |
| Waydroid | Needs the kernel's binder driver (absent: no `binder` in `/proc/filesystems`, no `/dev/binder*`) and a privileged LXC container | No |
| Android emulator | No `/dev/kvm` on this machine; without KVM an x86_64 system image runs under full software emulation, which is no fair comparison | No |
| A real Android device | None available | |

Also not measured: frame rate or time after the first frame, input latency, memory over time, GPU rendering, and RustDesk with its data on tmpfs (only on disk).
