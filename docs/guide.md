# uxdroid user guide

Run Android APKs as native Linux processes, with no emulator, no container and no Android system image.

**Status: milestone 3.** `uxdroid run` opens the launcher Activity of a simple test APK in a Wayland window, joining its DEX and its bionic libraries in one process. Milestone 4, AOSP's own framework below real apps, is in progress: RustDesk's x86_64 build shows its first Flutter screen through `dev.uxdroid.system.Launcher` and takes clicks, touch and typed text from the Wayland seat. Of a corpus of five F-Droid apps, Loop Habit Tracker, Markor, Aegis and Mindustry (through `GLSurfaceView`) show their first screens; AnkiDroid shows its deck list under a fatal error dialog, because no WebView provider exists yet. See [the architecture](architecture.md) for the design and roadmap.

## Quick path

All builds and tests run in Docker. The only host requirement is Docker with Compose.

```bash
docker compose run --rm dev cargo test --workspace
docker compose run --rm dev cargo build
```

Inspect APKs in the sandbox, which has no network and mounts everything read-only:

```bash
UXDROID_SAMPLES=/path/to/apks docker compose run --rm sandbox \
  /cache/target/debug/uxdroid inspect /samples/app.apk
```

```text
package: org.schabi.newpipe
version: 0.29.1 (1015)
sdk: min 23, target 35
launcher: org.schabi.newpipe.MainActivity
dex: 3 files, 10671 classes, 75742 methods
abis: arm64-v8a (1), armeabi-v7a (1), x86 (1), x86_64 (1)
execution: direct
```

## Reading the result

| `execution` | Meaning |
|-------------|---------|
| `direct` | The APK has no native libraries, or has them for this machine's ABI |
| `needs-abi-bridge (...)` | Native libraries exist only for the listed ABIs and need translation |

Exit codes: `0` success, `1` unreadable or malformed APK, `2` wrong usage.

## Load a native library

`uxdroid load` loads a `.so` built for Android for the host's CPU, with its dependencies, and runs its initializers. On an x86_64 host an arm64-v8a `.so` runs through the translator, `libuxdroid_dynarmic.so`, which the `translation` service builds and `UXDROID_TRANSLATOR` names; without it, the load fails with the reason. On an aarch64 host an arm64-v8a `.so` is host code and loads with no translator, and an x86_64 one is refused: `error: libfoo.so is x86_64 code, which needs the translator: an arm64 host has no translator for x86_64 code yet`. x86_64 and aarch64 glibc hosts are supported. Loading runs the library's code, so use the sandbox for anything you did not build:

```bash
UXDROID_SAMPLES=/path/to/libs docker compose run --rm sandbox \
  /cache/target/debug/uxdroid load /samples/libfoo.so --search-dir /samples --call answer
```

```text
loaded: libfoo.so
needs: libc.so, libm.so
call answer: 42
```

| Option | Meaning |
|--------|---------|
| `--search-dir <dir>` | Also look for dependencies here, besides the library's own directory. Can repeat. |
| `--call <symbol>` | Call the exported function `int symbol(void)` and print its result. At most once. |

Exit codes: `0` success, `1` the library or a dependency cannot be loaded, or the symbol does not exist, `2` wrong usage. A load that fails names every missing library or symbol it knows of. A function from glibc that has not been audited for bionic compatibility, such as `regcomp`, stops the program with its name if called. See [the architecture](architecture.md).

The aarch64 build comes from the `dev-arm64` service, which cross-compiles into `/cache/target-arm64-host` and runs what it builds under QEMU user, so it works on an x86_64 machine; `sandbox-arm64` is its sandbox:

```bash
docker compose build dev dev-arm64
docker compose run --rm dev-arm64 cargo test -p uxdroid-cli --test load
UXDROID_SAMPLES=/path/to/libs docker compose run --rm sandbox-arm64 qemu-aarch64 \
  /cache/target-arm64-host/aarch64-unknown-linux-gnu/debug/uxdroid load /samples/libfoo.so --call answer
```

Tests that start `uxdroid` go through `UXDROID_TEST_RUNNER` (`qemu-aarch64` there); only the load tests do so far, so the other command tests fail in `dev-arm64` with an exec format error.

## Run an APK

`uxdroid run` needs ART built for the host, which takes about 16 GB of download and an hour of build. There is one runtime package per host CPU, x86_64 (`/runtime/art`) and arm64 (`/runtime/art-arm64-host`), with the same layout, the same framework library and the same install command; the commands below build the x86_64 one, and the arm64 one is built after it, as shown further down.

