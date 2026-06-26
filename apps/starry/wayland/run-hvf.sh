#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE="$(cd "$SCRIPT_DIR/../../.." && pwd)"
ARCH="aarch64"
TARGET="aarch64-unknown-none-softfloat"
STD_TARGET="aarch64-unknown-linux-musl"

ROOTFS_BASE_NAME="rootfs--alpine.img"
ROOTFS_BASE_DIR="/tmp/axbuild/rootfs/rootfs--alpine"
ROOTFS_BASE="/"
ROOTFS_LEGACY_BASE="/tmp/axbuild/rootfs/"
ROOTFS_APP_NAME="rootfs--wayland.img"
ROOTFS_APP_DIR="/tmp/axbuild/rootfs/rootfs--wayland"
ROOTFS_APP="/"
BUILD_CONFIG="$SCRIPT_DIR/build-${TARGET}.toml"
PROVISION_MARKER=".wayland-provisioned"
WAYLAND_ROOTFS_MB="${STARRY_WAYLAND_ROOTFS_MB:-4096}"

NO_BUILD=false
REPROVISION=false
USE_HVF=false
PROVISION_ONLY=false
USE_COCOA="${STARRY_COCOA:-true}"

for arg in "$@"; do
    case "$arg" in
        --no-build)       NO_BUILD=true ;;
        --reprovision)    REPROVISION=true ;;
        --hvf)            USE_HVF=true ;;
        --provision-only) PROVISION_ONLY=true ;;
        --vnc-only)       USE_COCOA=false ;;
        *) echo "unknown: $arg" >&2; exit 1 ;;
    esac
done

DBSBIN="/opt/homebrew/opt/e2fsprogs/sbin"
export PATH="$DBSBIN:/opt/homebrew/bin:/usr/local/bin:$PATH"

require_tool() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "error: missing required host tool: $1" >&2
        exit 1
    fi
}

resolve_kernel_bin() {
    local std_kernel="$WORKSPACE/target/${STD_TARGET}/release/starryos.bin"
    local legacy_kernel="$WORKSPACE/target/${TARGET}/release/starryos.bin"

    if [ -f "$std_kernel" ]; then
        printf '%s\n' "$std_kernel"
        return
    fi
    if [ -f "$legacy_kernel" ]; then
        printf '%s\n' "$legacy_kernel"
        return
    fi

    echo "error: kernel not found; checked:" >&2
    echo "  $std_kernel" >&2
    echo "  $legacy_kernel" >&2
    exit 1
}

resolve_rootfs_base() {
    if [ -f "" ]; then
        printf '%s\n' ""
        return
    fi
    if [ -f "" ]; then
        printf '%s\n' ""
        return
    fi

    echo "error: base rootfs not found; checked:" >&2
    echo "  " >&2
    echo "  " >&2
    exit 1
}

run_provision_qemu() {
    local provision_log="$1"
    local input_fifo
    local pipeline_pid
    local deadline
    local saw_done=false
    local qemu_status=0

    input_fifo="$(mktemp -u "${TMPDIR:-/tmp}/wayland-qemu-input.XXXXXX")"
    mkfifo "$input_fifo"

    (
        qemu-system-aarch64 \
            -machine virt \
            -cpu cortex-a53 \
            -smp 4 -m 2048M \
            -nographic \
            -device virtio-blk-pci,drive=disk0 \
            -drive "id=disk0,if=none,format=raw,file=$ROOTFS_APP" \
            -append "root=/dev/sda console=ttyS0" \
            -kernel "$KERNEL" \
            <"$input_fifo" 2>&1 | tee "$provision_log"
    ) &
    pipeline_pid=$!

    exec 3>"$input_fifo"
    rm -f "$input_fifo"

    sleep 25
    printf 'sh /usr/bin/provision-wayland.sh\n' >&3

    deadline=$((SECONDS + 900))
    while kill -0 "$pipeline_pid" >/dev/null 2>&1; do
        if grep -q "PROVISION_DONE" "$provision_log" 2>/dev/null; then
            saw_done=true
            printf '\001x' >&3 || true
            break
        fi
        if grep -q "PROVISION_NO_PREFETCHED_APKS\\|PROVISION_FAILED\\|panic" "$provision_log" 2>/dev/null; then
            break
        fi
        if [ "$SECONDS" -ge "$deadline" ]; then
            echo "error: provisioning timed out after 900s" >&2
            break
        fi
        sleep 2
    done

    exec 3>&-

    for _ in 1 2 3 4 5 6 7 8 9 10; do
        if ! kill -0 "$pipeline_pid" >/dev/null 2>&1; then
            break
        fi
        sleep 1
    done
    if kill -0 "$pipeline_pid" >/dev/null 2>&1; then
        kill "$pipeline_pid" >/dev/null 2>&1 || true
    fi
    wait "$pipeline_pid" >/dev/null 2>&1 || qemu_status=$?

    if [ "$saw_done" = true ]; then
        return 0
    fi
    return "${qemu_status:-1}"
}

