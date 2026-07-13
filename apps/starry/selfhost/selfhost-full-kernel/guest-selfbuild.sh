#!/bin/sh

set -eu

TOOLCHAIN="nightly-2026-05-28"
HOST_TRIPLE="x86_64-unknown-linux-musl"
RUSTUP_TOOLCHAIN="${TOOLCHAIN}-${HOST_TRIPLE}"
SOURCE_TAR="${SELFHOST_SOURCE_TAR:-/opt/tgoskits-src.tar}"
SOURCE_META="${SELFHOST_SOURCE_META:-/opt/tgoskits-src.meta}"
SOURCE_DIR="${SELFHOST_SOURCE_DIR:-/tmp/tgoskits-src}"
TARGET_DIR="${SELFHOST_TARGET_DIR:-/opt/starry-selfhost-target}"
ARTIFACT="${SELFHOST_ARTIFACT:-/opt/starryos-selfbuilt}"
STATE_FILE="${SELFHOST_STATE_FILE:-/opt/starry-selfhost.state}"
RUN_ID_FILE="${SELFHOST_RUN_ID_FILE:-/opt/starry-selfhost.run-id}"
FAILURE_REASON="guest command failed"
CURRENT_PHASE="bootstrap"
RUN_ID="unknown"

write_state() {
    state="$1"
    phase="$2"
    state_tmp="${STATE_FILE}.tmp"

    printf '%s %s %s\n' "$state" "$RUN_ID" "$phase" >"$state_tmp"
    mv "$state_tmp" "$STATE_FILE"
    sync
}

mark_phase() {
    CURRENT_PHASE="$1"
    write_state running "$CURRENT_PHASE"
    echo "SELF_COMPILE_PHASE=$CURRENT_PHASE"
}

finish_failure() {
    status="$1"
    trap - EXIT
    write_state failed "$CURRENT_PHASE" 2>/dev/null || true
    echo "SELF_COMPILE_FAILED: $FAILURE_REASON (phase=$CURRENT_PHASE, status=$status)"
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
    write_state success publish
    echo "SELF_COMPILE_ARTIFACT=$ARTIFACT"
    echo "SELF_COMPILE_ARTIFACT_SIZE=$artifact_size"
    echo "SELF_COMPILE_SUCCESS"
    poweroff -f 2>/dev/null || poweroff 2>/dev/null || true
    exit 0
}

load_run_id() {
    [ -s "$RUN_ID_FILE" ] || fail "run id is missing: $RUN_ID_FILE"
    IFS= read -r RUN_ID <"$RUN_ID_FILE"
    [ -n "$RUN_ID" ] || fail "run id is empty: $RUN_ID_FILE"
}

