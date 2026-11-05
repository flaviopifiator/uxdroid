#!/bin/bash
# The GPU path end to end: the NDK sample's EGL frame drawn on the host GPU through GBM, handed to a
# compositor that renders with GL as a dmabuf, and read back from the compositor's screenshot with
# its white quarter at the top left. Needs the GPU, so it runs in dev-gpu:
#
#   docker compose --profile gpu run --rm -T dev-gpu bash /work/runtime/gpu/gpu_smoke.sh
set -u -o pipefail
RENDERER=gl DMABUF=1 exec bash /work/runtime/framework/ndk_smoke.sh
