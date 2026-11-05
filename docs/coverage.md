# NDK coverage audit

How much of each Android system library uxdroid binds, measured without running an app. The official export lists are the NDK's stub libraries for the highest API level it ships (`sysroot/usr/lib/<triple>/<API>/lib*.so`), which the NDK generates from AOSP's `.map.txt` files. Every defined dynamic symbol of every stub library is classified twice:

- **x86_64-bridge**: as the loader binds it for an x86_64 library that needs that library (`uxdroid_bridge::host_binding`): its signature table, then liblog, then the shims and the audited glibc symbols.
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
| `trap` | Bound to a stub that stops the program naming the function when called |
| `missing` | Nothing binds: a load of a library that imports it fails, or a weak import is null |
| `resolved%` | `resolved` over `total` |

Then one line per library listing its missing names, data marked `(data)`.

The TSV has one row per symbol and path: `path`, `api`, `library`, `symbol`, `kind` (`function` or `data`), `status` (`resolved`, `trap`, `missing`) and `detail`, the reason the classifier gives, such as `the library is not provided` or `glibc has it, not audited for bionic`.

Bridge rows for tables that look up a host library, such as EGL's and GLES's, depend on the host's libraries: a name the host's Mesa lacks counts as missing, `its table's host library lacks it`. The translator's planner does not ask the host, so the same names count as resolved there.
