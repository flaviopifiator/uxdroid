# uxdroid

Run Android APKs as native Linux processes, with no emulator, no container and no Android system image.

**Status: milestone 3.** `uxdroid run` opens the launcher Activity of a simple test APK in a Wayland window, joining its DEX and its bionic libraries in one process. Milestone 4, AOSP's own framework below real apps, is in progress: RustDesk's x86_64 build shows its first Flutter screen through `dev.uxdroid.system.Launcher` and takes clicks, touch and typed text from the Wayland seat. Of a corpus of five F-Droid apps, Loop Habit Tracker, Markor, Aegis and Mindustry (through `GLSurfaceView`) show their first screens; AnkiDroid shows its deck list under a fatal error dialog, because no WebView provider exists yet. See [the architecture](docs/architecture.md) for the design and roadmap.

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

`uxdroid load` loads an x86_64 `.so` built for Android, with its dependencies, and runs its initializers. An arm64-v8a `.so` runs through the translator, `libuxdroid_dynarmic.so`, which the `translation` service builds and `UXDROID_TRANSLATOR` names; without it, the load fails with the reason. Only an x86_64 glibc host is supported. Loading runs the library's code, so use the sandbox for anything you did not build:

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

Exit codes: `0` success, `1` the library or a dependency cannot be loaded, or the symbol does not exist, `2` wrong usage. A load that fails names every missing library or symbol it knows of. A function from glibc that has not been audited for bionic compatibility, such as `regcomp`, stops the program with its name if called. See [the architecture](docs/architecture.md).

## Run an APK

`uxdroid run` needs ART built for the host, which takes about 16 GB of download and an hour of build. Only an x86_64 glibc host is supported.

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

With the framework's runtime, found as for `uxdroid run` through `UXDROID_RUNTIME`, then `$XDG_DATA_HOME/uxdroid/runtime`, the install also prepares everything a start would otherwise redo, in the app's code directory `<data>/app/<package>`, as Android's installer does:

| File | What a start does with it instead of the work |
|------|-----------------------------------------------|
| `base.apk` | A copy of the APK, which the launcher is given: `Launcher <runtime> <libuxdroid_system.so> <data>/app/<package>/base.apk` |
| `oat/x86_64/base.odex`, `base.vdex` | The DEX compiled by `dex2oat` with the `verify` filter, against the boot image and the framework's class loader, which ART maps instead of verifying the code at each start |
| `package.record` | The framework's own `PackageParser` result: `ApplicationInfo`, the launcher `ActivityInfo` and `PackageInfo`, read back instead of parsing the manifest |
| `defaults.so` | The default bodies of every framework native, bound by ART by name on first call, instead of a stub per native made at each start |

The report then has a second line:

```
installed com.example.app: 2 native libraries for x86_64 in /data/data/com.example.app/lib
prepared com.example.app in /data/app/com.example.app: code compiled (verify), manifest recorded with launcher com.example.app.MainActivity, 5207 default bodies
```

The `verify` filter was chosen by measurement, in [benches/framework](benches/framework/README.md). The record and the library are valid only for the runtime and the `base.apk` they were made with: if either changes, the launcher says so on stderr and parses the APK and makes the default bodies itself, and ART ignores compiled code that no longer matches. On a file system mounted `noexec`, `defaults.so` cannot be mapped and the bodies are made at start. If preparing fails, the install fails with `error: cannot prepare <package> for the framework: ...` and the previous install stays. Without a framework, only the libraries are installed, an older preparation is removed, and stderr says `note: no framework in <runtime>, so <package> is not prepared: ...`; the launcher then works from the APK as before. The install runs the framework's parser on the manifest, not the app's code.

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

The outcome of an intent is `launched`, `new_intent` (the activity's `onNewIntent`) or `brought_forward`, by the activity's launch mode and the intent's flags, as on Android; see [the architecture](docs/architecture.md). A launch of an app that is already running, through `uxdroid app` or the launcher itself, starts no second process.

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

The performance rule in [AGENTS.md](AGENTS.md) applies to every milestone. [benches/loader](benches/loader/README.md) compares load times with other loaders, and [benches/art](benches/art/README.md) compares runtime startup. [benches/translation](benches/translation/README.md) compares arm64 code run by the bridge's translator with native x86_64 and QEMU user. [benches/framework](benches/framework/README.md) measures startup and memory on AOSP's framework, RustDesk included, against milestone 3's own platform, and the cost and latency of audio; no competitor could run those workloads here.