```bash
# Build the runtime in Docker, once. Volumes keep the result.
docker compose --profile art run --rm -T art-builder bash /work/runtime/art/sync.sh
docker compose --profile art run --rm -T art-builder bash /work/runtime/art/build.sh
docker compose --profile art run --rm -T art-builder bash /work/runtime/art/package.sh
docker compose --profile art run --rm -T art-builder bash /work/runtime/art/compile.sh
docker compose --profile art run --rm -T art-builder bash /work/runtime/art/build_platform.sh
# AOSP's Android 16 framework, pinned by checksum: 355 MB of download, 205 MB installed.
docker compose --profile art run --rm -T art-builder bash /work/runtime/framework/fetch.sh
docker compose --profile art run --rm -T art-builder bash /work/runtime/framework/build.sh
docker compose --profile art run --rm -T art-builder bash /work/runtime/framework/build_system.sh
# Check it: the framework draws, writes text and reads its resources.
docker compose run --rm -T dev bash /work/runtime/framework/smoke.sh
# The arm64 package, cross-built on this x86_64 machine from the same trees (another 26 GB of
# download and about 5 GB of output; see the architecture). ART comes from the master-art tree, and
# libandroid_runtime, ICU and Conscrypt from part of android-16.0.0_r1, in volume uxdroid-android16_src.
docker compose --profile art run --rm -T android16-builder bash /work/runtime/framework/android16/sync.sh
docker compose --profile art run --rm -T android16-builder bash /work/runtime/framework/android16/build.sh
docker compose --profile art run --rm -T art-builder bash /work/runtime/art/build_arm64.sh
docker compose --profile art run --rm -T -e ARCH=arm64 android16-builder bash /work/runtime/art/package.sh
docker compose --profile art run --rm -T -e ARCH=arm64 art-builder bash /work/runtime/art/compile.sh
docker compose --profile art run --rm -T -e ARCH=arm64 art-builder bash /work/runtime/art/build_platform.sh
docker compose --profile art run --rm -T -e ARCH=arm64 android16-builder bash /work/runtime/framework/build.sh
docker compose --profile art run --rm -T -e ARCH=arm64 art-builder bash /work/runtime/framework/build_system.sh
# Check it under QEMU user: a DEX main() as compiled arm64 code, the framework, an APK's first frame
# through `uxdroid install`, and `uxdroid run`. Programs uxdroid starts go through runtime/art/qemu-bin.
docker compose run --rm -T -e ARCH=arm64 sandbox-arm64 bash /work/runtime/art/smoke.sh
docker compose run --rm -T -e ARCH=arm64 dev-arm64 bash /work/runtime/framework/smoke.sh
docker compose run --rm -T -e ARCH=arm64 -v ./runtime/art/qemu-bin:/runtime/art-arm64-host/bin:ro \
    -v uxdroid-art_art_runtime:/runtime-real:ro dev-arm64 bash /work/runtime/framework/window_smoke.sh
docker compose run --rm -T -e ARCH=arm64 -v ./runtime/art/qemu-bin:/runtime/art-arm64-host/bin:ro \
    -v uxdroid-art_art_runtime:/runtime-real:ro dev-arm64 bash /work/runtime/art/app_smoke.sh
# An SDK-built APK's Activity, launched by AOSP's own ActivityThread, with no window.
docker compose run --rm -T dev bash /work/runtime/framework/app_smoke.sh
# The same Activity resumed and drawn by AOSP's ViewRootImpl into a Wayland window.
docker compose run --rm -T dev bash /work/runtime/framework/window_smoke.sh
# Bundles, Intents, file descriptors and AIDL calls through android.os.Parcel, whose natives are ours.
docker compose run --rm -T dev bash /work/runtime/framework/parcel_smoke.sh
# An NDK-built library using liblog, libandroid (with AChoreographer), libjnigraphics, EGL, GLES and
# OpenSL ES.
docker compose run --rm -T dev bash /work/runtime/framework/ndk_smoke.sh
# An NDK-built library that finds no AMediaCodec and keeps an AMediaFormat through libmediandk.
docker compose run --rm -T dev bash /work/runtime/framework/media_smoke.sh
# Sound through AudioTrack, AudioRecord, OpenSL ES and AAudio, checked sample by sample in files, then
# played through a private PulseAudio server.
docker compose run --rm -T dev bash /work/runtime/framework/audio_smoke.sh
# Wayland pointer, touch and keyboard input to an SDK-built APK's Views, through a scripted seat.
docker compose run --rm -T dev bash /work/runtime/framework/input_smoke.sh
# One process per app: uxdroid app starts it, a second launch goes to it, and intents, status, list
# and stop reach it through its control socket.
docker compose run --rm -T dev bash /work/runtime/framework/control_smoke.sh
# Apps as systemd user services, with socket_unit.py playing systemd: started on demand through the
# control socket, again after stop, once per profile, and with a socket passed at once.
docker compose run --rm -T dev bash /work/runtime/framework/service_smoke.sh
# Composition through a scripted compositor: an app's own layer changed by one atomic transaction,
# damage-only redraws, a dialog and a popup as subsurfaces (the dialog's translucent, with alpha),
# frame callbacks pacing an animation, a resize and a close.
docker compose run --rm -T dev bash /work/runtime/framework/compose_smoke.sh
# What the F-Droid corpus needed, checked with an app of ours: its own application uid, storage, the
# system services the corpus asks for, startActivityForResult results, broadcasts to registered and
# manifest receivers (alarms included), network callbacks, GLES 3.1 and 3.2, and a PC's features.
docker compose run --rm -T dev bash /work/runtime/framework/compat_smoke.sh
# An app delivered as a base APK and a feature split: the split's code, compiled at install, and its
# resources are the app's, from the install record and from a parse of the code directory.
docker compose run --rm -T dev bash /work/runtime/framework/split_smoke.sh
# RustDesk 1.5.1's x86_64 build (Flutter) to its first frame, then a click and typed text. Its code
# is untrusted: sandbox only.
docker compose run --rm dev cargo build --release -p uxdroid-system -p uxdroid-nativebridge -p uxdroid-cli
UXDROID_SAMPLES=/path/to/apks docker compose run --rm -T sandbox bash /work/runtime/framework/rustdesk_smoke.sh
UXDROID_SAMPLES=/path/to/apks docker compose run --rm -T sandbox bash /work/runtime/framework/rustdesk_input_smoke.sh
# An agent drives an app: ui, tap, type, wait, screenshot, through uxdroid app and uxdroid mcp; then
# the same on RustDesk's Flutter widgets in the sandbox.
docker compose run --rm -T dev bash /work/runtime/framework/agent_smoke.sh
UXDROID_SAMPLES=/path/to/apks docker compose run --rm -T sandbox bash /work/runtime/framework/rustdesk_agent_smoke.sh
# Which renderer a headless GLES 2 context gets: llvmpipe in dev, the host GPU in dev-gpu, which passes
# the host's GPU nodes and mounts its driver's libraries read-only (paths in compose.yaml).
# With DMABUF=1 it also fails unless a GPU frame comes out as a dmabuf, the path app EGL takes.
docker compose --profile gpu run --rm -T -e EXPECT=NVIDIA -e DMABUF=1 dev-gpu bash /work/runtime/gpu/gpu_probe.sh
# App EGL on the GPU end to end: the NDK sample's frame drawn through GBM, shown by weston's GL
# renderer as a dmabuf with no copy, and checked in a screenshot. RENDERER=gl gives the other smokes
# that compositor too.
docker compose --profile gpu run --rm -T dev-gpu bash /work/runtime/gpu/gpu_smoke.sh
# The compatibility corpus, F-Droid builds of Loop Habit Tracker, Markor, Aegis, AnkiDroid and
# Mindustry (verified against F-Droid's key, untrusted: sandbox only), each to a first screen that is
# not blank. A missing APK is skipped; CORPUS_APPS picks some; SHOTS=<dir> keeps the screenshots.
UXDROID_SAMPLES=/path/to/apks docker compose run --rm -T sandbox bash /work/runtime/framework/corpus_smoke.sh

# Apps play to the host's PulseAudio or PipeWire server, else ALSA. UXDROID_AUDIO=null plays into
# silence, and UXDROID_AUDIO=file:<dir> writes each stream's samples to a file.

# Build uxdroid, then run an APK with a Wayland compositor available.
docker compose run --rm dev cargo build --release -p uxdroid-cli -p uxdroid-nativebridge -p uxdroid-platform
uxdroid run app.apk --runtime /runtime/art
```

`libuxdroid_nativebridge.so` and `libuxdroid_platform.so` must sit next to the `uxdroid` program. Those Rust libraries need the glibc they were built against, so today this runs in the `dev` image.

| Option | Meaning |
|--------|---------|
| `--runtime <dir>` | The packaged runtime. Otherwise `UXDROID_RUNTIME`, then `$XDG_DATA_HOME/uxdroid/runtime` |
| `--exit-after-first-frame` | Exit once a frame is shown, for tests |

The first run of an APK compiles its DEX and extracts its native libraries into `$XDG_CACHE_HOME/uxdroid/<package>/<id>`. The id comes from the APK's archive directory and the runtime, so a changed APK installs fresh and an older one does not run a newer one's code. Only a crafted APK that copies another's CRC values could match it. A version in use keeps its files until it exits. The compiled runtime must stay at the path where it was compiled; a copy elsewhere works, but without compiled code. The app's current directory is `$XDG_CACHE_HOME/uxdroid/<package>/data`. APKs with native code only for another CPU are refused, because translation does not exist yet.

