# uxdroid

**Run Android apps on Linux as native processes.** No emulator, no virtual machine, no container and no Android system image: each app is an ordinary Linux process with its own Wayland window, its sound on your audio server and its input from your seat.

uxdroid joins three things in one process: Android's own runtime (ART) and framework, built from AOSP for a glibc host; a loader that runs the app's bionic-linked native libraries under glibc; and a platform layer that answers the framework's system services with the desktop's real ones.

```console
$ uxdroid search markor
NAME    PACKAGE                SOURCE  VERSION           UPDATED
Markor  net.gsantner.markor    fdroid  2026.10.0 (164)   2026-10-28

$ uxdroid install net.gsantner.markor
$ uxdroid app net.gsantner.markor start
```

## Highlights

| Area | What works today |
|------|------------------|
| Apps | Real apps open and are usable: RustDesk (Flutter), Loop Habit Tracker, Markor, Aegis, Mindustry (GLES), the F-Droid client and microG |
| Framework | AOSP's Android 16 framework below the app, launched by its own `ActivityThread`, drawing through `ViewRootImpl` |
| Graphics | EGL and GLES 2 to 3.2; frames handed to the compositor as dmabufs on the GPU, with no copy, on any Mesa or vendor driver |
| Native code | bionic `.so` files loaded under glibc; `arm64-v8a`-only apps translated on x86_64 |
| CPUs | One package per host CPU: x86_64, and arm64 where ARM libraries run natively |
| Store | `uxdroid search`, `install`, `update`, `remove` and a terminal UI over F-Droid repositories, every index and APK verified |
| Installs | Split APKs and `.apks`/`.xapk` bundles, APK signature schemes v1 to v3.1, updates refused when the signer changes |
| Desktop | Wayland windows, pointer, touch and keyboard, PulseAudio/PipeWire/ALSA audio, one process per app, systemd user services |
| Automation | Every running app has a control socket; `uxdroid mcp` exposes it to agents over the Model Context Protocol |

## Performance

Measured on this project's test machine; each benchmark is under [`benches/`](benches) and names what it did not measure.

| Workload | Result |
|----------|--------|
| RustDesk 1.5.1 (Flutter), first frame, x86_64 | 365-401 ms |
| Mindustry, x86_64, software rendering | 153-220 fps |
| App frame handed to the compositor on the GPU | 0.33 ms of app CPU per frame (was 6.6 ms with readback) |
| arm64 code translated on x86_64 | about 40% of native speed (QEMU user: 36%) |
| `uxdroid search`, cached F-Droid index (4,957 apps) | 71 ms |
| `uxdroid install` of Markor from a local F-Droid mirror | 0.33 s |

Not measured yet: arm64 hardware (arm64 numbers so far come from QEMU), Waydroid and Android Translation Layer on the same apps.

## Quick start

uxdroid builds and tests entirely in Docker; the only host requirement is Docker with Compose.

```bash
git clone https://github.com/flaviopifiator/uxdroid.git
cd uxdroid

# Tests and a debug build
docker compose run --rm dev cargo test --workspace
docker compose run --rm dev cargo build --release

# Android's runtime and framework, built once from pinned AOSP sources (volumes keep the result)
docker compose --profile art run --rm -T art-builder bash /work/runtime/art/sync.sh
docker compose --profile art run --rm -T art-builder bash /work/runtime/art/build.sh
docker compose --profile art run --rm -T art-builder bash /work/runtime/framework/build.sh
```

The [user guide](docs/guide.md) covers every build step, including the arm64 package and the translator image.

## Usage

### Find and install apps

```bash
uxdroid search signal                       # every enabled source
uxdroid install org.fdroid.fdroid           # newest compatible release
uxdroid install ./app.apk ./split_config.en.apk --data ~/.local/share/uxdroid/data
uxdroid list
uxdroid update                              # every app, from the source it came from
uxdroid remove net.gsantner.markor
uxdroid sources add izzy https://apt.izzysoft.de/fdroid/repo --fingerprint <sha256>
uxdroid tui                                 # the same, as a terminal UI
```

F-Droid's official repository is enabled by default and pinned to its signing certificate. An index is trusted only through its signed entry, and an APK only when its hash, package, version and signer match the index and its own signature verifies.

### Run and control apps

```bash
uxdroid app org.fdroid.fdroid start         # one process per app; a second start reaches it
uxdroid app org.fdroid.fdroid status
uxdroid app org.fdroid.fdroid screenshot shot.png
uxdroid app org.fdroid.fdroid stop
uxdroid service enable org.fdroid.fdroid --now   # a systemd user service, started on demand
```

### Let an agent drive an app

```bash
claude mcp add uxdroid -- uxdroid mcp
```

The MCP server's tools read the view tree, tap, type, wait for text and take screenshots of any running app.

## How it works

```text
 APK ──► uxdroid install ──► verified copy, splits chosen, DEX compiled by dex2oat, libraries staged
                                   │
 uxdroid app start ──► one Linux process
        ├── ART + AOSP framework (Android 16), built from AOSP for glibc
        ├── bionic loader: the app's .so files under glibc (translated if they are arm64-only)
        └── platform: Wayland window, input, audio, GPU, system services, control socket
```

The [architecture](docs/architecture.md) explains each layer, its decisions and the roadmap.

## Project status

uxdroid is under active development and not yet packaged for distributions.

| Done | In progress | Not planned |
|------|-------------|-------------|
| Framework, graphics, audio, input, services, store, split installs, arm64 hosts, arm64 translation | Google Play source (opt-in), apps starting other apps' activities, x86-to-arm64 translation, distribution packages | Google Mobile Services (licensed by Google only), Play Integrity |

Apps that require Play Integrity attestation, such as many banking apps, cannot work on uxdroid. WebView is not implemented yet.

## Repository layout

| Path | Contents |
|------|----------|
| `crates/apk` | APK, manifest, DEX and signature parsing; split planning |
| `crates/bridge`, `crates/nativebridge` | bionic loader, symbol shims, arm64 translation glue |
| `crates/translate` | arm64 translator engine integration |
| `crates/ndk`, `crates/audio`, `crates/platform` | NDK libraries, audio, Wayland and input |
| `crates/system` | the system services the framework calls |
| `crates/store` | app sources, index verification and release choice |
| `crates/cli` | the `uxdroid` command, its terminal UI and MCP server |
| `system/java` | the Java side of the platform |
| `runtime/` | AOSP build scripts, patches and smoke tests |
| `benches/` | repeatable benchmarks |

## Contributing

- Build and test only through `docker compose`; never install toolchains on the host.
- Untrusted APKs run only in the `sandbox` service, which has no network and a read-only file system.
- Treat every APK as hostile: bounds-check every offset and never panic on bad input.
- Write a failing test for each rule first, through the public interface.
- Measure performance changes against the competitors that can run here, and say what was not measured.
- Use [Conventional Commits](https://www.conventionalcommits.org).

## Legal

uxdroid is not affiliated with Google or the Android Open Source Project. Android is a trademark of Google LLC. AOSP components are built from source under their own licenses; [NOTICE](NOTICE) lists the third-party code in uxdroid's artifacts. uxdroid's own license has not been chosen yet.
