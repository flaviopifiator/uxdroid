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

`RUNS` sets the measured runs per row (default 30), for example `docker compose run --rm -T -e RUNS=10 dev ...`. Both scripts use the release build in `CARGO_TARGET_DIR` (default `/cache/target`).

The runtime must be at `/runtime/art`, the path its boot image and framework record. A copy elsewhere, such as `/runtime/art-<unit>`, runs without its boot image and gives wrong times; mount the copy at `/runtime/art` instead (with `docker run --mount type=volume,src=uxdroid-art_art_runtime,dst=/runtime/art,volume-subpath=<copy>`), and build it there.

Input latency, through the scripted Wayland seat of `runtime/framework/input_compositor.py`:

```sh
docker compose run --rm -T dev bash /work/benches/framework/input.sh
UXDROID_SAMPLES=<directory with the RustDesk APK> docker compose run --rm -T \
    -v uxdroid-framework-bench:/var/tmp -e APP=rustdesk sandbox bash /work/benches/framework/input.sh
```

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
| Framework sample APK (`runtime/framework/window_smoke.sh`), from the APK and installed | `first frame posted` on Wayland | `activity resumed com.example.framework.MainActivity` |
| RustDesk 1.5.1, `sandbox` only, from the APK and installed | `uxdroid: activity fully drawn: com.carriez.flutter_hbb/.MainActivity` | `I/flutter:` output, and a frame posted on Flutter's `SurfaceView` |
| Milestone 3 baseline: `uxdroid run` with the first sample (`runtime/art/app_smoke.sh`) | `frame presented` | `activity created in com.example.first` |
| `uxdroid install` RustDesk, into an emptied directory each run, without a framework and with one | `installed com.carriez.flutter_hbb: 4 native libraries ...`, or `prepared com.carriez.flutter_hbb in ...`, and exit | |

"From the APK" starts the launcher on the APK file, as every start did before `uxdroid install` prepared apps: it parses the manifest, makes a stub for every framework native, and runs the app's DEX uncompiled. "Installed" starts it on `<data>/app/<package>/base.apk` after `uxdroid install` with the framework, which compiled the DEX, recorded the manifest and wrote the default bodies' library.

## Install-time preparation

2026-11-03, 30 runs per row, from ext4, the same method as below, measured in the `art-prep` runtime copy (its boot image, framework and system odex recompiled at that path, so ART runs with its image). Median, interquartile range in brackets. Memory is from the warm rows. The machine was not idle: other builds and tests ran beside these (load average 2 to 6), so cold times are wider than in the table below, and three runs of the same unchanged row, `FrameworkSmoke` warm, read 74.0, 84.6 and 84.2 ms.

| Workload | Cold time | Warm time | Peak RSS | PSS | Private dirty |
|----------|-----------|-----------|----------|-----|---------------|
| Framework sample APK to first frame, from the APK | 1198 ms (1021-1364) | 140.5 ms (137.6-155.4) | 146.7 MiB | 133.4 MiB | 19.5 MiB |
| Framework sample APK to first frame, installed | 799 ms (744-1218) | 123.5 ms (122.2-124.1) | 141.0 MiB | 127.5 MiB | 19.1 MiB |
| RustDesk 1.5.1 to its first Flutter frame, from the APK | 1464 ms (1170-2396) | 274.2 ms (261.8-298.3) | 307.7 MiB | 295.9 MiB | 87.5 MiB |
| RustDesk 1.5.1 to its first Flutter frame, installed | 1152 ms (1074-1250) | 222.2 ms (217.6-224.0) | 301.3 MiB | 289.4 MiB | 80.8 MiB |