Exit codes: `1` it cannot start, `2` wrong usage. Once started, the process is the app and exits with the app's status.

### Install for the framework launcher

`dev.uxdroid.system.Launcher`, which starts an APK on AOSP's framework, reads app files where Android's installer leaves them. `uxdroid install` does the installer's part before the first start: it extracts the APK's native libraries for this CPU into `<data>/data/<package>/lib`, the directory `ApplicationInfo.nativeLibraryDir` names, and records the chosen ABI in `<data>/data/<package>/abi`. `<data>` is the absolute directory the launcher gets as `ANDROID_DATA`. Installing again replaces the libraries. An APK whose native code exists only for `arm64-v8a` installs those libraries on an x86_64 host, to run through the translator; APKs with native code only for any other CPU are refused, as are archives that name an entry twice, which Android refuses too. Installs of one package into one data directory take turns, through a lock on `<data>/app/.<package>.lock`, and clear what an interrupted install left.

```sh
UXDROID_RUNTIME=/runtime/art uxdroid install app.apk --data /path/to/android-data
```

For `arm64-v8a` libraries the install also resolves every import of every library, against the APK's other libraries, the system library tables and the translator's guest libc, and writes the result with each library's JNI entry points and exported function bounds to `<data>/data/<package>/lib/arm64-v8a.plan.json`. Each start of such an app saves which arm64 code it translated beside the plan, in `arm64-v8a.profile`, and the next start translates that code ahead on a thread of its own; a reinstalled library or another translator makes the profile stale, and it is replaced (`UXDROID_TRANSLATION_PROFILE=0` turns this off). A needed library nobody provides, or an import found nowhere, fails the install naming it, as the start would fail. An import the host has but guest code cannot call yet is bound to a stub that stops the program naming it only if called, since apps import much they never call; the report counts them and lists the first 20:

```text
installed io.anuke.mindustry: 2 native libraries for arm64-v8a in /data/data/io.anuke.mindustry/lib
translated for x86_64: 108 imports resolved at install, 0 from the app's libraries, 6 from system library tables, 102 from the guest libc, 0 weak ones null, 0 bound to stubs that stop the program naming them if called; plan in /data/data/io.anuke.mindustry/lib/arm64-v8a.plan.json
```

A start does not read the plan yet; it resolves the same imports again, which costs microseconds per import.

Every APK is verified before anything is installed, as Android's installer does: the APK Signature Scheme v3 or v2 signature when it has one (v3.1 and v3 signers are chosen for SDK 36), otherwise its JAR signature (v1), which Android accepts alone only from apps targeting SDK 29 or older. Signers with RSA keys up to 4096 bits or EC P-256 and P-384 keys verify; DSA and the verity-only algorithms do not, and ZIP64 archives are refused. The SHA-256 of each signer's certificate, as `apksigner verify --print-certs` prints it, is recorded in `<data>/app/.<package>.signer`, and an update signed by other certificates is refused, keeping the installed app, unless its key rotated from the installed one: a v3 or v3.1 signer can carry a proof of rotation, the certificates its key rotated from, each node signed by the key before it and the last one the signer's own, which the verifier checks whole (an APK whose proof is not signed by the key it rotated from is refused, `its key rotation proof is not signed by the key it rotated from`), and as on Android an update by a rotated key is accepted when its proof names the installed signer with the capability to keep the installed app's data. A past certificate whose capabilities were all removed is revoked: it matches neither an update nor the certificate a source names. Every signature's algorithm ID must fit its key, so an EC signature labelled as an RSA algorithm (or the reverse) is refused, in a signer and in a proof of rotation. The signer record then names the new key. An app installed before signers were recorded takes the next install's signer.

| Error | Meaning |
|-------|---------|
| `<apk>: it is not signed, and Android installs only signed APKs` | No v2, v3 or JAR signature |
| `<apk>: its signature does not verify: <reason>` | Damaged, changed after signing, or a stronger signature stripped |
| `<apk>: it targets SDK <n>, which needs a v2 or later signature, and it has only v1` | Android 11 and later refuse it too |
| `<package> is installed signed by a different certificate, so this update is refused: installed <sha256>, update <sha256>` | The installed app stays as it was |

An app delivered as a base APK plus split APKs, as Google Play delivers App Bundles, installs from its files or from one bundle: a `.apks` written by bundletool or exported by an installer, or an `.xapk`. Inside a bundle, an APK is any entry ending in `.apk`; with bundletool's table of contents, `toc.pb`, only the variant for the highest minimum SDK up to 36 is used, and standalone APKs for old systems are ignored when splits are there. Other files, such as an `.xapk`'s OBB expansion files, are not installed.

Every file given, APK or bundle, is first copied into a directory of the install's own, `<data>/app/.unpack-*` (mode 0700), and only that copy is verified, read and installed, so the bytes verified are the bytes installed even if the file given changes meanwhile; bundles are unpacked there too, and the directory is removed after the install. A refused install leaves nothing behind, not even a data directory it had to create. A file larger than 4 GiB, the most a bundle entry may unpack to, is refused (`<file> is larger than the 4294967296 bytes an APK or bundle may have, so it is refused`): a regular file before anything is copied, a device or pipe such as `/dev/zero` once that much was read. A directory given is named as one: `cannot read APK <dir>: Is a directory (os error 21)`.

```sh
uxdroid install base.apk split_config.x86_64.apk split_config.en.apk --data /path/to/android-data
uxdroid install app.apks --data /path/to/android-data
```