install_build_packages() {
    # Ensure busybox symlinks exist BEFORE apk touches the rootfs.  The
    # base image's busybox is statically linked and known-good; after apk
    # upgrades musl libc / libcrypto / libssl, exec-ing the new busybox
    # binary may SIGSEGV when the dynamic linker loads the just-upgraded
    # shared libraries.  Running this step first avoids that crash.
    /bin/busybox --install -s /bin 2>/dev/null || true

    # apk upgrades libcrypto3 and libssl3 first (packages 1-2/53).  The
    # new .so files are written to disk and all 53 packages install
    # successfully, but apk's atexit / libc _fini cleanup may SIGSEGV
    # when the dynamic linker touches the just-upgraded libraries during
    # process exit.  This is a known StarryOS ELF-loader limitation,
    # not an rsext4 data corruption — apk reports "OK: … in 97 packages"
    # before the crash.  Absorb the non-fatal exit signal and validate
    # the installed tools below.
    apk add --no-cache --no-scripts \
        bash build-base ca-certificates clang clang-dev cmake curl git libudev-zero-dev \
        linux-headers musl-dev openssl-dev perl pkgconf python3 tar xz \
        || true

    sync

    # Re-create busybox symlinks (apk may have replaced busybox).
    /bin/busybox --install -s /bin 2>/dev/null || true
    update-ca-certificates 2>/dev/null || true

    # Verify the build toolchain actually landed — if any critical binary
    # is missing, fail explicitly rather than crashing later with a
    # confusing "command not found".
    for bin in bash gcc g++ make git curl perl python3 tar xz pkg-config cmake; do
        command -v "$bin" >/dev/null 2>&1 || fail "$bin missing after apk"
    done
    [ -x /bin/bash ] || fail "/bin/bash not executable after apk"
    [ -x /usr/bin/gcc ] || fail "/usr/bin/gcc not executable after apk"
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
    # rsext4 (CACHE_ENTRIES=4, 16 KiB metadata cache) writes small files at
    # ~1-4 KiB/s during tar extraction.  Installing the ~6.9 GiB Rust
    # toolchain directly to ext4 would take hours.  Instead, install to /tmp
    # (MemoryFs / tmpfs, RAM-backed, fast), then copy the result to ext4 in
    # one bulk operation — exactly the approach the selfhost-bootstrap path
    # uses.
    local tmp_rustup=/tmp/rustup-home
    local tmp_cargo=/tmp/cargo-home

    export RUSTUP_HOME="$tmp_rustup"
    export CARGO_HOME="$tmp_cargo"
    export PATH="$tmp_cargo/bin:/usr/local/bin:/usr/bin:/bin"
    export RUSTUP_IO_THREADS="${SELFHOST_RUSTUP_IO_THREADS:-4}"
    export RUSTUP_MAX_RETRIES="${SELFHOST_RUSTUP_MAX_RETRIES:-5}"

    mkdir -p "$tmp_rustup" "$tmp_cargo"

    # Populate rustup download cache from host-side pre-downloaded tarballs.
    local dl_cache="$tmp_rustup/downloads"
    local host_cache="/root/.rustup-dl-cache"
    if [ -d "$host_cache" ]; then
        mkdir -p "$dl_cache"
        for f in "$host_cache"/*; do
            [ -f "$f" ] || continue
            cp "$f" "$dl_cache/" 2>/dev/null || true
        done
        if ls "$dl_cache"/???????????????????????????????????????????????????????????????? 2>/dev/null | grep -q .; then
            echo "[self-compile] rustup download cache pre-populated from host prebuild ($(ls "$dl_cache" | wc -l) entries)"
        fi
        rm -rf "$host_cache"
    fi

    if [ ! -x "$tmp_cargo/bin/rustup" ]; then
        curl --fail --silent --show-error --location https://sh.rustup.rs \
            -o /tmp/rustup-init.sh
        sh /tmp/rustup-init.sh -y --profile minimal --default-host "$HOST_TRIPLE" \
            --default-toolchain "$TOOLCHAIN"
    fi

    rustup toolchain install "$RUSTUP_TOOLCHAIN" --profile minimal
    rustup default "$RUSTUP_TOOLCHAIN"
    rustup component add --toolchain "$RUSTUP_TOOLCHAIN" rust-src llvm-tools-preview
    rustup target add --toolchain "$RUSTUP_TOOLCHAIN" x86_64-unknown-none

    # Copy the completed toolchain from tmpfs to the ext4 rootfs.
    echo "[self-compile] copying Rust toolchain from tmpfs to ext4 rootfs..."
    mkdir -p /root/.rustup /root/.cargo
    cp -a "$tmp_rustup"/. /root/.rustup/ 2>/dev/null || fail "copy rustup from tmpfs failed"
    cp -a "$tmp_cargo"/. /root/.cargo/ 2>/dev/null || fail "copy cargo from tmpfs failed"
    echo "[self-compile] Rust toolchain copied to ext4 rootfs."
    rm -rf "$tmp_rustup" "$tmp_cargo"

    # Restore the persistent paths for subsequent phases.
    export RUSTUP_HOME=/root/.rustup
    export CARGO_HOME=/root/.cargo
    export PATH="$CARGO_HOME/bin:/usr/local/bin:/usr/bin:/bin"
}

install_kallsyms_tools() {
    # kallsyms tools run after install_rust restored CARGO_HOME to ext4.
    # cargo install downloads source + compiles, which is small enough to
    # tolerate rsext4 throughput.
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

    mkdir -p "$TARGET_DIR"
    rm -rf "$SOURCE_DIR/target"
    ln -s "$TARGET_DIR" "$SOURCE_DIR/target"
    [ -d "$SOURCE_DIR/target" ] || fail "persistent target directory is unavailable"

    if [ -f "$SOURCE_META" ]; then
        echo "SELF_COMPILE_SOURCE_METADATA_BEGIN"
        cat "$SOURCE_META"
        echo "SELF_COMPILE_SOURCE_METADATA_END"
    fi
}

report_build_storage() {
    echo "SELF_COMPILE_STORAGE_BEGIN"
    mount | grep -E ' on /(tmp|opt) ' || true
    df -T / /tmp "$TARGET_DIR" 2>/dev/null || df -h / /tmp "$TARGET_DIR"
    free -m 2>/dev/null || sed -n '1,5p' /proc/meminfo
    echo "SELF_COMPILE_STORAGE_END"
}

detect_rust_host() {
    DETECTED_HOST="$(rustc -vV | sed -n 's/^host: //p')"
    [ "$DETECTED_HOST" = "$HOST_TRIPLE" ] \
        || fail "Rust host must be $HOST_TRIPLE, got ${DETECTED_HOST:-unknown}"
    echo "SELF_COMPILE_RUST_HOST=$DETECTED_HOST"
}

build_host_xtask() {
    cd "$SOURCE_DIR"
    RUSTFLAGS= CARGO_ENCODED_RUSTFLAGS= \
        cargo "+$RUSTUP_TOOLCHAIN" build --locked -p tg-xtask --target "$DETECTED_HOST"
    XTASK="$SOURCE_DIR/target/$DETECTED_HOST/debug/tg-xtask"
    [ -x "$XTASK" ] || fail "tg-xtask was not built for $DETECTED_HOST"
}

build_kernel() {
    cd "$SOURCE_DIR"
    "$XTASK" starry build \
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
load_run_id
export CARGO_BUILD_JOBS="${SELFHOST_CARGO_BUILD_JOBS:-2}"
export AXBUILD_STARRY_KALLSYMS_AUTO_INSTALL=0
unset CARGO_BUILD_TARGET

mark_phase packages
install_build_packages
mark_phase network
verify_network
mark_phase rust
configure_musl_toolchain_aliases
install_rust
mark_phase tools
install_kallsyms_tools
mark_phase source
prepare_source_tree
report_build_storage
detect_rust_host
mark_phase xtask-host
build_host_xtask
mark_phase kernel
build_kernel
mark_phase publish
artifact_size="$(publish_artifact)"
finish_success "$artifact_size"
