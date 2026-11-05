#!/bin/bash
# Passes framework Parcelables through android.os.Parcel, whose natives are in crates/system. Runs in
# the dev service; LIBRARY may name a libuxdroid_system.so to test instead of building one.
set -u -o pipefail
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
fail() { echo "FAIL: $*"; tail -"${TAIL:-25}" "$WORK/out" | cut -c1-220; exit 1; }

if [ -z "${LIBRARY:-}" ]; then
    cargo build -q --release -p uxdroid-system --manifest-path /work/Cargo.toml || fail "build"
    LIBRARY="$(cargo metadata -q --format-version 1 --no-deps --manifest-path /work/Cargo.toml \
        | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')/release/libuxdroid_system.so"
fi
source /work/runtime/art/run.sh
export ANDROID_DATA="$WORK/data"
mkdir -p "$ANDROID_DATA/dalvik-cache/x86_64"
CP="$RUNTIME/framework/framework.jar:$RUNTIME/framework/uxdroid-system.jar:$RUNTIME/samples/framework/parcel.jar"
timeout 60 "$RUNTIME/bin/dalvikvm64" "${ART_FLAGS[@]}" -cp "$CP" ParcelSmoke "$RUNTIME" "$LIBRARY" \
    >"$WORK/out" 2>&1 || fail "exit $?"

expect() { grep -q -x -F "$1" "$WORK/out" || fail "$2: expected \"$1\""; }
BUNDLE="valor 7 [1, 2, 3] [a, b] Rect(1, 2 - 3, 4) [Point(5, 6), Point(7, 8)] 42"
expect "primitives -7 1099511627776 1.5 -2.25 héllo ☺ 😀 ascii null [1, 2, 3] [4, 5] avail 0" \
    "values did not survive marshall and unmarshall"
expect "bundle $BUNDLE" "a Bundle with nested Parcelables did not round-trip"
expect "bundle copy $BUNDLE" "copying a parcelled Bundle (appendFrom) lost its contents"
expect "intent dev.uxdroid.ACTION dev.uxdroid/.Target 10000000 x $BUNDLE" "an Intent with extras did not round-trip"
expect "fds true" "a ParcelFileDescriptor did not write a file descriptor"
expect "marshall refused: Tried to marshall a Parcel that contained Binder objects." \
    "a parcel with objects was marshalled"
expect "pipe por el pipe" "the ParcelFileDescriptor read back is not the same pipe"
expect "binders true" "a binder was not written"
expect "binder same true" "a binder read back is not the same object"
expect "aidl result 3 $BUNDLE" "the framework's AIDL proxy did not reach its stub through parcels"
expect "aidl refused: Binder invocation to an incorrect interface" "enforceInterface accepted a wrong token"
expect "bitmap 3x2 ARGB_8888 240 ff112233 ff00ff00 0 mutable true same true" \
    "a Bitmap in a Bundle did not round-trip"
expect "bitmap unpremultiplied 40ff8020 premultiplied false" "an unpremultiplied Bitmap changed on the way"
expect "bitmap forged refused: Failed to unparcel Bitmap" "a forged Bitmap header was not refused"
expect "motion refused: android.view.MotionEvent.nativeWriteToParcel: AOSP's host library cannot use uxdroid's parcels" \
    "a host native was handed a parcel it cannot read"
expect "allocations true" "Parcel.getGlobalAllocCount failed"
if grep -q "unimplemented native android.os.Parcel" "$WORK/out"; then fail "a Parcel native has a default body"; fi
grep -E "^(primitives|bundle|intent|fds|marshall|pipe|binder|aidl|bitmap|motion) " "$WORK/out" | cut -c1-200
echo "ok: Bundles, Intents, file descriptors, binders and AIDL calls passed through uxdroid's Parcel"