| Install of RustDesk | Cold | Warm | On disk |
|---------------------|------|------|---------|
| Without a framework: libraries only | 240 ms (203-265) | 198 ms (196-403) | 65 MiB of libraries |
| With the framework: also `base.apk`, odex, record, `defaults.so` | 2071 ms (1696-6260) | 528 ms (491-543) | 81 MiB more: `base.apk` 75.3 MiB, odex and vdex 4.1 MiB, `defaults.so` 0.6 MiB, `package.record` 4 KiB |

Warm, an installed start of RustDesk is 52 ms (19%) faster to its first Flutter frame than one from the APK, with 6.7 MiB less private dirty memory and 6.4 MiB less peak RSS; the sample is 17 ms (12%) faster, with 0.4 MiB less private dirty and 5.7 MiB less peak RSS. Against milestone 4's table below, measured on the shared runtime on an idle machine, installed RustDesk is 28 ms faster warm (222 against 250 ms) with 6.7 MiB less private dirty, and the installed sample is the same warm (123.5 against 123.8 ms) with 0.4 MiB less private dirty. Cold, both installed starts are faster by median, but the brackets of this loaded run overlap, so no cold claim is made. The price is install time, about 0.3 s more warm, and disk, mostly the copy of the APK, which Android's installer makes too.

What each part removes, measured by timing the launcher's phases (3 warm runs each, sample and RustDesk alike): making a stub for each of the 5,207 natives, which loads every class that has one, about 11.5 ms; the manifest parse, about 10 ms; and for RustDesk, running its 4.2 MB of DEX uncompiled.

The compiler filter for the app's DEX was chosen on RustDesk, warm, 10 runs each, from ext4:

| Filter | dex2oat | Output | Time to first frame | Private dirty |
|--------|---------|--------|---------------------|---------------|
| none (JIT and interpreter) | | | 247.8 ms (246.6-252.3) | 119.2 MiB |
| `verify` | 223 ms | 4.2 MiB | 215.6 ms (213.2-220.7) | 113.9 MiB |
| `speed` | 1092 ms | 17.8 MiB | 220.1 ms (215.5-222.2) | 114.7 MiB, and 6.6 MiB more peak RSS |
| `speed-profile` | | | not measurable | |

`speed-profile` needs a profile. The APK ships `assets/dexopt/baseline.prof`, but in profile version 010, which this ART refuses ("Profile version mismatch"), and `profman` is not in the runtime. On the sample, with two classes, the three filters measured within 0.4 ms of each other. `verify` is the install's filter. (That table's absolute memory differs from the rows above: it was measured with a hand-made launch script before the default bodies' library existed.)

On a `noexec` file system, such as the sandbox's `/tmp`, `defaults.so` cannot be mapped, so the launcher makes the stubs at start as before, and says so; compiled `verify` code still loads, since it holds no machine code.

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
## Input

[`input.sh`](input.sh) measures, for each click or key, the time from the compositor sending it to the first frame the compositor receives whose pixels differ. Each event toggles what is drawn: the input sample's button switches the page color, and a key alternates `a` and backspace in its text field, whose cursor is hidden so blinking changes nothing; on RustDesk, clicks alternate the Chat and Settings tabs, and keys `2` and backspace in the Remote ID field. Every event waits until no frame came for 0.5 s. 30 clicks and 30 keys per app.

2026-11-03, branch `feat/compat-input`, other writers' builds running on the machine. Median (interquartile range), min-max:

| App | Click | Key |
|-----|-------|-----|
| Input sample APK (`View`s, software drawing) | 29.2 ms (24.7-33.8), 21.3-38.1 | 13.4 ms (9.0-17.8), 4.8-37.2 |
| RustDesk 1.5.1 (Flutter) | 16.6 ms (13.4-20.5), 7.3-82.4 | 14.7 ms (9.6-17.7), 6.0-22.7 |

This is input to a frame received by the compositor, not to light on a screen: no display scan-out is counted. Frames are paced by the 60 Hz vsync timer, so up to 16.7 ms of each figure is waiting for the next tick.

