#!/usr/bin/env bash
# Run a command inside the StarryOS arceos-build container with persistent
# cargo registry/git cache and a docker-private CARGO_TARGET_DIR so repeated
# builds skip the ~30-min crates.io fetch + recompile cycle.
#
# Usage:
#   ./.docker-run.sh                       # interactive shell
#   ./.docker-run.sh make ARCH=riscv64 build
#   ./.docker-run.sh cargo xtask starry test qemu --arch riscv64 --test-case busybox
set -euo pipefail

IMAGE=${STARRY_BUILD_IMAGE:-docker.cnb.cool/starry-os/arceos-build:latest}
LOCAL_IMAGE=${STARRY_BUILD_IMAGE_LOCAL:-starryos-dev-local:latest}
HOST_CACHE=${STARRY_DOCKER_CACHE:-$HOME/.cache/cargo-docker}
WORKSPACE_ROOT=$(cd "$(dirname "$0")/../.." && pwd)

# Build a derived image once that adds packages the base image is missing
# (libudev-dev for ostool's serialport crate, pkg-config, etc).
if ! docker image inspect "$LOCAL_IMAGE" >/dev/null 2>&1; then
    echo "[docker-run] building derived image $LOCAL_IMAGE on top of $IMAGE ..." >&2
    docker build --platform linux/amd64 -t "$LOCAL_IMAGE" -<<EOF >&2
FROM $IMAGE
RUN printf 'Types: deb\nURIs: http://deb.debian.org/debian\nSuites: trixie trixie-updates trixie-backports\nComponents: main contrib non-free non-free-firmware\nSigned-By: /usr/share/keyrings/debian-archive-keyring.gpg\n\nTypes: deb\nURIs: http://security.debian.org/debian-security\nSuites: trixie-security\nComponents: main contrib non-free non-free-firmware\nSigned-By: /usr/share/keyrings/debian-archive-keyring.gpg\n' > /etc/apt/sources.list.d/debian.sources && \
    apt-get update && apt-get install -y --no-install-recommends \
        libudev-dev pkg-config e2fsprogs \
    && rm -rf /var/lib/apt/lists/*
EOF
fi
IMAGE="$LOCAL_IMAGE"

mkdir -p "$HOST_CACHE/registry" "$HOST_CACHE/git" "$HOST_CACHE/target-tgoskits" "$HOST_CACHE/rustup"

# Prime $HOST_CACHE/rustup from the image once, so the first build doesn't
# have to redownload the entire nightly toolchain.
if [ -z "$(ls -A "$HOST_CACHE/rustup" 2>/dev/null)" ]; then
    echo "[docker-run] priming rustup cache from image $IMAGE ..." >&2
    docker run --rm --platform linux/amd64 \
        -v "$HOST_CACHE/rustup":/tmp/host-rustup \
        "$IMAGE" \
        bash -c 'cp -a /root/.rustup/. /tmp/host-rustup/' >&2 || true
fi

TTY_FLAGS=()
if [ -t 0 ] && [ -t 1 ]; then TTY_FLAGS=(-it); fi

# The image is linux/amd64; emulate on arm64 Mac silently.
exec docker run --rm ${TTY_FLAGS[@]+"${TTY_FLAGS[@]}"} \
    --platform linux/amd64 \
    -v "$WORKSPACE_ROOT":/workspace \
    -v "$HOST_CACHE/registry":/root/.cargo/registry \
    -v "$HOST_CACHE/git":/root/.cargo/git \
    -v "$HOST_CACHE/rustup":/root/.rustup \
    -v "$HOST_CACHE/target-tgoskits":/cargo-target \
    -e CARGO_TARGET_DIR=/cargo-target \
    -e CARGO_HTTP_MULTIPLEXING=false \
    -w /workspace/os/StarryOS \
    "$IMAGE" \
    "$@"
