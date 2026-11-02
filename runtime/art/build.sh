#!/bin/bash
# Builds ART for the host inside the art-builder service. JOBS sets the parallelism.
set -u
cd /aosp

MIN_FREE_KIB=$((60 * 1024 * 1024))
JOBS="${JOBS:-6}"

# A full disk can harm the whole machine, so the build stops first. Its output stays and resumes.
(
    while sleep 30; do
        free_kib=$(df -k --output=avail /aosp | tail -1)
        if [ "$free_kib" -lt "$MIN_FREE_KIB" ]; then
            echo "disk guard: less than 60 GiB free, stopping the build" >&2
            pkill -f soong_ui
            pkill ninja
            exit 1
        fi
    done
) &
guard=$!

export SOONG_ALLOW_MISSING_DEPENDENCIES=true TARGET_BUILD_UNBUNDLED=true
set +u
source build/envsetup.sh
lunch armv8-trunk_staging-eng
set -u
build/soong/soong_ui.bash --make-mode -j"$JOBS" build-art-host
status=$?

kill "$guard" 2>/dev/null
exit "$status"
