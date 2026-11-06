#!/bin/bash
# Builds the runtime for /usr/lib/uxdroid/runtime and pushes it to GHCR as <image>:<key>-<arch>, where
# CI's package job finds it and packages it as uxdroid-runtime (packaging/runtime_key.sh names the key).
# Building it takes AOSP's trees, so this runs where they are, logged in to ghcr.io with a token that
# can write packages; docs/guide.md (Packages) has the setup: packaging/publish_runtime.sh amd64|arm64
set -eu -o pipefail

ARCH=${1:?usage: packaging/publish_runtime.sh amd64|arm64}
IMAGE=${IMAGE:-ghcr.io/flaviopifiator/uxdroid-runtime}
SOURCE=${SOURCE:-https://github.com/flaviopifiator/uxdroid}
cd "$(dirname "$0")/.."
# Each step runs where ART's build for that CPU is: arm64 links against android-16's trees.
case "$ARCH" in
    amd64) ISA=x86_64 SERVICES=(art-builder art-builder art-builder art-builder art-builder) ;;
    arm64) ISA=arm64 SERVICES=(android16-builder art-builder art-builder android16-builder art-builder) ;;
    *) echo "error: unknown architecture $ARCH, expected amd64 or arm64" >&2; exit 2 ;;
esac
STEPS=(art/package.sh art/compile.sh art/build_platform.sh framework/build.sh framework/build_system.sh)
# The key names the committed files, so uncommitted ones would publish a runtime it does not describe.
if [ -n "$(git status --porcelain)" ]; then
    echo "error: the working tree has changes; commit them so the key describes the runtime" >&2
    exit 1
fi
TAG="$IMAGE:$(packaging/runtime_key.sh)-$ARCH"
if docker manifest inspect "$TAG" > /dev/null 2>&1; then
    echo "$TAG is already published"
    exit 0
fi

# A volume of its own per CPU, owned by the builders' user, and emptied so nothing older remains.
VOLUME=uxdroid-pkg-runtime-$ARCH
MOUNT=(-v "$VOLUME:/usr/lib/uxdroid")
docker compose run --rm -T --no-deps --user 0 --cap-add CHOWN "${MOUNT[@]}" dev \
    chown 1000:1000 /usr/lib/uxdroid
docker compose --profile art run --rm -T "${MOUNT[@]}" art-builder rm -rf /usr/lib/uxdroid/runtime
for i in "${!STEPS[@]}"; do
    docker compose --profile art run --rm -T -e RUNTIME=/usr/lib/uxdroid/runtime -e ARCH="$ISA" \
        "${MOUNT[@]}" "${SERVICES[$i]}" bash "/work/runtime/${STEPS[$i]}"
done
# runtime.sh refuses a runtime for another CPU or another root, so it checks what is pushed.
docker compose run --rm -T --no-deps -v "$VOLUME:/usr/lib/uxdroid:ro" packager \
    sh -c "packaging/runtime.sh $ARCH /usr/lib/uxdroid/runtime \$(mktemp -d)"

# The image holds only the runtime, at /runtime, without the smoke tests' samples as in the package.
docker compose run --rm -T --no-deps -v "$VOLUME:/usr/lib/uxdroid:ro" packager sh -c '
    context=$(mktemp -d)
    printf "FROM scratch\nCOPY runtime /runtime\n" > "$context/Dockerfile"
    tar -cf - -C "$context" Dockerfile -C /usr/lib/uxdroid --exclude=runtime/samples runtime' |
    docker build -q --platform "linux/$ARCH" --label "org.opencontainers.image.source=$SOURCE" \
        --label "org.opencontainers.image.revision=$(git rev-parse HEAD)" -t "$TAG" -
docker push "$TAG"
