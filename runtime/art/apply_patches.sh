#!/bin/bash
# Applies uxdroid's small changes to the AOSP sources. Safe to run again. Fails on any mismatch.
set -eu -o pipefail
apply() {
    local tree=$1 patch=$2
    if git -C "$tree" apply --reverse --check "$patch" 2>/dev/null; then
        echo "already applied: $(basename "$patch")"
    else
        git -C "$tree" apply "$patch"
        echo "applied: $(basename "$patch")"
    fi
}
for patch in /work/runtime/art/patches/art-*.patch; do
    apply /aosp/art "$patch"
done