The APKs must make one app: exactly one base APK, splits of its package and `versionCode` with unique names, each signed by the base's certificates, a configuration split only for a feature split that is given, and every split type the base requires (`android:requiredSplitTypes`, or `android:isSplitRequired` with no split at all). All feature splits given are installed. Configuration splits are chosen as Play chooses them for a device, by their bundletool names: the ABI split for this CPU, or on an x86_64 host without one the `arm64-v8a` split, whose libraries are translated as an `arm64-v8a`-only APK's are (an app with ABI splits only for other CPUs is refused, as an APK only for another CPU is; the ABI chosen is the same for every feature's splits), the density split closest above 160 dpi, the screen density the framework reports, or else the highest, the language splits of the user's locale (`LANGUAGE`, then `LC_ALL`, `LC_MESSAGES` or `LANG`; English for none or `C`), and any whose purpose the name does not tell. The chosen splits go beside `base.apk` in the code directory as `split_<name>.apk`, their names in `<data>/app/<package>/splits`, one per line, sorted, even without a framework; their native libraries join the base's in `lib`. The launcher gives them to the app as Android does, in `ApplicationInfo.splitNames` and `splitSourceDirs`, so the framework's `LoadedApk` adds their code to the app's class loader after the base's and their resources to the app's; the install record and a parse without one both read the code directory as one package. The report then names them:

```
installed com.example.app: 1 native libraries for x86_64 in /data/data/com.example.app/lib
installed splits of com.example.app: config.en (language), config.mdpi (density), config.x86_64 (abi); skipped config.arm64_v8a (abi), config.fr (language), config.xxhdpi (density)
```

A single APK is reported as before, with no second line.

The sample apps `runtime/framework/build_system.sh` builds are signed by AOSP's `apksigner` with the public test key in [runtime/keys](../runtime/keys/README.md), and `make-first-apk` signs with the testkit's, so the smokes install them as any app.

With the framework's runtime, found as for `uxdroid run` through `UXDROID_RUNTIME`, then `$XDG_DATA_HOME/uxdroid/runtime`, the install also prepares everything a start would otherwise redo, in the app's code directory `<data>/app/<package>`, as Android's installer does:

| File | What a start does with it instead of the work |
|------|-----------------------------------------------|
| `base.apk` | A copy of the APK, which the launcher is given: `Launcher <runtime> <libuxdroid_system.so> <data>/app/<package>/base.apk` |
| `oat/x86_64/base.odex`, `base.vdex` (`oat/arm64/` on aarch64) | The DEX compiled by `dex2oat` with the `verify` filter, against the boot image and the framework's class loader, which ART maps instead of verifying the code at each start. A split with code gets `split_<name>.odex`, compiled after the base and the splits before it in the same class loader |
| `package.record` | The framework's own `PackageParser` result: `ApplicationInfo` and its meta-data, the launcher `ActivityInfo`, every activity, receiver, service and provider with its meta-data and intent filters, and `PackageInfo`, read back instead of parsing the manifest, also by every other app's PackageManager that looks this one up |
| `defaults.so` | The default bodies of every framework native, bound by ART by name on first call, instead of a stub per native made at each start |

The report then has a second line:

```
installed com.example.app: 2 native libraries for x86_64 in /data/data/com.example.app/lib
prepared com.example.app in /data/app/com.example.app: code compiled (verify), manifest recorded with launcher com.example.app.MainActivity, 5207 default bodies
```

The `verify` filter was chosen by measurement, in [benches/framework](../benches/framework/README.md). The record and the library are valid only for the runtime and the `base.apk` they were made with: if either changes, the launcher says so on stderr and parses the APK and makes the default bodies itself, and ART ignores compiled code that no longer matches. On a file system mounted `noexec`, `defaults.so` cannot be mapped and the bodies are made at start. If preparing fails, the install fails with `error: cannot prepare <package> for the framework: ...` and the previous install stays. Without a framework, only the libraries are installed (and the APKs of an app with splits or from a store, whose download is deleted, are kept in its code directory), an older preparation is removed, and stderr says `note: no framework in <runtime>, so <package> is not prepared: ...`; the launcher then works from the APK as before. The install runs the framework's parser on the manifest, not the app's code.

### Install from app sources

`uxdroid search`, `install <package>`, `update`, `remove`, `list` and `sources` find apps in app sources and keep them installed. A source is an F-Droid repository, and F-Droid's official one, `fdroid`, is there from the start, or Google Play, off until `uxdroid sources enable play` (see [Google Play](#google-play) below). Every command takes `--data <dir>`, the data directory to work on; without it they use `$XDG_DATA_HOME/uxdroid/data`, where `uxdroid app` starts apps from.

```sh
uxdroid search markor
uxdroid install net.gsantner.markor
uxdroid list
uxdroid update
uxdroid remove net.gsantner.markor
```

```text
NAME    PACKAGE              SOURCE  VERSION    UPDATED
Markor  net.gsantner.markor  fdroid  2026.10.0  2026-10-29
downloaded net.gsantner.markor 2026.10.0 (164) from fdroid: 11849016 bytes, SHA-256 c29412e02d03e4f0511746a1bcef48c6b73fc1f70c1024fbf04903b0b3f4c485
installed net.gsantner.markor: 0 native libraries for x86_64 in /home/me/.local/share/uxdroid/data/data/net.gsantner.markor/lib
prepared net.gsantner.markor in /home/me/.local/share/uxdroid/data/app/net.gsantner.markor: code compiled (verify), manifest recorded with launcher net.gsantner.markor.activity.MainActivity, 5207 default bodies
PACKAGE              SOURCE  VERSION
net.gsantner.markor  fdroid  2026.10.0 (164)
net.gsantner.markor 2026.10.0 (164) from fdroid is up to date
removed net.gsantner.markor: its code, data, signer record and component settings
```

| Command | What it does |
|---------|--------------|
| `search <text>... [--refresh] [--limit <n>]` | Apps of the enabled sources, Google Play asked for the text, whose name, package or summary holds every word, best first: a word that is the name or the package counts most, then the start of the name, a word of the name, the name, the package, and the summary. Each row has the app's name, package, source, the version that would be installed and the day the source last changed the app (UTC). At most 20 rows unless `--limit` says otherwise; how many more matched, and how many matched with no version for this machine, go to stderr as notes. Nothing found exits 1 |
| `install <package> [--from <source>] [--refresh]` | Installs the newest version for this machine: stable (no beta channel), native code for this CPU or none, and `minSdkVersion` up to 36 and `maxSdkVersion` from 36. The source is `--from`, else the one the app was installed from, else the first enabled source that has it, in `sources` order; Google Play is asked only when no F-Droid repository has the package, and when one has it stderr says so: `note: <package> is in <source>, so it is installed from there and Google Play is not asked; uxdroid install <package> --from play installs it from Google Play`. The APK is downloaded into `<data>/store/downloads`, refused if it is larger than the index says or does not have its SHA-256, and handed to the installer above, which checks the SHA-256 again on its own copy, then that its package, `versionCode` and signer certificates are those the index gives, then its signature and the installed app's signer as for any APK. The source is recorded in `<data>/app/.<package>.source`. The same version from the same source is not installed again, and an older one than the installed is refused |
| `update [<package>] [--refresh]` | For each app installed from a source, or the one named, installs the newest version for this machine from that source when it is newer, as `install` does; an update signed by another certificate is refused and the app stays. Apps installed from a file are left alone; naming one is an error. Exits 1 when any app failed |
| `remove <package>` | Removes the app's code directory, its data directory `<data>/data/<package>`, its signer and source records and its component settings `<data>/system/components/<package>`. A running app is refused: stop it first. The lock files stay |
| `list` | Installed apps with their source (`file` for an APK given as a file) and version |
| `sources` | The sources, with kind, whether enabled, address and the SHA-256 of the certificate their index must be signed with; once Google Play was enabled, a last line says how it signs in |
| `sources add <name> <url> --fingerprint <sha256>` | Adds an F-Droid repository: a name of lowercase letters, digits and `-`, its https address (plain http only for this machine: `localhost`, `127.x.x.x` or `[::1]`) and its signing certificate's SHA-256, as the repository publishes it (colons and case do not matter) |
| `sources enable\|disable <name>` | Uses a source, or stops using it, for `search`, `install` and `update`; `--from` cannot name a disabled source. `enable play` adds Google Play the first time, with a warning on stderr |
| `sources play [--account <email>]` | Signs in to Google Play as `<email>`, your own Google account, with a token read from standard input; without the option it says which account, if any, is used |
| `sources refresh-after <hours>` | How old an index may be before it is fetched again, 24 hours by default |

Exit codes: `0` success, also for `install` of the version already installed and for `update` with nothing newer; `1` the command failed, with its reason, or `search` found nothing; `2` wrong usage. Notes, such as an index fetched or read stale, go to stderr.

`install` takes a package when it is given one word that is a package name, does not end in `.apk`, `.apks` or `.xapk`, and names no existing file, or when `--from` or `--refresh` is given; anything else is installed from files, as above. When only the missing file decided it, stderr says so first: `note: <word> is not a file, so it is looked up as a package name in the enabled sources`. A lookup that fails before any index was ever kept leaves no store files behind. `install <package>` without `--data` uses the default data directory, where `install <apk>` still needs `--data`.

An F-Droid repository's index is trusted only as far as it is signed. `entry.jar` must carry a JAR signature by one certificate with the pinned SHA-256, `43238d512c1e5eb2d6569f4a3afbf5523418b82e0a3ed1552770abb9a9c9ccab` for F-Droid's own as F-Droid publishes it, and its `entry.json` names the index and diffs by size and SHA-256. An entry older than the index already read is refused as a replay. The index is fetched again when it is older than the interval or with `--refresh`: a diff from the cached index when the entry offers one, else all of `index-v2.json` (72 MB, 23 MB compressed in transfer, for F-Droid in November 2026). Only what the commands read is kept, about 10 MB in `<data>/store/sources/<name>/index.json`, with the permissions each version requests beside it in `permissions.json` (8 MB), which only `uxdroid tui` reads; a cache kept before uxdroid kept licenses and permissions, or whose `permissions.json` is missing or unreadable, is read as it is and fetched whole, instead of as a diff, on its next refresh. When an index cannot be fetched, the cached one is used and stderr says how old it is. Names from the index are checked before use: an entry with a package name that is not one, or a file name that is not a plain path inside the repository, is ignored. Fetching uses HTTPS with the system's certificates and proxy variables; an https address that redirects to plain http is refused, and a plain http address on this machine follows no redirect. Each download must finish within 10 s plus its largest allowed size at 1 Mbit/s (12 s for an `entry.jar`, about 9 minutes for F-Droid's whole index, at most an hour), so a stalled server is given up on with `cannot fetch <url>: it did not finish within <n> s, so it was stopped`. An index that is fresh is read without waiting for a refresh another command is running.

