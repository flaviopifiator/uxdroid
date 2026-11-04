#!/bin/bash
# Lists the EGL devices a container sees and the GL renderer each gives a GLES 2 context. Run it in
# `dev` to see the software fallback, and in `dev-gpu` (compose profile gpu) to see the host's GPU:
#
#   docker compose run --rm -T dev bash /work/runtime/gpu/gpu_probe.sh
#   docker compose --profile gpu run --rm -T dev-gpu bash /work/runtime/gpu/gpu_probe.sh
#
# With EXPECT=<text>, it fails unless some renderer line contains that text.
set -u -o pipefail
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
cc -O1 -o "$WORK/egl_probe" /work/runtime/gpu/egl_probe.c -ldl || { echo "FAIL: build"; exit 1; }
"$WORK/egl_probe" | tee "$WORK/out" || { echo "FAIL: the probe found no usable EGL device"; exit 1; }
if [ -n "${EXPECT:-}" ]; then
    grep -q -F -- "$EXPECT" "$WORK/out" || { echo "FAIL: no renderer with \"$EXPECT\""; exit 1; }
    echo "ok: a GLES 2 context renders on \"$EXPECT\""
fi
