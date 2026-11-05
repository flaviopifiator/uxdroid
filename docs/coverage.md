# NDK coverage audit

How much of each Android system library uxdroid binds, measured without running an app. The official export lists are the NDK's stub libraries for the highest API level it ships (`sysroot/usr/lib/<triple>/<API>/lib*.so`), which the NDK generates from AOSP's `.map.txt` files. Every defined dynamic symbol of every stub library is classified twice:

- **x86_64-bridge**: as the loader binds it for an x86_64 library that needs that library (`uxdroid_bridge::host_binding`): its signature table, then liblog, then the shims and the audited glibc symbols, then the stub naming a function on that library's list.
- **arm64-translator**: as the install-time planner binds it for an arm64-v8a library of no app that needs only that library (`uxdroid_bridge::guest::plan::plan`).

`libc++.so` is skipped, as apps ship their own C++ runtime.

## Running it

```sh
docker compose run --rm -v "$PWD/out:/out" dev \
  cargo run -q -p uxdroid-cli --example coverage -- /opt/android-ndk /out/coverage.tsv > out/coverage.txt
```

Both arguments are optional: the NDK root defaults to `/opt/android-ndk`, and without a TSV path only the report is printed. The example needs an x86_64 glibc host, the only one with both paths. It runs no app code; `dev` is fine.

## Output

For each path, one row per library:

| Column | Meaning |
|--------|---------|
| `total` | Exported symbols in the stub library |
| `funcs`, `data` | Of them, functions (`T`, `W`, `i`) and data objects (`D`, `B`, `R`, `V`) |
| `resolved` | Bound to a table, a shim, audited glibc (bridge) or the guest libc (translator) |
| `trap` | Bound to a stub that stops the program naming the function when called: an unaudited glibc function, an incompatible ABI, or a guest call with no signature yet |
| `unimpl` | A function nothing above has, bound to the stub that stops the program naming the library and the function when called: uxdroid does not have it yet. Loads and installs warn, naming these per library (`uxdroid_bridge::unimplemented`, from the same NDK stubs through `crates/bridge/src/ndk_exports.py`) |
| `missing` | Nothing binds: a load of a library that imports it fails, or a weak import is null. Only data stays here, as it cannot trap |
| `resolved%` | `resolved` over `total` |

Then one line per library listing its missing names, data marked `(data)`.

The TSV has one row per symbol and path: `path`, `api`, `library`, `symbol`, `kind` (`function` or `data`), `status` (`resolved`, `trap`, `unimplemented`, `missing`) and `detail`, the reason the classifier gives, such as `the library is not provided` or `glibc has it, not audited for bionic`.

Bridge rows for tables that look up a host library, such as EGL's and GLES's, depend on the host's libraries: a name the host's Mesa lacks binds to the stub naming it, as the loader binds it. The translator's planner does not ask the host, so the same names count as resolved there.

## Results

NDK r30, API 37, every symbol of every stub library (4774: 4490 functions, 284 data):

| Path | resolved | trap | unimpl | missing |
|------|---------:|-----:|-------:|--------:|
| x86_64-bridge, before the stubs (`f537ea1`) | 2077 (43.5%) | 725 | - | 1972 |
| x86_64-bridge, unimplemented stubs (`1239a1c`) | 2079 (43.5%) | 725 | 1892 | 78 |
| x86_64-bridge | 2138 (44.8%) | 719 | 1850 | 67 |
| arm64-translator, before the stubs | 2081 (43.6%) | 798 | - | 1895 |
| arm64-translator, unimplemented stubs | 2083 (43.6%) | 737 | 1875 | 79 |
| arm64-translator | 2143 (44.9%) | 731 | 1833 | 67 |

The two resolved functions are the fortified `__pwrite_chk` and `__pwrite64_chk`, which LinkedIn's `libLMDBAndroid.so` calls. The arm64 trap count fell by 61 because the planner used to count a signed libc function the host lacks, such as `fgetln`, as an unaudited host function, while the load found nothing to call and failed; both now bind it to the stub naming `libc.so`, so the two paths agree on libc (149 each).

Every function now loads. bionic's data symbols are implemented on both paths (`crates/bridge/src/shims/data.rs`), so what is missing is the data of two libraries uxdroid has no table for: `libOpenMAXAL`'s interface IDs (41) and `libmediandk`'s newer keys and features (26).

The last row adds libandroid's `ATrace_*` (no-ops, tracing off), libc's fdsan, API-level, property, FORTIFY (`__memchr_chk`, `__memrchr_chk`, `__sendto_chk`, `__ppoll_chk`, `__ppoll64_chk`), `sigset64_t`, `CLOCK_MONOTONIC` wait, `__get_h_errno`, `setprogname`, `fpurge`, `fgetln` and `free_sized` calls, and the data symbols. The arm64 guest table is generated at API 37 with `android/` headers, which signs fdsan and the API 36 and 37 calls: libc resolves 784 of 1445 symbols for x86_64 code and 774 for arm64 code, against 734 and 723 before.