| Error | Meaning |
|-------|---------|
| `the index of <source> (<url>) is refused: entry.jar does not verify: <reason>` | Damaged, or changed after signing |
| `the index of <source> (<url>) is refused: entry.jar is signed by <sha256>, not by the certificate pinned for the source, <sha256>` | Another key signed it: a wrong fingerprint, or someone else's repository |
| `the index of <source> (<url>) is refused: its index does not have the SHA-256 its signed entry gives, so it was changed after signing` | The index, or a diff, is not the signed one; `is larger than its signed entry says` when it is longer |
| `the APK of <package> from <source> does not match its index (SHA-256 <sha256>, <n> bytes; the index gives <sha256>, <n> bytes), so it is refused` | The download is not the APK the index signed; `is larger than the <n> bytes its index gives` when it is longer |
| `the APK of <package> from <source> is the package <name>` / `has version code <n>` / `is signed by <sha256>, and its index says <sha256>` | The APK is not the release the index describes |

#### Google Play

Google Play is a source of its own kind, `play`, asked app by app over the API Play's store app uses, by a client written for uxdroid from public descriptions of that protocol and from observing its answers. It is off until enabled, and the first `uxdroid sources enable play` warns, on stderr:

```text
warning: Google Play's terms allow only Google's own store app, so Google may limit or block the accounts that sign in with another client such as uxdroid. uxdroid signs in only with your own Google account, at your own risk: uxdroid sources play --account <email> gives it.
```

```sh
uxdroid sources enable play
uxdroid sources play --account you@gmail.com   # paste the token, see below
uxdroid search signal
uxdroid install org.thoughtcrime.securesms --from play
```

```text
NAME                      PACKAGE                     SOURCE  VERSION  UPDATED
Signal Private Messenger  org.thoughtcrime.securesms  play    8.34.2   2026-11-02
downloaded org.thoughtcrime.securesms 8.34.2 (177501) from play: 3 APKs (base, config.mdpi, config.x86_64), 121812046 bytes
installed org.thoughtcrime.securesms: 12 native libraries for x86_64 in /home/me/.local/share/uxdroid/data/data/org.thoughtcrime.securesms/lib
installed splits of org.thoughtcrime.securesms: config.mdpi (density), config.x86_64 (abi)
```

Play is told about a generic device: Android's emulator build names, this machine's ABI (`x86_64` or `arm64-v8a`), SDK 36, 160 dpi, and the languages of the locale, so the splits it serves are the ones the installer keeps. `search` asks Play for the text and shows what it finds that matches every word, beside the F-Droid repositories' apps; `install`, `update` and `list` work as for any source, and `UPDATED` is the day Play shows. In `uxdroid tui`, Enter in the query box asks Play. An install acquires the app, free, asks Play for its delivery and downloads the base and every split it delivers into `<data>/store/downloads`; each file must have the size and SHA-256 (and SHA-1) Play states, a file Play states no hash for is refused (`states no hash for <file>`), and each must come from `https://play.googleapis.com` (or the API's own address, as a test server has it), and then the installer checks it as any APK: signatures, one signer for the base and its splits, the installed app's signer on an update, the splits chosen as for an `.apks` file, and the signing certificate Play's details name: the APK's signer, or, for an app that rotated its key, a certificate of its proof of rotation, since Play names the one it started from (Signal's names `29f34e5f…`, the APK's signer for SDK 36 is `4be4f6cd…`). Apps that are paid, or that Play does not deliver to this device, are refused: `<package> costs <price> on Google Play, and uxdroid installs only free apps from it` and `Google Play does not deliver <package> <version code> to this device (x86_64, SDK 36) (status <n>): ...`. Play is asked at most every 250 ms.

Signing in: Play is used only with your own Google account; uxdroid never signs in anonymously or through someone else's accounts. `--account <email>` takes a token read from standard input, not echoed on a terminal, never from the command line, where other users can see it; uxdroid never asks for a password. Give the AAS token (`aas_et/...`), or the OAuth token of Google's sign-in page, which uxdroid trades for an AAS token: open `https://accounts.google.com/EmbeddedSetup` in a browser, sign in and accept, then copy the value of the `oauth_token` cookie of `accounts.google.com` (it starts with `oauth2_4/`, and expires within minutes) and run `uxdroid sources play --account <email>`, pasting it. The device then checks in with Google, as a new Android device does, and its profile is registered with Play. Until an account is given, Play commands fail with `Google Play needs your own Google account: uxdroid sources play --account <email> signs in with it`; a data directory an earlier build set to sign in anonymously, through a third party's token dispenser, still loads and says `this data directory was set to sign in to Google Play anonymously, which uxdroid no longer does: ...`, and its kept anonymous sign-in is not used.