inject_overlay() {
    local overlay_dir="$1"
    local rootfs_img="$2"
    local commands
    local log

    commands="$(mktemp "${TMPDIR:-/tmp}/wayland-overlay.XXXXXX")"
    log="${rootfs_img}.debugfs-overlay.log"
    {
        (
            cd "$overlay_dir"
            find . -type d | LC_ALL=C sort
        ) | while IFS= read -r rel; do
            [ "$rel" = "." ] && continue
            printf 'mkdir /%s\n' "${rel#./}"
        done

        (
            cd "$overlay_dir"
            find . -type f | LC_ALL=C sort
        ) | while IFS= read -r rel; do
            local host_path="$overlay_dir/${rel#./}"
            local guest_path="/${rel#./}"
            local mode
            mode="$(stat -f '%Lp' "$host_path" 2>/dev/null || stat -c '%a' "$host_path")"
            printf 'rm %s\n' "$guest_path"
            printf 'write %s %s\n' "$host_path" "$guest_path"
            printf 'sif %s mode 0100%s\n' "$guest_path" "$mode"
        done
    } >"$commands"

    if ! debugfs -w -f "$commands" "$rootfs_img" 2>&1 | tee "$log"; then
        rm -f "$commands"
        echo "error: debugfs overlay injection failed; see $log" >&2
        exit 1
    fi
    if grep -E "Could not allocate block|No space left|write:|error:" "$log" >/dev/null 2>&1; then
        rm -f "$commands"
        echo "error: debugfs reported overlay injection errors; see $log" >&2
        exit 1
    fi
    rm -f "$commands"
}

resize_rootfs() {
    local rootfs_img="$1"
    local target_mb="$2"
    local current_bytes
    local fsck_status
    local target_bytes

    current_bytes="$(stat -f '%z' "$rootfs_img" 2>/dev/null || stat -c '%s' "$rootfs_img")"
    target_bytes=$((target_mb * 1024 * 1024))
    if [ "$current_bytes" -ge "$target_bytes" ]; then
        return
    fi

    echo "==> Expanding manual rootfs to ${target_mb} MiB..."
    set +e
    e2fsck -fy "$rootfs_img" >/dev/null
    fsck_status=$?
    set -e
    if [ "$fsck_status" -gt 1 ]; then
        echo "error: e2fsck failed for $rootfs_img with status $fsck_status" >&2
        exit "$fsck_status"
    fi
    if command -v truncate >/dev/null 2>&1; then
        truncate -s "${target_mb}M" "$rootfs_img"
    else
        dd if=/dev/zero bs=1m count=0 seek="$target_mb" of="$rootfs_img" 2>/dev/null
    fi
    resize2fs "$rootfs_img" >/dev/null
}

rootfs_path_exists() {
    local guest_path="$1"
    local stat_output

    stat_output="$(debugfs -R "stat $guest_path" "$ROOTFS_APP" 2>&1 || true)"
    printf '%s\n' "$stat_output" | grep -q '^Inode:' \
        && ! printf '%s\n' "$stat_output" | grep -q 'File not found'
}

marker_exists() {
    rootfs_path_exists "/$PROVISION_MARKER"
}

installed_packages() {
    debugfs -R "cat /lib/apk/db/installed" "$ROOTFS_APP" 2>/dev/null \
        | sed -n 's/^P://p' \
        | tr '\n' ' '
}

require_tool debugfs
require_tool e2fsck
require_tool resize2fs
require_tool python3
require_tool qemu-system-aarch64

# ---- Build ----
if [ "$NO_BUILD" = false ]; then
    echo "==> Building StarryOS for $ARCH..."
    cd "$WORKSPACE"
    cargo xtask starry build --arch "$ARCH" --config "$BUILD_CONFIG"
fi
KERNEL="$(resolve_kernel_bin)"

# ---- Rootfs ----
mkdir -p ""

if [ ! -f "" ] && [ ! -f "" ]; then
    echo "==> Downloading rootfs..."
    cd ""
    cargo xtask starry rootfs --arch ""
fi
ROOTFS_BASE="1000 4 20 24 27 30 46 109 122 135 136 999 1000resolve_rootfs_base)"

if [ ! -f "$ROOTFS_APP" ] || [ "$REPROVISION" = true ]; then
    echo "==> Creating wayland rootfs from base..."
    cp "$ROOTFS_BASE" "$ROOTFS_APP"
    chmod 0644 "$ROOTFS_APP"
    resize_rootfs "$ROOTFS_APP" "$WAYLAND_ROOTFS_MB"
    echo "nameserver 10.0.2.3" | \
        debugfs -w "$ROOTFS_APP" -R "cd /etc; rm resolv.conf; write /dev/stdin resolv.conf" 2>/dev/null || true