Input added nothing measurable to startup or memory. Against the base commit, both mounted at `/runtime/art` and measured back to back with 30 runs per row: framework sample to first frame warm 139.4 against 141.0 ms, 19.5 against 19.6 MiB private dirty; RustDesk to its first Flutter frame warm 253.8 against 252.2 ms, 87.5 against 86.1 MiB private dirty, cold 1064 against 1049 ms. One row moved: `FrameworkSmoke` warm read 73.0 against 155.9 ms, with a tight range, while other writers built on the machine; interleaved runs of base and branch on an idle machine afterwards gave 81.0 and 82.2 ms against 81.5 and 81.5 ms to exit (medians of 15).

## Composition and frame pacing

[`frames.sh`](frames.sh) measures what a frame costs the app: the CPU time of all its threads (summed from `/proc/<pid>/task/*/schedstat`) per frame the scripted compositor received, over 60 keys that each redraw part of the window, a and backspace in a text field, five fresh processes per row. Run it as `input.sh` (`APP=rustdesk` in `sandbox`).

2026-11-03, branch `feat/compat-compose` against its base (b387338), both mounted at `/runtime/art` from their own runtime copy and measured back to back, other writers' builds running on the machine. Median (range):

| Workload | Base | Branch |
|----------|------|--------|
| Input sample, CPU per frame (60 frames per run) | 4.12 ms (4.07-4.72) | 3.25 ms (3.24-3.34) |
| RustDesk 1.5.1, CPU per frame (101-102 frames per run) | 10.30 ms (10.10-10.30) | 10.29 ms (10.11-10.69) |

- The sample's frame costs 21% less CPU: the framework redraws only the dirty field, which posting copies alone, and composition redraws and reports only that damage, where every frame used to copy and compose the whole 400x700 window.
- RustDesk did not change: Flutter posts its whole `SurfaceView` frame through `ANativeWindow`, which composition must redraw whole either way, so its cost is the copies of that frame, not composition.

Startup, memory and input latency, same pair, back to back (`run.sh` with `RUNS=20`, `rustdesk.sh` with `RUNS=10`, `input.sh` with 30 events), warm medians:

| Workload | Base | Branch |
|----------|------|--------|
| Framework sample APK to first frame, installed | 123.3 ms, 19.1 MiB private dirty | 123.8 ms, 19.1 MiB |
| Milestone 3 platform to first frame | 17.7 ms, 5.0 MiB | 17.9 ms, 5.0 MiB |
| RustDesk to its first Flutter frame, installed | 212.7 ms, 80.8 MiB private dirty, 301.7 MiB peak RSS | 212.4 ms, 81.2 MiB, 304.2 MiB |
| Sample click / key latency | 28.4 / 13.6 ms | 29.3 / 12.0 ms |
| RustDesk click / key latency | 16.1 / 14.8 ms | 16.6 / 14.5 ms |

Every difference is inside the other row's interquartile range, so composition, subsurfaces and frame pacing cost nothing measurable at startup or in latency. RustDesk's Flutter engine now paces its frames by `AChoreographer`, which it did not find before, with no change in latency. Cold rows were run but not compared here; the machine was loaded.

Frame pacing itself is checked, not benchmarked, by `runtime/framework/compose_smoke.sh`: with the compositor's frame callbacks at 30 Hz an endless animation drew 30 frames per second (60 before this branch), with immediate callbacks 60, never more, and with none, as for a hidden window, 15.

## Limits

- The install-time preparation rows were measured with other workloads running on the machine; their warm rows are tight, their cold rows are not.
- In those runs, cold `uxdroid run` of milestone 3 read 35 ms, against 170 ms on the idle machine; the cause was not looked for. The cold rows of that section are comparable with each other only.

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

Also not measured: frame rate on a real compositor, frame time of GPU-drawn apps, input latency on a real compositor or against any competitor, memory over time, GPU rendering, and RustDesk with its data on tmpfs (only on disk).