What it keeps, in `<data>/store/play`, a directory readable only by you: `settings.json` (the account's address), `token` (the AAS token) and `session.json` (the sign-in in use and the device profile it was made for), both readable only by you, never printed. A sign-in Play refuses (401 or 403) is renewed once; another profile, such as another locale, signs in again.

| Error | Meaning |
|-------|---------|
| `Google Play answered with a malformed message (<why>), so it is refused` | An answer that is cut short or damaged; nothing of it is used |
| `Google Play's delivery of <package> gives <file> the address <url>, which is not Google Play's, so it is refused` | A download address on another host or scheme; also `names a split <name>, which is not a valid or a new split name` and `has a malformed hash` |
| `the split <name> of <package> from play does not match its index (SHA-256 ...)` | A file is not the one Play stated |
| `Google's sign-in refused the account's token (it answered <code>): ...` | The token expired or was revoked: give a new one |
| `Google Play answered <code> to <request>` | Play refused a request, with a hint for 401, 403 and 429 |

Not measured: Play's search and install times with an account, since no account was available, so `benches/store/play.sh` has no results yet; the only Play timings taken came from a shared session of the token dispenser, which uxdroid can no longer obtain, and are not reported. Nor Aurora Store's own flow, an Android app that would need the network to run here, or downloads of Play's compressed files, which uxdroid does not use yet (Signal's would be 47 MB instead of 122). The account sign-in is proven only against a fake server in the tests; no real account was used.

Not yet: direct URLs as sources, beta channels, a source's own mirrors, removing a source, and checking `entry.json`'s `maxAge`.

#### A terminal UI over the same store

`uxdroid tui [--data <dir>]` searches, installs, updates and removes apps and changes sources in the terminal, through the same code as the commands above, so it installs exactly what `install` would and its errors are theirs. It needs a terminal: with stdin or stdout not one, it exits 1 with `error: uxdroid tui needs a terminal; uxdroid search, install, update, remove, list and sources work without one`; extra words exit 2 with `usage: uxdroid tui [--data <dir>]`.

```text
 1 Search │ 2 Installed │ 3 Sources
┌ Search ─────────────────────────────────────────────────────────────────────┐
│markor▏                                                                      │
└─────────────────────────────────────────────────────────────────────────────┘
┌ 1 of 4957 apps ─────────────────────────────────────────────────────────────┐
│NAME                  PACKAGE               SOURCE   VERSION      UPDATED    │
│Markor                net.gsantner.markor   fdroid   2026.10.0    2026-10-29 │
```

| View | Shows | Keys |
|------|-------|------|
| 1 Search | The apps of the enabled sources matching the query as it is typed, ranked as `search` ranks them, with name, package, source, version, last update and `✓` when installed; their details (summary, license, size, minimum SDK, installed version, and the permissions the version requests) beside the results from 120 columns, or in their place below that | `/` to the query, Enter or ↓ to the results, Enter details, `i` install, `u` update, `d` remove |
| 2 Installed | `list`, with the newer version each app's source offers | `u` update the selected app, `U` update every app installed from a source, `d` remove |
| 3 Sources | `sources` and the refresh interval | `e` or Space enable or disable, `a` add (name, URL and fingerprint, refused as `sources add` refuses them, with its message), `h` the interval in hours |

Everywhere: ↑ ↓ or `j` `k` move, PgUp PgDn `g` `G` jump, Tab and Shift-Tab go from the query to the results and to the next view, `1` `2` `3` choose a view, `r`, Ctrl-R or F5 refresh the indexes, `?` lists the keys, `q` or Ctrl-C quit. `d` asks first and removes only on `y`. Indexes, downloads, installs and source changes run on a worker thread, one at a time, with what it does and a download's progress on the status line, what the UI says meanwhile (such as `wait: <job> is still running`) on a line above it, and its outcome or error after it; Esc cancels a download in progress, and once the download is complete says the install can no longer be cancelled. Quitting while something runs waits for it to end, unless asked twice. SIGTERM, SIGINT and SIGHUP end it the same way, cancelling a download in progress first, then restore the terminal and end the process by that signal; a second signal ends it at once. If the store worker fails, the UI ends with the terminal restored, `error: the store worker failed: <message>` and exit 1. The first view is drawn before any index is read, and the UI starts from the cached indexes, refreshed only when older than the interval, as `search` does. It fits 80x24, drops the package, source and update columns below 76 columns and the query box's border below 15 rows, and asks for a larger terminal below 40x10. The terminal is restored when it quits, fails or panics. Over F-Droid's real index, cached, served on loopback (release build, 80x24 pseudo-terminal): the first frame 3.5 ms after launch, the apps 104 ms, and a keystroke's new results drawn in 2 ms (15 ms for the first letter, which matches most apps); measured by a pseudo-terminal harness, not yet a script under `benches/`. Not yet: a log of past outcomes, mouse input, and settings beyond sources and the interval.

### Control a running app

An installed app runs once per data directory, as Android keeps one process per app. `uxdroid app` sends it commands through its control socket, `$XDG_RUNTIME_DIR/uxdroid/<package>.sock` (`/tmp/uxdroid-<uid>/` without a runtime directory), which only the same user can open:

```sh
uxdroid app com.example.app start --data-dir /path/to/android-data
uxdroid app com.example.app intent --action android.intent.action.VIEW --data example://item/7 --extra source=cli
uxdroid app com.example.app status
uxdroid app list
uxdroid app com.example.app stop
```

```text
started com.example.app as process 4242: com.example.app/.MainActivity, log in /run/user/1000/uxdroid/com.example.app.log
com.example.app (process 4242): launched com.example.app/.DetailActivity
package: com.example.app
pid: 4242
data: /path/to/android-data
activity: com.example.app/.MainActivity (paused)
activity: com.example.app/.DetailActivity (resumed)
com.example.app 4242 /path/to/android-data
stopped com.example.app (process 4242, 2 activities finished)
```

| Command | Does |
|---------|------|
| `start` | Hands the intent to the running app, printing `<package> is already running as process <pid>; the intent went to it: <outcome> <activity>`. If the app is not running, starts the installed `<data-dir>/app/<package>/base.apk` in the background, its own session, with its output in the log beside the socket, and returns once it answers. Without options the intent is the launcher's, as a click on the app's icon |
| `intent` | Sends an intent to the running app, which must name an activity, an action or data |
| `status` | The process, its data directory, and its activities from bottom to top, `resumed` or `paused` |
| `stop` | Finishes every activity, and returns once the process has exited |
| `list` | Each running app: package, followed by `@<profile>` for a profile other than `default`, process and data directory |

`status`, `intent`, `stop` and `list` open a socket only while an instance holds its ownership lock, `<instance>.lock` beside it, so they never start an app whose socket a systemd socket unit holds (see `uxdroid service` below); `start` connects anyway, and that connection is what starts such an app.

