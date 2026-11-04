#!/bin/bash
# RustDesk's arm64-v8a build to its first Flutter frame, its native code translated on x86_64. The
# same stages and screenshot check as rustdesk_smoke.sh, plus the install's translation plan. The
# APK holds only arm64 libraries (made from the universal one by dropping the other lib/ folders),
# so the install cannot pick native code. Untrusted, so sandbox only:
#
#   docker compose run --rm dev cargo build --release -p uxdroid-system -p uxdroid-nativebridge -p uxdroid-cli
#   UXDROID_SAMPLES=<directory with the APK> docker compose run --rm translation-sandbox ...
#
# The engine is found through UXDROID_TRANSLATOR, as in the translation image. The APK is
# /samples/rustdesk-1.5.1-arm64-only.apk unless RUSTDESK_APK names another.
set -u -o pipefail
export RUSTDESK_APK="${RUSTDESK_APK:-/samples/rustdesk-1.5.1-arm64-only.apk}"
export RUSTDESK_ABI=arm64-v8a
exec bash "$(dirname "$0")/rustdesk_smoke.sh" "$@"