fi

if ! marker_exists || [ "$REPROVISION" = true ]; then
    resize_rootfs "$ROOTFS_APP" "$WAYLAND_ROOTFS_MB"

    echo "==> Injecting offline Wayland/GTK APK overlay..."
    OVERLAY_DIR="$WORKSPACE/tmp/axbuild/starry-app/wayland-manual-overlay"
    rm -rf "$OVERLAY_DIR"
    mkdir -p "$OVERLAY_DIR/usr/bin"
    export STARRY_APP_DIR="$SCRIPT_DIR"
    export STARRY_WORKSPACE="$WORKSPACE"
    export STARRY_ARCH="$ARCH"
    export STARRY_ROOTFS="$ROOTFS_APP"
    export STARRY_OVERLAY_DIR="$OVERLAY_DIR"
    export STARRY_WAYLAND_EXTRA_APKS="gtk4.0-demo font-dejavu"
    export STARRY_WAYLAND_INSTALLED_PACKAGES="$(installed_packages)"
    export STARRY_WAYLAND_WRITE_INSTALL_LIST=1
    bash "$SCRIPT_DIR/prebuild.sh"

    cat >"$OVERLAY_DIR/usr/bin/provision-wayland.sh" <<'SCRIPT'
#!/bin/sh
set -eu
echo "PROVISION_BEGIN"
apk_list=/usr/local/wayland-apks/install.list
if [ ! -s "$apk_list" ]; then
    echo "PROVISION_NO_PREFETCHED_APKS"
    exit 1
fi
xargs apk add --allow-untrusted --no-network < "$apk_list"
echo "PROVISION_PACKAGES_DONE"
touch /.wayland-provisioned
echo "PROVISION_DONE"
SCRIPT
    chmod 0755 "$OVERLAY_DIR/usr/bin/provision-wayland.sh"

    inject_overlay "$OVERLAY_DIR" "$ROOTFS_APP"
fi

if ! marker_exists || [ "$REPROVISION" = true ]; then
    echo "==> Provisioning: installing prefetched Weston + GTK4 demo APKs (~3-5 min)..."

    # Headless boot: send the provision command once the shell is ready.
    # AArch64 TCG boot is slow — wait 25s before sending.
    provision_log="$WORKSPACE/tmp/axbuild/rootfs/provision-${ARCH}-wayland.log"
    echo "    (booting headless, aarch64 TCG takes ~20s to reach shell...)"
    set +e
    run_provision_qemu "$provision_log"
    qemu_status=$?
    set -e

    if [ "$qemu_status" -ne 0 ]; then
        echo "==> WARNING: provisioning QEMU exited with status $qemu_status."
        echo "    See $provision_log"
    fi

    # Verify marker was written
    if marker_exists; then
        echo "==> Provision complete."
    else
        echo "error: provision failed; marker was not written." >&2
        echo "    See $provision_log" >&2
        exit 1
    fi
fi

if [ "$PROVISION_ONLY" = true ]; then
    echo "==> Provision-only requested; not launching graphical QEMU."
    exit 0
fi

# ---- Launch ----
CPU="cortex-a53"
ACCEL="TCG"
DISPLAY_NAME="Cocoa + VNC"
DISPLAY_ARGS=(-display cocoa,show-cursor=on -vnc ":${STARRY_VNC:-0}")
if [ "$USE_HVF" = true ]; then
    CPU="max"
    ACCEL="HVF"
fi
if [ "$USE_COCOA" = false ]; then
    DISPLAY_NAME="VNC"
    DISPLAY_ARGS=(-nographic -vnc ":${STARRY_VNC:-0}")
fi

echo ""
echo "==> Launching StarryOS ($ACCEL, $DISPLAY_NAME display)..."
echo "    Login as root, then:"
echo "      weston --backend=drm-backend.so --renderer=pixman &"
echo "      gtk4-demo"
echo ""

QEMU_ARGS=(-machine virt -cpu "$CPU")
if [ "$USE_HVF" = true ]; then
    QEMU_ARGS+=(-accel hvf)
fi
QEMU_ARGS+=(-smp 4 -m 2048M)
QEMU_ARGS+=("${DISPLAY_ARGS[@]}")
    QEMU_ARGS+=(
    -device virtio-gpu-pci
    -device virtio-keyboard-pci
    -device virtio-mouse-pci
    -device virtio-blk-pci,drive=disk0
    -drive "id=disk0,if=none,format=raw,file=$ROOTFS_APP"
    -append "root=/dev/sda console=ttyS0"
)
if [ "$USE_COCOA" != false ]; then
    QEMU_ARGS+=(-serial stdio)
fi
QEMU_ARGS+=(-kernel "$KERNEL")
exec qemu-system-aarch64 "${QEMU_ARGS[@]}"
