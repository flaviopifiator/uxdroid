#!/bin/bash
# Prints the key of the runtime the checked-out commit builds: a hash of every repository file the
# runtime's build reads (runtime/art, runtime/framework, runtime/keys, platform/java and system/java;
# AOSP's own sources are pinned inside runtime/art). packaging/publish_runtime.sh tags the runtime's
# image with it and CI looks the image up by it, so a release never packages a runtime built from
# other sources: packaging/runtime_key.sh [commit]
set -eu -o pipefail

PATHS=(runtime/art runtime/framework runtime/keys platform/java system/java)
git -C "$(dirname "$0")/.." ls-tree -r "${1:-HEAD}" -- "${PATHS[@]}" | sha256sum | cut -c1-16
