<div align="center">

<img src="docs/logo/icon.svg" alt="" width="112">

# uxdroid

**Run Android apps on Linux as native processes.**

[![CI](https://github.com/flaviopifiator/uxdroid/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/flaviopifiator/uxdroid/actions/workflows/ci.yml)
[![Coverage](https://img.shields.io/endpoint?url=https%3A%2F%2Fraw.githubusercontent.com%2Fflaviopifiator%2Fuxdroid%2Fbadges%2Fcoverage.json)](https://github.com/flaviopifiator/uxdroid/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/flaviopifiator/uxdroid?color=2EA043&logo=github&logoColor=white)](https://github.com/flaviopifiator/uxdroid/releases)
[![License](https://img.shields.io/github/license/flaviopifiator/uxdroid?color=0A66C2)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-2024-B7410E?logo=rust&logoColor=white)](Cargo.toml)

[![Debian](https://img.shields.io/badge/Debian-12%20%7C%2013-A81D33?logo=debian&logoColor=white)](#install)
[![Ubuntu](https://img.shields.io/badge/Ubuntu-22.04%20%7C%2024.04%20%7C%2026.04-E95420?logo=ubuntu&logoColor=white)](#install)
[![CPUs](https://img.shields.io/badge/CPU-amd64%20%7C%20arm64-555555?logo=linux&logoColor=white)](#install)
[![Display](https://img.shields.io/badge/display-Wayland-FFBC00?logo=wayland&logoColor=black)](docs/architecture.md)
[![Framework](https://img.shields.io/badge/framework-Android%2016-3DDC84?logo=android&logoColor=white)](docs/architecture.md)

[![F-Droid catalog](https://img.shields.io/badge/F--Droid%20catalog-107%2F150%20first%20frame-1976D2?logo=fdroid&logoColor=white)](docs/corpus.md)

</div>

No emulator, no virtual machine, no container and no Android system image: each app is an ordinary Linux process with its own Wayland window, its sound on your audio server and its input from your seat.

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
| Store | `uxdroid search`, `install`, `update`, `remove` and a terminal UI over F-Droid repositories, and Google Play with your own account when you enable it; every index and APK verified |
| Compatibility | Of 150 apps sampled from the F-Droid catalog, 107 draw their first frame and 119 are still running when the run ends; [docs/corpus.md](docs/corpus.md) runs the catalog unattended and ranks what stops the rest |
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

## Install

Each [release](https://github.com/flaviopifiator/uxdroid/releases) has, for amd64 and arm64, a `.deb` and a `.tar.gz` of uxdroid and a `.deb` of its runtime, with their SHA-256 sums in `SHA256SUMS`. CI installs and uses the uxdroid packages on Debian 12 and 13 and Ubuntu 22.04, 24.04 and 26.04.

```bash
gh release download v0.1.0 --repo flaviopifiator/uxdroid --pattern '*_amd64.deb' --pattern SHA256SUMS
sha256sum --check --ignore-missing SHA256SUMS
sudo apt install ./uxdroid_0.1.0_amd64.deb ./uxdroid-runtime_0.1.0_amd64.deb
```

`uxdroid` is the program and its libraries, about 3 MB. `uxdroid-runtime` is ART and the Android framework, a download of 154 MB for amd64 and 170 MB for arm64, installed in `/usr/lib/uxdroid/runtime`. The tarball runs from wherever it is unpacked, for distributions without apt; a runtime works only at the path it was built for, so with the tarball, unpack the runtime package's files into `/usr/lib/uxdroid/runtime` and set `UXDROID_RUNTIME` to that directory. [Packages](docs/guide.md#packages) explains how they are built.

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
uxdroid tui                                 # the same, as a terminal UI that also starts apps
uxdroid --help                              # every command, under the logo
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

uxdroid is under active development. It ships as packages for Debian and Ubuntu, and as a tarball for other distributions.

| Done | In progress | Not planned |
|------|-------------|-------------|
| Framework, graphics, audio, input, services, store, Google Play source (opt-in), split installs, arm64 hosts, arm64 translation | Apps starting other apps' activities, x86-to-arm64 translation, the arm64 translator in the amd64 package | Google Mobile Services (licensed by Google only), Play Integrity |

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
| `packaging/`, `.github/workflows` | package builds, the distribution install check and CI |

## Contributing

- Build and test only through `docker compose`; never install toolchains on the host.
- Untrusted APKs run only in the `sandbox` service, which has no network and a read-only file system.
- Treat every APK as hostile: bounds-check every offset and never panic on bad input.
- Write a failing test for each rule first, through the public interface.
- Measure performance changes against the competitors that can run here, and say what was not measured.
- Use [Conventional Commits](https://www.conventionalcommits.org).

## Legal

uxdroid is not affiliated with Google or the Android Open Source Project. Android is a trademark of Google LLC. AOSP components are built from source under their own licenses; [NOTICE](NOTICE) lists the third-party code in uxdroid's artifacts.

uxdroid is licensed under the [Apache License, Version 2.0](LICENSE).
