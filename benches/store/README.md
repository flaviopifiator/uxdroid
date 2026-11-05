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
