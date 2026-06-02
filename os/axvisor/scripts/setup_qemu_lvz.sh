#!/bin/bash
#
# Build and install the pinned QEMU-LVZ used by LoongArch64 AxVisor runs.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
VERSION_FILE="$SCRIPT_DIR/qemu-lvz.version"

if [[ ! -f "$VERSION_FILE" ]]; then
    echo "[ERROR] QEMU-LVZ version file not found: $VERSION_FILE" >&2
    exit 1
fi

# shellcheck disable=SC1090
source "$VERSION_FILE"

: "${QEMU_LVZ_REPO:?missing QEMU_LVZ_REPO in $VERSION_FILE}"
: "${QEMU_LVZ_COMMIT:?missing QEMU_LVZ_COMMIT in $VERSION_FILE}"
: "${QEMU_LVZ_TARGET_LIST:?missing QEMU_LVZ_TARGET_LIST in $VERSION_FILE}"

CACHE_ROOT="${AXVISOR_QEMU_LVZ_CACHE:-$HOME/.cache/axvisor/qemu-lvz}"
SRC_DIR="$CACHE_ROOT/src/$QEMU_LVZ_COMMIT"
INSTALL_DIR="$CACHE_ROOT/$QEMU_LVZ_COMMIT"
QEMU_BIN="$INSTALL_DIR/bin/qemu-system-loongarch64"
PYTHON_VENV="$CACHE_ROOT/python-venv"

jobs="${JOBS:-$(nproc 2>/dev/null || echo 1)}"

case "${1:-}" in
    -h|--help)
        cat <<EOF
Usage: $0 [--print-path]

Build and install the pinned QEMU-LVZ into:
  $INSTALL_DIR

Options:
  --print-path    Print the expected qemu-system-loongarch64 path without building.
  -h, --help      Show this help.

Environment:
  AXVISOR_QEMU_LVZ_CACHE  Override cache root. Default: $HOME/.cache/axvisor/qemu-lvz
  AXVISOR_QEMU_LVZ_PYTHON Override host Python used for configuring QEMU.
  JOBS                    Build parallelism. Default: nproc
EOF
        exit 0
        ;;
    --print-path)
        printf '%s\n' "$QEMU_BIN"
        exit 0
        ;;
    "")
        ;;
    *)
        echo "[ERROR] Unknown option: $1" >&2
        exit 1
        ;;
esac

info() {
    echo "[INFO] $*"
}

run() {
    echo "+ $*"
    "$@"
}

git_run() {
    echo "+ git $*"
    GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null git "$@"
}

host_python() {
    if [[ -n "${AXVISOR_QEMU_LVZ_PYTHON:-}" ]]; then
        printf '%s\n' "$AXVISOR_QEMU_LVZ_PYTHON"
    elif [[ -x /usr/bin/python3 ]]; then
        printf '%s\n' /usr/bin/python3
    else
        command -v python3
    fi
}

prepare_python() {
    local base_python="$1"

    if "$base_python" -c 'import distlib' >/dev/null 2>&1; then
        printf '%s\n' "$base_python"
        return 0
    fi

    if [[ ! -x "$PYTHON_VENV/bin/python3" ]] \
        || ! "$PYTHON_VENV/bin/python3" -c 'import distlib' >/dev/null 2>&1; then
        info "Preparing QEMU configure Python venv" >&2
        run "$base_python" -m venv "$PYTHON_VENV" >&2
        run "$PYTHON_VENV/bin/python3" -m pip install --upgrade pip >&2
        run "$PYTHON_VENV/bin/python3" -m pip install distlib >&2
    fi

    printf '%s\n' "$PYTHON_VENV/bin/python3"
}

if [[ -x "$QEMU_BIN" ]]; then
    info "Pinned QEMU-LVZ already installed: $QEMU_BIN"
else
    base_python="$(host_python)" || {
        echo "[ERROR] python3 not found. Install python3 or set AXVISOR_QEMU_LVZ_PYTHON." >&2
        exit 1
    }
    qemu_python="$(prepare_python "$base_python")"

    mkdir -p "$CACHE_ROOT/src"

    if [[ ! -d "$SRC_DIR/.git" ]]; then
        info "Cloning QEMU-LVZ $QEMU_LVZ_COMMIT"
        git_run clone "$QEMU_LVZ_REPO" "$SRC_DIR"
    fi

    git_run -C "$SRC_DIR" fetch --depth 1 origin "$QEMU_LVZ_COMMIT"
    git_run -C "$SRC_DIR" checkout --detach "$QEMU_LVZ_COMMIT"

    mkdir -p "$INSTALL_DIR"

    mkdir -p "$SRC_DIR/build"
    if [[ ! -f "$SRC_DIR/build/build.ninja" ]]; then
        info "Configuring QEMU-LVZ"
        (
            cd "$SRC_DIR/build"
            GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null run ../configure \
                --target-list="$QEMU_LVZ_TARGET_LIST" \
                --prefix="$INSTALL_DIR" \
                --python="$qemu_python"
        )
    fi

    info "Building QEMU-LVZ with $jobs job(s)"
    run make -C "$SRC_DIR/build" -j "$jobs"
    run make -C "$SRC_DIR/build" install
fi

info "QEMU-LVZ binary: $QEMU_BIN"
"$QEMU_BIN" --version
info "After this one-time setup, run ./scripts/quick-start.sh qemu-loongarch64 run --linux directly."
