#!/bin/sh

set -eu

TOOLCHAIN="${SELFHOST_RUST_TOOLCHAIN:-nightly-2026-05-28}"
HOST_TRIPLE="x86_64-unknown-linux-musl"
SOURCE_TAR="${SELFHOST_SOURCE_TAR:-/opt/tgoskits-src.tar}"
SOURCE_META="${SELFHOST_SOURCE_META:-/opt/tgoskits-src.meta}"
SOURCE_DIR="${SELFHOST_SOURCE_DIR:-/tmp/tgoskits-src}"
ARTIFACT="${SELFHOST_ARTIFACT:-/opt/starryos-selfbuilt}"
FAILURE_REASON="guest command failed"

finish_failure() {
    status="$1"
    trap - EXIT
    echo "SELF_COMPILE_FAILED: $FAILURE_REASON (status=$status)"
    sync 2>/dev/null || true
    poweroff -f 2>/dev/null || poweroff 2>/dev/null || true
    exit "$status"
}

handle_exit() {
    status="$?"
    if [ "$status" -ne 0 ]; then
        finish_failure "$status"
    fi
}

fail() {
    FAILURE_REASON="$1"
    exit 1
}

finish_success() {
    artifact_size="$1"

    trap - EXIT
    sync 2>/dev/null || true
    echo "SELF_COMPILE_ARTIFACT=$ARTIFACT"
    echo "SELF_COMPILE_ARTIFACT_SIZE=$artifact_size"
    echo "SELF_COMPILE_SUCCESS"
    poweroff -f 2>/dev/null || poweroff 2>/dev/null || true
    exit 0
}

install_build_packages() {
    apk add --no-cache --no-scripts \
        bash build-base ca-certificates clang clang-dev cmake curl git libudev-zero-dev \
        linux-headers musl-dev openssl-dev perl pkgconf python3 tar xz
    /bin/busybox --install -s /bin 2>/dev/null || true
    update-ca-certificates 2>/dev/null || true
}

verify_network() {
    curl --fail --silent --show-error --location --retry 3 \
        --connect-timeout 20 --max-time 120 \
        https://static.rust-lang.org/dist/channel-rust-nightly.toml \
        -o /tmp/channel-rust-nightly.toml
}

configure_musl_toolchain_aliases() {
    gcc_path="$(command -v gcc)" || fail "gcc is unavailable after apk install"
    ar_path="$(command -v ar)" || fail "ar is unavailable after apk install"

    mkdir -p /usr/local/bin
    ln -sf "$gcc_path" /usr/local/bin/x86_64-linux-musl-cc
    ln -sf "$gcc_path" /usr/local/bin/x86_64-linux-musl-gcc
    ln -sf "$ar_path" /usr/local/bin/x86_64-linux-musl-ar
}

install_rust() {
    export RUSTUP_HOME=/root/.rustup
    export CARGO_HOME=/root/.cargo
    export PATH="$CARGO_HOME/bin:/usr/local/bin:/usr/bin:/bin"
    # StarryOS reports a conservatively small memory budget to Rustup even when
    # QEMU provides the app's configured 16 GiB. Avoid Rustup's unusably slow
    # single-threaded fallback while keeping the value configurable for debug.
    export RUSTUP_IO_THREADS="${SELFHOST_RUSTUP_IO_THREADS:-4}"
    export RUSTUP_MAX_RETRIES="${SELFHOST_RUSTUP_MAX_RETRIES:-5}"

    if [ ! -x "$CARGO_HOME/bin/rustup" ]; then
        curl --fail --silent --show-error --location https://sh.rustup.rs \
            -o /tmp/rustup-init.sh
        sh /tmp/rustup-init.sh -y --profile minimal --default-host "$HOST_TRIPLE" \
            --default-toolchain "$TOOLCHAIN"
    fi

    rustup toolchain install "$TOOLCHAIN" --profile minimal
    rustup default "$TOOLCHAIN"
    rustup component add --toolchain "$TOOLCHAIN" rust-src llvm-tools-preview
    rustup target add --toolchain "$TOOLCHAIN" x86_64-unknown-none
}

install_kallsyms_tools() {
    if ! cargo install --list | grep -q '^cargo-binutils v0.4.0:'; then
        cargo install cargo-binutils --version 0.4.0 --locked
    fi
    if ! cargo install --list | grep -q '^ksym v0.6.0:'; then
        cargo install ksym --version 0.6.0 --locked
    fi

    command -v rust-nm >/dev/null 2>&1 || fail "cargo-binutils did not install rust-nm"
    command -v rust-objcopy >/dev/null 2>&1 || fail "cargo-binutils did not install rust-objcopy"
    command -v gen_ksym >/dev/null 2>&1 || fail "ksym did not install gen_ksym"
}

prepare_source_tree() {
    [ -f "$SOURCE_TAR" ] || fail "source archive is missing: $SOURCE_TAR"
    rm -rf "$SOURCE_DIR"
    mkdir -p "$SOURCE_DIR"
    tar -xf "$SOURCE_TAR" -C "$SOURCE_DIR"
    [ -f "$SOURCE_DIR/Cargo.toml" ] || fail "source archive does not contain Cargo.toml"

    if [ -f "$SOURCE_META" ]; then
        echo "SELF_COMPILE_SOURCE_METADATA_BEGIN"
        cat "$SOURCE_META"
        echo "SELF_COMPILE_SOURCE_METADATA_END"
    fi
}

build_with_canonical_xtask() {
    detected_host="$(rustc -vV | sed -n 's/^host: //p')"
    [ "$detected_host" = "$HOST_TRIPLE" ] \
        || fail "Rust host must be $HOST_TRIPLE, got ${detected_host:-unknown}"
    echo "SELF_COMPILE_RUST_HOST=$detected_host"

    cd "$SOURCE_DIR"
    export CARGO_BUILD_JOBS="${SELFHOST_CARGO_BUILD_JOBS:-4}"
    export AXBUILD_STARRY_KALLSYMS_AUTO_INSTALL=0
    unset CARGO_BUILD_TARGET

    RUSTFLAGS= CARGO_ENCODED_RUSTFLAGS= \
        cargo build --locked -p tg-xtask --target "$detected_host"
    xtask="$SOURCE_DIR/target/$detected_host/debug/tg-xtask"
    [ -x "$xtask" ] || fail "tg-xtask was not built for $detected_host"

    "$xtask" starry build \
        -c apps/starry/selfhost/build-x86_64-unknown-none.toml \
        --arch x86_64
}

publish_artifact() {
    built_artifact="$SOURCE_DIR/target/x86_64-unknown-linux-musl/release/starryos"
    [ -s "$built_artifact" ] || fail "x86_64 kernel artifact is missing: $built_artifact"

    cp "$built_artifact" "$ARTIFACT"
    chmod 0755 "$ARTIFACT"
    [ -s "$ARTIFACT" ] || fail "failed to persist self-built kernel"
    wc -c <"$ARTIFACT"
}

trap handle_exit EXIT

echo "SELF_COMPILE_START"
install_build_packages
verify_network
configure_musl_toolchain_aliases
install_rust
install_kallsyms_tools
prepare_source_tree
build_with_canonical_xtask
artifact_size="$(publish_artifact)"
finish_success "$artifact_size"
