# Shared by the smoke scripts: RELEASE, where cargo's release build of uxdroid is, for the CPU cargo
# builds for (CARGO_BUILD_TARGET, set by the dev-arm64 service) or else this machine's.
RELEASE="$(cargo metadata -q --format-version 1 --no-deps --manifest-path /work/Cargo.toml \
    | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')"
RELEASE="$RELEASE${CARGO_BUILD_TARGET:+/$CARGO_BUILD_TARGET}/release"