The outcome of an intent is `launched`, `new_intent` (the activity's `onNewIntent`) or `brought_forward`, by the activity's launch mode and the intent's flags, as on Android; see [the architecture](architecture.md). A launch of an app that is already running, through `uxdroid app` or the launcher itself, starts no second process.

| Option | Meaning |
|--------|---------|
| `--activity <class>` | The activity, by class name or `.Name` relative to the package. Otherwise the intent's filters choose, or the launcher activity |
| `--action <action>`, `--data <uri>` | The intent's action and data |
| `--extra <key>=<value>` | A string extra. Can repeat |
| `--flags <n>` | `Intent.setFlags`, decimal or `0x` hexadecimal, such as `0x24000000` for `CLEAR_TOP` and `SINGLE_TOP` |
| `--profile <name>` | The profile, an instance of the app over its own data directory with its own control socket. Named as a package is, by letters, digits, `.` and `_`. Otherwise `default` |
| `--data-dir <dir>` | `start` only: the absolute data directory given to `uxdroid install`. Otherwise the profile's: `$XDG_DATA_HOME/uxdroid/data` for `default`, `$XDG_DATA_HOME/uxdroid/profiles/<name>` for any other |
| `--runtime <dir>` | `start` only: the runtime, found otherwise as for `uxdroid install` |

Exit codes: `0` success, also for `stop` of an app that is not running; `1` the command failed or the app refused it, with its reason; `2` wrong usage; `3` for `status`, `intent` and the agent commands below, the app is not running, printed as `<package> is not running`.

Profiles run one package several times at once, each over its own data directory, as separate users of one phone would. The default profile keeps the names above; profile `work` of `com.example.app` has the socket `com.example.app@work.sock` and the log `com.example.app@work.log`, and the launcher learns its profile from `UXDROID_PROFILE`. Install into a profile by naming its directory:

```sh
uxdroid install app.apk --data ~/.local/share/uxdroid/profiles/work
uxdroid app com.example.app start --profile work
uxdroid app com.example.app status --profile work
```

Two profiles must not share a data directory: the second finds the instance lock taken and hands its launch to the first one's socket, which is not its own, so it waits 30 s and fails.

### Drive a running app as an agent

The same socket lets a program, such as an AI agent, read the screen and act on it. `ui` reads every window's views and accessibility nodes in the app, on its main thread; `screenshot` reads the frames the compositor shows; `tap`, `type`, `scroll`, `back` and `key` go through the Wayland seat's own translation, so the app sees a finger and key presses, as from a user:

```sh
uxdroid app com.example.app ui                     # one line of JSON, below
uxdroid app com.example.app tap --node 5           # the node's center, from the last ui
uxdroid app com.example.app type "Hello" --node 6  # taps the field, then types
uxdroid app com.example.app wait --text "Hello" --timeout 5000
uxdroid app com.example.app screenshot shot.png
uxdroid app com.example.app key ENTER
```

```text
{"accessibility":true,"nodes":11,"snapshot":3,"truncated":false,"windows":[{"bounds":[0,0,400,700],"focused":true,"index":0,"root":{"bounds":[0,0,400,700],"children":[...{"bounds":[0,56,400,176],"class":"android.widget.Button","clickable":true,"enabled":true,"focusable":true,"id":5,"text":"TAP","visible":true}...],"class":"android.widget.FrameLayout","enabled":true,"id":1,"visible":true},"title":"com.example.app/com.example.app.MainActivity","type":1}]}
tapped node 5 at 200,116
typed 5 characters: 5 keys, 0 committed
{"node":{"bounds":[0,176,400,296],"class":"android.widget.EditText","editable":true,...,"id":6,"text":"Hello","visible":true},"snapshot":4,"waited_ms":12}
wrote shot.png (400x700)
pressed ENTER
```

| Command | Does |
|---------|------|
| `ui [--max-nodes <n>] [--max-depth <n>]` | Every window, bottom first, and its tree of nodes: `id` (valid for this snapshot), `class`, `resource_id`, `text`, `description`, `hint`, `bounds` on the display as `[left, top, right, bottom]`, and the flags `visible`, `enabled`, `clickable`, `long_clickable`, `focusable`, `focused`, `checkable`, `checked`, `selected`, `scrollable`, `editable`, `password`, `heading`, present only when true. At most 1000 nodes (5000 asked) and 64 levels (128); `truncated` says when more were left out; text is cut at 1000 characters |
| `screenshot <file.png> [--window <n>] [--max-size <n>]` | The display as the compositor shows it, or one window by its index in `ui`, as PNG; a longer side than `--max-size` (default 2048, 16 to 4096) is scaled down by a whole factor |
| `tap <x> <y>`, `tap --node <id> [--snapshot <n>]` | A finger down and up, 40 ms apart, at a display point or a node's center. With `--snapshot`, a node of an older snapshot is refused (`stale_snapshot`) |
| `type <text> [--node <id>]` | Key presses of the compositor's keymap, with shift where a character needs it, into the focused view, or into a node tapped first. Newline is Enter, tab is Tab. Characters the keymap cannot type are committed through the focused view's `InputConnection`, as an input method does. At most 4096 bytes; other control characters are refused |
| `scroll <x> <y> \| --node <id> [--dx <n>] [--dy <n>]` | Wheel steps at the point, positive down and right, at most 50 a call |
| `back`, `key <name>` | The back key, or a key named as `KeyEvent.KEYCODE_<name>`: `ENTER`, `DEL`, `TAB`, `DPAD_DOWN`, `ESCAPE` |
| `wait --text <text> \| --idle [--timeout <ms>]` | Until a visible node's text, description or hint holds the text, printing it as JSON, or until no window drew for 500 ms. Default 10 s, at most 60 s; past it, the app refuses with `timeout` |

All take `--profile`, and exit `3` when the app is not running.

Nothing an agent sends runs code: text is only typed, nodes are only read, and the commands reach only the app the socket belongs to, as the same user. Some apps report their widgets only while accessibility is on, as Flutter does through its accessibility bridge, so the app's process registers an in-process `IAccessibilityManager` the first time an agent asks for `ui` and tells the app that accessibility is on, with touch exploration off; when the last connection that asked closes, it is off again. The first `ui` on a connection waits up to 3 s for the nodes to settle (about 300 ms on the samples). One-shot `uxdroid app` commands each open their own connection, so each `ui` turns it on anew; `uxdroid mcp` keeps one connection per app.

#### An MCP server for agents

`uxdroid mcp` is a [Model Context Protocol](https://modelcontextprotocol.io) server over stdio whose tools are these commands: `list`, `status`, `start`, `stop`, `intent`, `ui`, `screenshot`, `tap`, `type`, `scroll`, `back`, `key` and `wait`, each taking the app's `package` (and `profile`) besides the arguments of the protocol command (`node`, `snapshot`, `x`, `y`, `text`, `dx`, `dy`, `key`, `idle`, `timeout_ms`, `max_nodes`, `max_depth`, `window`, `max_size`, and the intent fields with `data_dir` for `start`). `screenshot` returns image content. One server serves every app, so a client is set up once and an agent can move between apps. It speaks MCP from 2024-11-05 to 2025-11-25 through `initialize`, and 2026-07-28 through `server/discover`. To add it to Claude Code:

```sh
claude mcp add uxdroid -- uxdroid mcp
```

Other clients take the same command, `uxdroid mcp`, as a stdio server. `uxdroid` must be the installed program, next to `libuxdroid_system.so`, with `XDG_RUNTIME_DIR` set as for `uxdroid app`.

### Run an app as a systemd user service

`uxdroid service` makes an installed app, one profile of it, a systemd user service that starts on demand: a socket unit holds its control socket, and the first connection to it, such as `uxdroid app <package> start`, starts the app. It restarts on failure, logs to the journal, and runs inside the memory and CPU limits given here.

```sh
uxdroid install app.apk --data ~/.local/share/uxdroid/data
uxdroid service enable com.example.app
systemctl --user daemon-reload && systemctl --user enable --now uxdroid-com.example.app@default.socket
uxdroid app com.example.app start
journalctl --user -u uxdroid-com.example.app@default.service
```

`enable` writes two units into `$XDG_CONFIG_HOME/systemd/user` (`~/.config/systemd/user`) and prints the `systemctl --user` command that activates them; with `--now` it runs that command itself. Nothing else runs `systemctl`.

| Command | Does |
|---------|------|
| `enable <package>` | Writes `uxdroid-<package>@<profile>.socket` and `.service` for an installed app, replacing earlier ones it wrote |
| `disable <package>` | Removes them and the links `systemctl --user enable` made, then prints how to stop what still runs; with `--now`, runs `systemctl --user disable --now` first and `daemon-reload` after |
| `status <package>` | The units, the socket, the data directory, whether it starts at login, and the running process if any. It never connects to a socket no instance serves, which would start the app |
| `run <package>` | The units' command: runs the app in the foreground, this process becoming the launcher, so the socket systemd passes reaches it. Without systemd it runs as `uxdroid app start` would, attached |

| Option | Meaning |
|--------|---------|
| `--profile <name>` | The profile, as for `uxdroid app`. Otherwise `default` |
| `--data-dir <dir>` | `enable`, `run`: the data directory the app was installed into. Otherwise the profile's |
| `--runtime <dir>` | `enable`, `run`: the runtime, found otherwise as for `uxdroid install` |
| `--memory-max <size>`, `--memory-high <size>` | `enable`: the cgroup's `MemoryMax=` and `MemoryHigh=`, in bytes with an optional `K`, `M`, `G` or `T`, a percentage, or `infinity`. Otherwise unlimited |
| `--cpu-weight <n>` | `enable`: `CPUWeight=`, 1 to 10000. Otherwise 100 |
| `--on-login` | `enable`: also start the app when the user logs in (`WantedBy=default.target`), not only on demand |
| `--now` | `enable`, `disable`: run `systemctl --user` instead of printing it |

The units are written per package and profile rather than from one template, because what differs between apps is most of the unit: the data directory, the network the manifest asks for, the limits and the start at login. Their names keep systemd's template syntax, with the profile as instance, so `systemctl --user list-units 'uxdroid-*'` lists them. The socket unit listens on `%t/uxdroid/<package>.sock` (`<package>@<profile>.sock` for another profile) with mode 0600 in a directory of mode 0700, the same socket `uxdroid app` uses. The service:

| Setting | Why |
|---------|-----|
| `ExecStart=<uxdroid> service run <package> --profile ... --data-dir ... --runtime ...` | Absolute paths, resolved at `enable`; `%` and `$` in them are escaped, and paths with spaces, quotes, backslashes or control characters are refused |
| `Environment=` `UXDROID_PROFILE`, `UXDROID_RUNTIME`, `ANDROID_DATA` | `WAYLAND_DISPLAY`, `XDG_RUNTIME_DIR` and the sound server come from the user manager's environment, which desktop sessions fill; no secret is written |
| `Restart=on-failure`, `RestartSec=2`, at most 5 starts in 60 s, `RestartPreventExitStatus=75` | 75 is a start that finds the app already running over the same data directory outside the service |
| `StandardOutput=journal`, `StandardError=journal`, `SyslogIdentifier=uxdroid-<package>@<profile>` | The app's log is the journal |
| `NoNewPrivileges`, `ProtectSystem=strict` with `ReadWritePaths=<data> %t/uxdroid`, `PrivateTmp`, `ProtectKernelTunables`, `ProtectKernelModules`, `ProtectControlGroups`, `RestrictNamespaces`, `RestrictRealtime`, `RestrictSUIDSGID`, `LockPersonality`, `SystemCallArchitectures=native`, `UMask=0077` | Hardening that leaves Wayland and the sound server reachable: they are Unix sockets, which a read-only file system does not block. `MemoryDenyWriteExecute` is not set, because ART's JIT writes code |
| `RestrictAddressFamilies=AF_UNIX AF_NETLINK`, plus `AF_INET AF_INET6` when the manifest names `android.permission.INTERNET` | The network only for apps that ask for it |

On a user manager, `ProtectSystem`, `PrivateTmp` and the other namespace settings need unprivileged user namespaces; where the host restricts them, as Ubuntu does through AppArmor, systemd may run the service without those namespaces. Check what applies with `systemd-analyze --user security uxdroid-<package>@<profile>.service`.

Exit codes: `0` success, also for `disable` of a profile with no service; `1` the command failed, such as an app that is not installed or a unit of that name that `uxdroid service` did not write, which is never touched; `2` wrong usage; `3` for `status`, the profile has no service, printed as `<package> has no service for profile <profile>`. `run` exits with the app's status.

## Tests

`cargo test` covers parsers, the loader and command errors. It needs the NDK but not ART, so it never runs an app. These scripts do, with the runtime built:

| Script, in `runtime/art` | Proves |
|--------------------------|--------|
| `app_smoke.sh` | An APK paints a Wayland window, read back as pixels |
| `jni_smoke.sh` | DEX reaches a bionic library over JNI |
| `cache_smoke.sh` | Each APK runs its own code, and failed installs stay failed |
| `window_events_smoke.sh` | Close and resize work, including during a frame |
| `smoke.sh` | ART runs compiled code in a clean system |

Run the first four with `docker compose run --rm -T dev bash /work/runtime/art/<script>`, and `smoke.sh` in the `art-runtime` service. There is no CI yet.

The arm64 translation tests skip in `dev`, which has no engine. Run them in the `translation` service, which is `dev` plus the engine built from pinned sources:

```bash
docker compose build dev && docker compose build translation
docker compose run --rm translation cargo test -p uxdroid-translate -p uxdroid-cli
```

## Performance

Every milestone is measured against the competitors that can run in the test environment, with a repeatable benchmark under `benches/`. [benches/loader](../benches/loader/README.md) compares load times with other loaders, and [benches/art](../benches/art/README.md) compares runtime startup. [benches/translation](../benches/translation/README.md) compares arm64 code run by the bridge's translator with native x86_64 and QEMU user. [benches/framework](../benches/framework/README.md) measures startup and memory on AOSP's framework, RustDesk included, against milestone 3's own platform, and the cost and latency of audio; no competitor could run those workloads here.
