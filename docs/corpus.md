# Running the F-Droid catalog

`runtime/corpus/nightly.sh` runs F-Droid's apps unattended, one after another, and ranks what stops them, so a fix can go where it unblocks the most apps.

```sh
runtime/corpus/nightly.sh
```

That runs the whole catalog. A sample is `--sample 150 --seed 2026`; the other options are `--jobs <n>` (sandbox containers at once, default 3), `--wait <s>` (seconds each app runs before the checks, default 60), `--batch <n>` (apps downloaded at a time, default 4 per job), `--catalog <dir>` and `--out <dir>`, and `--keep` to keep the APKs. To run it every night at 1:00, add to `crontab -e`:

```cron
0 1 * * * cd /path/to/uxdroid && runtime/corpus/nightly.sh >/dev/null 2>&1
```

## What it does

1. Builds `uxdroid` (release) in `dev` and copies the arm64 translator, `libuxdroid_dynarmic.so`, out of the `translation` image, so `arm64-v8a` code runs. Without that image, apps with only `arm64-v8a` code fail to load it.
2. Refreshes the index with uxdroid's own store, `uxdroid search --refresh`, in `dev`: it checks `entry.jar` against F-Droid's pinned signing key and the index against the SHA-256 the entry signs, and only then keeps it. `corpus.py select` reads that verified copy and takes each app's newest stable release (no release channel) whose SDK range holds 36 and whose native code, if it has any, includes `x86_64` (preferred) or `arm64-v8a`. In November 2026 that is 4933 of F-Droid's 4957 apps; 22 have native code for other ABIs only, 2 an SDK range without 36.
3. In batches: downloads the APKs on the host with `corpus.py download`, keeping a file only when its size and SHA-256 are the index's (it never opens the APK), while the previous batch runs; splits the batch among `--jobs` sandbox containers; and in each, `run_apps.sh` installs and runs the apps one at a time. Untrusted code runs only there: no network, a read-only system. The APKs are deleted after their batch unless `--keep` is given.
4. After every batch, ranks the blockers into `ranking.txt`.

Each app in the sandbox: `uxdroid install`, then `uxdroid service run` under a headless compositor for `--wait` seconds. If it is still alive, a screenshot (the flat check), `ui`, a second screenshot a second later, a tap, and two seconds later `ui` and a screenshot again (the tap check).

It resumes: run it again with the same `--out` and it skips the apps `report.tsv` already has. The selection is kept in `apps.tsv`, so a resumed run does not change apps when the index does. A failed download is listed in `download-errors.tsv` and tried again by the next run.

## Output

In `--out` (default `<catalog>-out/<date>`; the catalog defaults to `~/uxdroid-samples/fdroid-catalog`): `report.tsv`, `ranking.txt`, `logs/<package>.log` (install and run output), `shots/<package>.png` (the first screenshot), `apps.tsv` and `run.log`.

`report.tsv` has one row per app:

| Column | Meaning |
|--------|---------|
| `package` | Package name |
| `version` | `versionCode` run |
| `abi` | The native code it ran: `x86_64`, `arm64-v8a` (translated) or `none` |
| `installed` | `uxdroid install` prepared it |
| `first_frame_ms` | Milliseconds from the framework's start to the first frame posted, or `-` if none was |
| `alive` | Still running after `--wait` seconds and the checks; `-` if not installed |
| `flat` | `yes` when one color covers at least 98% of the screenshot's pixels (a blank or splash screen); `-` without a screenshot |
| `tap_changed` | `yes` when the tap changed the `ui` tree (snapshot and node numbers aside), or changed the screen when the screen was still before it; `-` when it was not tapped |
| `blocker` | What stopped the app, or `-` when it passed: installed, a first frame, alive, not flat |

The tap goes to the center of the first visible clickable node with no clickable node under it, in the topmost window; with none, as in a game's `SurfaceView`, to the window's center. A tap that changes nothing is not a blocker, since some first screens have nothing to tap.

The blocker is the line that best names the failure: for a Java crash, its last `Caused by:` (the root cause); then uxdroid's refusals (`not audited`, `no shim`, `unresolved symbols`, `which was not found`), a native `Fatal signal`, a launch error, any `Caused by:`; for a failed install, its error. With nothing logged, it names the check that failed, such as `exited (no error logged)`.

`ranking.txt` counts each outcome, then groups the blockers by signature, with addresses, pids, thread names, paths and numbers replaced, and lists them by how many apps each stops:

```text
Caused by: java.security.KeyStoreException: AndroidKeyStore not found: 3 apps (org.b, org.c, org.f)
```

## How long it takes

Measured on a 12-CPU, 30 GB machine, a 150-app sample with 3 jobs and 60 s each took 51 minutes, about 176 apps an hour, downloads included: apps that crash at once take seconds, apps that stay alive about 70 s plus their install. At that rate the whole catalog of about 4900 apps takes about 28 hours, so an 8-hour night covers about 1400 apps with 3 jobs; a resumed run continues where the last one stopped. More jobs fit: a sandbox container held 40 to 360 MiB while its app ran.

## Tests

```sh
docker compose run --rm dev python3 runtime/corpus/test_corpus.py
```

They cover the index selection, the PNG decoder and flat check, the choice of tap target, the UI comparison, and blocker extraction, normalization and ranking, with fixtures in `runtime/corpus/fixtures`. The containers' Python is python3-minimal, so `corpus.py` and its tests use no `unittest` or `urllib`; the download runs in the host's Python.

## Not yet

Only apps whose newest release fits are run, not an older one that would. One screen and one tap per app: no logins, permissions dialogs or onboarding flows.
