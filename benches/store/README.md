# Store benchmark

`bench.sh` times `uxdroid search` and `uxdroid install <package>` on a snapshot of F-Droid's official repository served over loopback by `serve.py`, so the numbers hold uxdroid's own work and not the Internet: fetching `entry.jar` and the index over HTTP, verifying the JAR signature and the index's SHA-256, keeping the fields uxdroid reads, searching, and downloading, checking and installing an APK. It runs offline in the sandbox, since the snapshot's APK is untrusted. The snapshot is `entry.jar` and `index-v2.json` fetched together from https://f-droid.org/repo/ and the APK to install.

```sh
UXDROID_SAMPLES=<snapshot> docker compose run --rm -T -e BASE=<older uxdroid> sandbox bash /work/benches/store/bench.sh
```

## Results

2026-11-04, F-Droid's index of that day (4,957 apps, 15,203 versions, 71,657,161 bytes), Markor 2026.10.0 (11,849,016 bytes), release build, the framework runtime of `feat/app-stores`, 5 runs each (20 for a cached search), medians:

| Workload | Time |
|----------|------|
| `search markor`, no cached index: fetch, verify and keep the whole index | 0.186 s |
| `search markor`, cached index | 0.071 s |
| `install net.gsantner.markor` from the store, index cached: read the index, download, check, install, compile | 0.326 s |
| `install net.gsantner.markor_164.apk`, the same APK as a file | 0.249 s |
| The same with the build before the store (`95345c3`), which did not copy the file it verified | 0.241 s |

A cached search is the cost of reading the kept index, 10.5 MB of JSON; an install from the store adds that read, the loopback download and its SHA-256 to an install from a file. Copying the file given before verifying it, so the bytes verified are the bytes installed, costs 8 ms for this 12 MB APK.

Over the Internet, from the same machine to f-droid.org on 2026-11-04, a search with no cached index took 51.0, 15.2 and 61.8 s, almost all of it the 23 MB compressed transfer (curl took 22.7 s for the same file minutes earlier), and an install of Markor 25.8, 9.5 and 13.7 s; a cached search took 0.071 s, as over loopback.

## Not measured

- No competitor ran here. F-Droid's own client is an Android app, and `fdroidcl`, the one F-Droid command line client known, could not be fetched: network access was granted only to F-Droid's repository, and its source is on other hosts.
- Refreshing through a diff instead of the whole index, which the tests check for correctness but not for time; F-Droid's newest diffs are smaller than the index (the oldest offered, from 30 days back, is 12.7 MB).
- Memory: `/usr/bin/time` is not in the image.
- A cold page cache: dropping it needs privileges the containers do not have.

## Google Play

`play.sh` times `uxdroid search` and `uxdroid install <package> --from play` against the real Google Play, anonymously. It needs the network, so it runs in a copy of the sandbox with the project's default network instead of none (a compose override such as `services: {sandbox-net: {extends: {file: compose.yaml, service: sandbox}, network_mode: !reset null, networks: [default]}}`); the default bridge network resolved names 5 s slower here. No app code runs: the install compiles the app and parses its manifest, and nothing starts it. `DATA` can name a data directory already signed in, since every request costs a shared account a little.

2026-11-05, release build, from the same machine as above, anonymous, 5 runs each, medians:

| Workload | Time |
|----------|------|
| `search markor`, signed in | 0.735 s (0.714 to 0.959) |
| `install net.gsantner.markor --from play`: details, acquire, delivery, download 11,980,325 bytes, check, install, compile | 2.013 s (1.939 to 2.237) |
| `install org.thoughtcrime.securesms --from play`, once, no framework: 3 files, 121,812,046 bytes | 8.4 s |

The F-Droid figures above were measured the day before over the same connection: Markor's install from F-Droid took 9.5 to 25.8 s and a search with no cached index 15 to 62 s, both dominated by f-droid.org's transfer, while a search of the cached index took 0.071 s.

Not measured: Aurora Store's own flow, an Android app that would need the network to run here, where untrusted apps run offline; F-Droid on the same day, since only Google Play's hosts and the token dispenser were open for this run; Play's compressed downloads, which uxdroid does not use yet.
