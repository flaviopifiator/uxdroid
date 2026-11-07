# Popular corpus

Well-known apps and games, from official sources that need no account, run with the F-Droid catalog's runner (`runtime/corpus/run_apps.sh`, see [docs/corpus.md](../../docs/corpus.md)), with the emphasis on arm64 translation: apps their developers build per ABI run twice, once with their x86_64 code and once with their arm64-v8a code through the translator.

```sh
benches/popular/fetch.sh        # download and verify into ~/uxdroid-samples/popular/apks
benches/popular/run.sh          # [--jobs 3] [--wait 60] [--only vlc,luanti] [--out <dir>]
python3 -I benches/popular/report.py table ~/uxdroid-samples/popular/out/<date>/report.tsv
```

## Sources and verification

[`manifest.tsv`](manifest.tsv) lists one APK per row, with its official URL, its expected SHA-256 and where that hash comes from. `fetch.sh` downloads over HTTPS only (redirects too) and keeps a file only when its SHA-256 matches; `fetched.tsv` in the output directory records URL, size and SHA-256 of each file kept.

| Source | How a file is verified |
|--------|------------------------|
| [IzzyOnDroid](https://apt.izzysoft.de/fdroid/) | uxdroid's own store: `uxdroid sources add izzy https://apt.izzysoft.de/fdroid/repo --fingerprint 3BF0D6AB…937A` (the fingerprint the repository publishes), then `search --refresh`, which checks `entry.jar` against that certificate and the index against the SHA-256 the entry signs; `corpus.py download --repo` then checks each APK's size and SHA-256 against the verified index. The newest stable release is taken, so these rows are not pinned |
| Developer sites | The checksum the developer publishes: Signal's `latest.json`, VideoLAN's `.sha256` files, the Tor Project's `sha256sums-signed-build.txt` |
| Official GitHub releases | The SHA-256 digest GitHub's release API states for the developer's asset |
| No published checksum (Telegram, Firefox from Mozilla's archive, PPSSPP, RetroArch's buildbot) | Pinned at the first download (`tofu`); `uxdroid install` verifies the APK signature as Android does before anything runs |

Every APK runs only in the `sandbox` service: no network, a read-only system. `run.sh` builds uxdroid into its own target directory, `/cache/target-popular`, copies the arm64 engine (`libuxdroid_dynarmic.so`) out of the `translation` image, reads each APK's package, version and ABIs with `uxdroid inspect` in the sandbox, and runs `run_apps.sh` unchanged but for its build directory. Two passes run one after the other, so translated and native runs do not share the CPU: `native` (every app, x86_64 code when it has any) and `translated` (the arm64-v8a builds). Beside the runner, `sample_cpu.py` records each app's CPU time and resident memory every second.

## Output

In `--out` (default `~/uxdroid-samples/popular/out/<date>`): `report.tsv` (the manifest row, `run_apps.sh`'s columns, and `cpu_30s`, the CPU seconds the app used in its first 30 s, `cpu_run_s` and `rss_peak_mib`), `ranking.txt` (`corpus.py rank`; translated runs appear as `<package>@arm64`), `logs/<pass>/`, `shots/<pass>/`, `cpu/<pass>/`, `inspect.tsv`, `translator.txt` (engine image and hash) and `commit.txt` (source commit and uncommitted files).

The ABI cannot be forced: `uxdroid install` takes the host's ABI when the APK has it and arm64-v8a otherwise (`host_abi()` in `crates/apk`, docs/guide.md). Apps that ship both ABIs in one APK therefore run natively only; the translated runs are the developer's own arm64-only builds.

## Results

Run on 2026-11-07 on a 12-CPU, 30 GB x86_64 machine: source commit `4c51b76` plus the uncommitted translation work of that day, engine `libuxdroid_dynarmic.so` SHA-256 `5d53d23e…dc8cfa` from the `translation` image built 17:12, `--jobs 3 --wait 60`; 46 runs (37 native pass, 9 translated pass) in 13 minutes, downloads apart. One run per app; the apps below the second table were run twice more (`--wait 40`) to check the spread.

`first_frame_ms` counts from the framework's start (after the process and ART start) to the first frame posted. `CPU s, first 30 s` is the CPU time of the app's whole session in its first 30 s, `-` when it died before.

| App | Kind | Source | ABI run | First frame (ms) | Alive | Flat | Tap changed | CPU s, first 30 s | Peak RSS MiB | Framework | Blocker |
|---|---|---|---|---|---|---|---|---|---|---|---|
| aurora-store 4.8.4 | app | izzy | x86_64 | 215 | yes | no | yes | 0.3 | 179 | native | `-` |
| aves 1.15.9 | app | izzy | arm64-v8a (translated) | 1214 | yes | no | yes | 3.2 | 578 | flutter | `-` |
| bitwarden 2026.10.1 | app | direct | x86_64 | - | no | - | - | - | 159 | native | `exited (no error logged)` |
| brave 1.99.55 | app | direct | x86_64 | - | yes | - | - | 0.1 | 164 | native | `Caused by: java.lang.UnsatisfiedLinkError: libchrome.so: unsupported ELF: ELF TLS` |
| breezyweather 6.2.2 | app | izzy | x86_64 | - | yes | - | - | 0.2 | 162 | native | `Caused by: android.system.ErrnoException: statvfs failed: ENOENT (No such file or directory)` |
| comaps 2026.10.05-19 | app | izzy | arm64-v8a (translated) | 596 | no | - | - | - | 306 | native | `Fatal signal 6 (SIGABRT), code -6 (SI_TKILL)` |
| duckduckgo 5.301.1 | app | direct | x86_64 | - | no | - | - | - | - | native | `Caused by: java.lang.NullPointerException: Attempt to invoke virtual method 'java.lang.Class java.lang.Obje...` |
| element-x 26.10.1 | app | direct | x86_64 | - | no | - | - | - | 228 | native | `exited (no error logged)` |
| firefox 159.0.1 | app | direct | x86_64 | - | no | - | - | - | 211 | native | `uxdroid: sigaltstack is not audited for bionic compatibility yet` |
| jellyfin 2.7.3 | app | izzy | x86_64 | 134 | yes | no | - | 0.2 | 163 | native | `-` |
| k9mail 24.0 | app | direct | x86_64 | 197 | yes | - | - | 0.9 | 170 | native | `-` |
| keepassdx 4.5.8 | app | direct | x86_64 | 149 | yes | no | yes | 4.7 | 172 | native | `-` |
| kiwix 3.14.1 | app | direct | x86_64 | - | no | - | - | - | 167 | native | `Caused by: java.lang.UnsupportedOperationException` |
| libresudoku 2.0.2 | game | izzy | x86_64 | - | no | - | - | - | 147 | native | `Caused by: java.lang.NullPointerException: null cannot be cast to non-null type java.lang.Thread.UncaughtEx...` |
| lichess 0.30.3 | game | direct | x86_64 | 278 | no | - | - | - | 367 | flutter | `OpenjdkJvm.cc:343 System.exit called, status: 1` |
| luanti 5.18.0 | game | direct | x86_64 | 118 | yes | - | - | 0.8 | 154 | native | `-` |
| metrolist 13.7.0 | app | izzy | x86_64 | 167 | yes | no | yes | 0.5 | 182 | native | `-` |
| moonlight 12.2 | app | direct | x86_64 | 154 | yes | yes | no | 0.2 | 230 | native | `java.lang.RuntimeException: createContext failed: EGL_BAD_ALLOC` |
| mullvad 2026.12 | app | direct | x86_64 | 211 | yes | no | no | 0.3 | 165 | native | `-` |
| newpipe 0.29.1 | app | direct | x86_64 | - | no | - | - | - | - | native | `OpenjdkJvm.cc:343 System.exit called, status: 10` |
| nextcloud 35.0.2 | app | direct | x86_64 | 282 | no | no | no | 0.4 | 209 | native | `OpenjdkJvm.cc:343 System.exit called, status: 0` |
| obtainium 1.7.4 | app | izzy | arm64-v8a (translated) | 2316 | yes | no | no | 3.4 | 583 | flutter | `-` |
| organic-maps 2026.09.29-32-Web | app | direct | x86_64 | 136 | yes | no | - | 0.3 | 212 | native | `-` |
| pixelorama 1.2.4 | app | direct | arm64-v8a (translated) | 2184 | no | - | - | - | 1257 | godot | `Fatal signal 6 (SIGABRT), code -6 (SI_TKILL)` |
| ppsspp v1.20.4 | app | direct | x86_64 | - | yes | - | - | 0.2 | 180 | native | `Caused by: java.lang.NullPointerException: Attempt to read from field 'int android.content.pm.Configuration...` |
| protonvpn 5.20.98.0 | app | direct | x86_64 | 196 | yes | no | yes | 0.6 | 215 | native | `-` |
| retroarch 1.22.2_GIT | app | direct | x86_64 | - | yes | - | - | 0.2 | 172 | native | `java.lang.UnsatisfiedLinkError: Unable to load native library "/tmp/tmp.URAuciRbh1/data/data/com.retroarch/...` |
| shattered-pd 4.0.2 | game | direct | x86_64 | - | yes | - | - | 0.1 | 201 | libgdx | `java.lang.UnsatisfiedLinkError: cannot read /cache/target-popular/release/libsoundpool.so: No such file or ...` |
| signal 8.34.2 | app | direct | x86_64 | - | yes | - | - | 0.2 | 185 | native | `Caused by: java.lang.NullPointerException: Attempt to invoke virtual method 'java.lang.String org.signal.co...` |
| supertux 0.7.0 | game | direct | x86_64 | 146 | yes | yes | no | 0.2 | 174 | native | `flat screen (no error logged)` |
| supertuxkart 1.5 | game | direct | x86_64 | 121 | yes | no | no | 0.1 | 153 | sdl | `-` |
| telegram 13.2.0 | app | direct | x86_64 | - | yes | - | - | 0.6 | 212 | native | `Caused by: java.lang.NullPointerException: Attempt to invoke virtual method 'boolean java.security.KeyStore...` |
| thunder 0.8.6 | app | izzy | arm64-v8a (translated) | 2690 | yes | no | yes | 3.4 | 584 | flutter | `-` |
| tor-browser 16.0 | app | direct | x86_64 | 225 | no | - | - | - | 388 | native | `Fatal signal 11 (SIGSEGV), code 1 (SEGV_MAPERR) fault addr (nil)` |
| unciv 4.22.17 | game | direct | x86_64 | - | yes | - | - | 0.1 | 213 | libgdx | `java.lang.UnsatisfiedLinkError: cannot read /cache/target-popular/release/libsoundpool.so: No such file or ...` |
| vlc 3.7.1 | app | direct | x86_64 | 249 | yes | no | yes | 0.4 | 234 | native | `-` |
| ytdlnis 1.9.1.1 | app | izzy | arm64-v8a (translated) | - | - | - | - | - | - | - | `error: cannot translate com.deniscerri.ytdl: libaria2c.so needs libz.so.1, which is neither in the APK nor ...` |
| brave 1.99.55 | app | direct | arm64-v8a (translated) | - | yes | - | - | 0.2 | 173 | native | `Caused by: java.lang.UnsatisfiedLinkError: libchrome.so: unsupported ELF: ELF TLS` |
| element-x 26.10.1 | app | direct | arm64-v8a (translated) | - | no | - | - | - | 232 | native | `exited (no error logged)` |
| firefox 159.0.1 | app | direct | arm64-v8a (translated) | - | no | - | - | - | 272 | native | `Fatal signal 6 (SIGABRT), code -6 (SI_TKILL)` |
| kiwix 3.14.1 | app | direct | arm64-v8a (translated) | - | no | - | - | - | 178 | native | `Caused by: java.lang.UnsupportedOperationException` |
| luanti 5.18.0 | game | direct | arm64-v8a (translated) | 120 | yes | - | - | 0.8 | 154 | native | `-` |
| retroarch 1.22.2_GIT | app | direct | arm64-v8a (translated) | - | yes | - | - | 0.2 | 172 | native | `java.lang.UnsatisfiedLinkError: Unable to load native library "/tmp/tmp.BhCu7mTjOo/data/data/com.retroarch....` |
| supertux 0.7.0 | game | direct | arm64-v8a (translated) | 530 | yes | yes | no | 0.6 | 201 | native | `flat screen (no error logged)` |
| tor-browser 16.0 | app | direct | arm64-v8a (translated) | 215 | no | - | - | - | 342 | native | `Fatal signal 11 (SIGSEGV), code 1 (SEGV_MAPERR) fault addr (nil)` |
| vlc 3.7.1 | app | direct | arm64-v8a (translated) | 179 | yes | no | yes | 0.3 | 184 | native | `-` |

### Translated against native

Apps whose developers publish both an x86_64 and an arm64-v8a APK of the same version, each run three times (first frame in ms; median, then the three runs):

| App | Native x86_64 | Translated arm64 | Translated / native | CPU s, first 30 s (native, translated) | Peak RSS MiB (native, translated) |
|---|---|---|---|---|---|
| SuperTux 0.7.0 (SDL, native game) | 131 (146, 122, 131) | 473 (530, 473, 445) | 3.6x | 0.1, 0.5 | 174, 201 |
| Luanti 5.18.0 (native game) | 121 (118, 127, 121) | 132 (120, 132, 135) | 1.09x | 0.8, 0.8 | 154, 155 |
| VLC 3.7.1 | 188 (249, 181, 188) | 179 (179, 171, 192) | 0.95x (within the spread) | 0.3, 0.3 | 234, 184 |
| Tor Browser 16.0 | 214 (225, 214, 214) | 210 (215, 196, 210) | 0.98x (within the spread); both crash later | - | 384, 342 |

Brave, Firefox, Kiwix, Element X and RetroArch were also run both ways but reach no first frame either way (or a different blocker each way), so they give no timing.

Flutter, the heaviest translated startup measured: three arm64-only Flutter apps against the one x86_64 Flutter app of the corpus (different apps, so this is an indication, not a paired measurement):

| App | ABI | First frame ms (3 runs) | CPU s, first 30 s | Peak RSS MiB |
|---|---|---|---|---|
| Aves 1.15.9 | arm64, translated | 1214, 1235, 1223 | 3.2 | 578 |
| Obtainium 1.7.4 | arm64, translated | 2316, 2356, 2351 | 3.4 | 583-614 |
| Thunder 0.8.6 | arm64, translated | 2690, 2727, 2726 | 3.4-3.5 | 584-589 |
| Lichess 0.30.3 | x86_64, native | 278, 284, 298 | - (exits before 30 s) | 366-370 |

Translated Flutter apps take 1.2-2.7 s to their first frame against 0.28 s for native Flutter, use 3.2-3.5 CPU seconds in their first 30 s against 0.1-0.9 for the other apps that stay alive (KeePassDX, 4.5, aside), and about 580 MiB against 370. Every other measured app spends little time in native code before its first frame, which is why VLC, Luanti and Tor show no translation cost there.

### Blockers

`ranking.txt` of the run, by apps stopped (translated runs marked `@arm64`): `Fatal signal 6` 3 (CoMaps, Pixelorama, Firefox@arm64); `System.exit called` 3 (Nextcloud, Lichess, NewPipe); `exited (no error logged)` 3 (Bitwarden, Element X both ways); `libchrome.so: unsupported ELF: ELF TLS` 2 (Brave both ways); `UnsupportedOperationException` 2 (Kiwix both ways); `SIGSEGV` 2 (Tor Browser both ways); `flat screen` 2 (SuperTux both ways); NativeActivity's `loadNativeCode` unimplemented 2 (RetroArch both ways); `cannot read .../libsoundpool.so` 2 (Shattered Pixel Dungeon, Unciv: uxdroid ships no `libsoundpool.so` for `android.media.SoundPool`); then one app each: a null `KeyStore` (Signal, Telegram), `statvfs` ENOENT (Breezy Weather), `ConfigurationInfo` null (PPSSPP), EGL_BAD_ALLOC (Moonlight), `sigaltstack is not audited` (Firefox x86_64), `libz.so.1` not provided to arm64 code (YTDLnis), and others.

Blockers of translated runs only, from their logs: Firefox arm64, `JNIEnv::CallObjectMethod with a method ID not made by GetMethodID or GetStaticMethodID, whose descriptor the bridge cannot know`; CoMaps, `__strncpy_chk2 has no guest signature yet, so arm64 code cannot call it`; Pixelorama (Godot), `guest code stopped: an unallocated encoding` (an instruction the engine does not decode); YTDLnis, `libaria2c.so needs libz.so.1, which is neither in the APK nor a system library uxdroid provides`.

In all: 45 of 46 runs installed, 25 posted a first frame, 29 were alive after 60 s, 14 showed a screen that was not flat and 8 changed their UI on a tap. Passed every check, the tap included: Aurora Store, Aves, KeePassDX, Metrolist, Proton VPN, Thunder, VLC (both ways); Jellyfin, K-9 Mail, Luanti (both ways), Mullvad, Obtainium, Organic Maps and SuperTuxKart posted a frame and stayed alive with no blocker.

### Games from GitHub releases

24 games whose developers attach APKs to their own GitHub releases, found with `gh api search/repositories` (topics such as `android-game`, `godot-game`, `libgdx`, `unity3d`, `love2d`, `sdl2`, `raylib`, `defold`) and then by which repos' latest releases ship an `.apk` with a GitHub digest. Run the same day with the same build and engine (`--only <the game ids>`, `--wait 60`); the four games built per ABI ran three times. Cube Merge 3D (Unity) ships `armeabi-v7a` code only and is skipped, as `uxdroid install` refuses it. Frame = first frame in ms, CPU = CPU seconds in the first 30 s.

| Game (repo, tag) | Engine | ABI run | Frame | Alive | CPU | Blocker |
|---|---|---|---|---|---|---|
| Card Game Simulator (finol-digital/Card-Game-Simulator v1.170) | unity | arm64 translated | 364 | yes | 0.4 | - (tap changed nothing) |
| Glassline (MoonCellar-Team/Glassline v1.4.2) | sdl | arm64 translated | 275 | yes | 0.3 | flat screen |
| Open Surge (alemart/opensurge v0.6.1.3) | native (Allegro) | arm64 translated | 203 | no | - | `Aborting because: Field not found: fd:I` |
| ROTA (HarmonyHoney/ROTA 2026.03.12) | godot | arm64 translated | - | no | - | NPE `ConfigurationInfo.reqGlEsVersion` |
| Tiny Crate (HarmonyHoney/tiny_crate 2026.03.12) | godot | arm64 translated | - | no | - | same |
| Turbo Fat (Poobslag/turbofat v1.1215) | godot | arm64 translated | - | no | - | same |
| Block Drop (brandonp2412/BlockDrop v1.0.74) | flutter | both | 639 / 2066 | yes / yes | 1.4 / 3.6 | - |
| Water Sort (sidhant947/water-sort V1.0.24) | flutter | both | 639 / 1948 | yes / yes | 0.8 / 2.2 | - |
| oSBM (RohanBhattacharyya/oSBM v1.1) | native | both | 157 / 450 | no / no | - | `ASensorManager_getInstance is not implemented yet`, then SIGABRT |
| VCMI (vcmi/vcmi 1.7.5) | qt | both | 192 / 135 | no / yes | - / 0.2 | native: `__res_init is not audited`; translated: - (tap not tried) |
| Principia (Bithack/principia 2026.10.22) | native | x86_64 | 124 | yes | 77.0 | - (tap changed nothing) |
| Braincup (SimonSchubert/Braincup v3.8.1) | native | x86_64 | 294 | yes | 6.4 | - |
| Cube Run (Eve-146T/cube-run v2.8) | libgdx | x86_64 | 202 | yes | 82.6 | - (tap changed nothing) |
| SuDokuL (Mode8fx/SuDokuL v1.5) | sdl | x86_64 | 175 | yes | 0.2 | flat screen |
| Alex vs Bus (M374LX/alexvsbus 2025.06.16.0) | native | x86_64 | - | no | - | NativeActivity `loadNativeCode` unimplemented |
| Pixel Wheels (agateau/pixelwheels 1.0.1) | libgdx | x86_64 | - | yes | 0.1 | no `libsoundpool.so` |
| Burger Party (agateau/burgerparty 1.4.6) | libgdx | x86_64 | - | yes | 0.1 | no `libsoundpool.so` |
| Feudal Tactics (Sesu8642/FeudalTactics v1.5.5) | libgdx | x86_64 | - | yes | 0.1 | no `libsoundpool.so` |
| Bird Hunt (jojczak/BirdHunt v1.4.4) | libgdx | x86_64 | - | yes | 0.1 | no `libsoundpool.so` |
| Darkest Pixel Dungeon (egoal/darkest-pixel-dungeon v0.8.0) | libgdx | x86_64 | - | yes | 0.1 | no `libsoundpool.so` |
| Astro Loop (PubDeer/astro-loop 1.5) | java | - | - | yes | 0.2 | no `libsoundpool.so` |
| Gauguin (meikpiep/gauguin v0.53.3) | java | - | 296 | yes | 1.0 | - |
| Remixed Dungeon (NYRDS/remixed-dungeon 32.4.beta.17) | java | - | - | no | - | NPE `Attempt to get length of null array` |

Paired runs (median of 3, first frame ms native / translated): Block Drop 639 / 2066 (3.2x; runs 589, 639, 642 / 2062, 2066, 2077), Water Sort 639 / 1948 (3.0x; 655, 639, 639 / 1943, 1952, 1948), oSBM 157 / 450 (2.9x; 154, 157, 166 / 454, 450, 444). Translated Flutter also uses 2.6-2.8 times the CPU in its first 30 s and about 150 MiB more (587 against 432 MiB, 544 against 398). These two are the first same-app measurements of Flutter's translated startup: about three times native. VCMI runs further translated than native, because the arm64 path does not reach the unaudited `__res_init`. Principia and Cube Run use 2.6-2.8 CPUs while alive, rendering continuously on the software renderer.

Blockers of the games, by games stopped: `libsoundpool.so` missing 6 (every libGDX game but Cube Run, plus Astro Loop; with Shattered Pixel Dungeon and Unciv, 8 in the corpus), `ConfigurationInfo.reqGlEsVersion` null 3 (every Godot game; with PPSSPP, 4), NativeActivity `loadNativeCode` 1 (with RetroArch, 3 runs), flat screen 2, then one each.

## Not measured

- No competitor. The performance rule asks for the competitors that can run here; none was run on these apps. Waydroid and Android Translation Layer need a host setup (kernel binder, LXC, or ATL's own build and install) the sandbox does not have; Google's Android emulator with its arm64 translation needs KVM and a Google system image, and QEMU user cannot run an Android app. The translator alone is compared with QEMU user in `benches/translation`.
- No frame rate or frame time: uxdroid logs only the first frame, and the runner takes screenshots only. Games were not played; one tap per app.
- Unavailable sources: itch.io answers automated requests with HTTP 403, so no itch.io game is in the corpus. GitHub gave one Unity game that runs here (Card Game Simulator; Cube Merge 3D is `armeabi-v7a` only), no Unreal game of the developer's own (the Unreal APKs found are ports needing a commercial game's data), and no Defold, LÖVE or raylib game with a digest. GitHub release assets uploaded before GitHub started stating digests (Destination Sol, Brogue CE, Ball2Box, Tetris and others) were left out, as were ports of commercial games (Dead Space, Zuma, Sonic R, System Shock, Heroes III clients) and online clients. Mindustry's recent GitHub releases ship no APK, PPSSPP's and RetroArch's sites and Mozilla's archive publish no checksum (pinned at first download), Termux's GitHub APKs are debug builds without a digest and were left out, as were Cataclysm DDA's (prereleases only). Apps that need Google Play services, Play Integrity or Widevine were not chosen.
- The ABI cannot be forced, so apps that ship both ABIs in one APK (Signal, Telegram, Bitwarden, NewPipe and most others) ran natively only.
- One run per app except the paired apps and games and the Flutter apps (three runs); install time is not reported.
