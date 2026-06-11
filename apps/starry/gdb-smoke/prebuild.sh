#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
base_rootfs="${STARRY_ROOTFS:-}"
staging_root="${STARRY_STAGING_ROOT:-}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"
apk_cache="${STARRY_WORKSPACE:-$(cd "$app_dir/../../.." && pwd)}/target/gdb-smoke-apk-cache"
host_artifact_dir="${STARRY_WORKSPACE:-$(cd "$app_dir/../../.." && pwd)}/target/gdb-smoke-host"
qemu_runner=""
lld_linker=""
lld_linker_dir=""

require_env() {
    local name="$1"
    local value="$2"
    if [[ -z "$value" ]]; then
        echo "error: $name is required" >&2
        exit 1
    fi
}

ensure_host_packages() {
    local missing=()

    command -v clang >/dev/null 2>&1 || missing+=(clang)
    command -v debugfs >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v install >/dev/null 2>&1 || missing+=(coreutils)
    if command -v ld.lld >/dev/null 2>&1; then
        lld_linker="$(command -v ld.lld)"
    elif command -v rust-lld >/dev/null 2>&1; then
        lld_linker_dir="$(mktemp -d "${TMPDIR:-/tmp}/gdb-smoke-lld.XXXXXX")"
        printf '#!/usr/bin/env bash\nexec %q -flavor gnu "$@"\n' \
            "$(command -v rust-lld)" >"$lld_linker_dir/ld.lld"
        chmod +x "$lld_linker_dir/ld.lld"
        lld_linker="$lld_linker_dir/ld.lld"
    else
        missing+=(lld)
    fi
    command -v readelf >/dev/null 2>&1 || missing+=(binutils)

    if [[ ${#missing[@]} -eq 0 ]]; then
        return
    fi

    if ! command -v apt-get >/dev/null 2>&1; then
        echo "error: missing required host packages and apt-get is unavailable: ${missing[*]}" >&2
        exit 1
    fi

    echo "installing missing host packages: ${missing[*]}"
    apt-get update
    apt-get install -y --no-install-recommends "${missing[@]}"
}

extract_base_rootfs() {
    debugfs -R "rdump / $staging_root" "$base_rootfs"
}

find_qemu_runner() {
    if command -v qemu-riscv64-static >/dev/null 2>&1; then
        qemu_runner="$(command -v qemu-riscv64-static)"
    elif command -v qemu-riscv64 >/dev/null 2>&1; then
        qemu_runner="$(command -v qemu-riscv64)"
    else
        echo "error: qemu-riscv64-static or qemu-riscv64 is required" >&2
        exit 1
    fi
}

run_guest_apk_with_retry() {
    local attempt
    local max_attempts=4

    for attempt in $(seq 1 "$max_attempts"); do
        if QEMU_LD_PREFIX="$staging_root" \
            LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib:$staging_root/usr/local/lib" \
            "$qemu_runner" -L "$staging_root" "$staging_root/sbin/apk" "$@"; then
            return 0
        fi

        if [[ "$attempt" -eq "$max_attempts" ]]; then
            return 1
        fi

        echo "apk command failed, retrying ($attempt/$max_attempts)..." >&2
        sleep $((attempt * 3))
    done
}

install_guest_packages() {
    local guest_apk="$staging_root/sbin/apk"

    mkdir -p "$apk_cache"
    if [[ ! -x "$guest_apk" ]]; then
        echo "error: staging root is missing guest apk: $guest_apk" >&2
        exit 1
    fi

    # Use the runner DNS settings so qemu-user apk can resolve Alpine mirrors.
    if [[ -f /etc/resolv.conf ]]; then
        cp /etc/resolv.conf "$staging_root/etc/resolv.conf"
    fi

    run_guest_apk_with_retry \
        --root "$staging_root" \
        --repositories-file "$staging_root/etc/apk/repositories" \
        --keys-dir "$staging_root/etc/apk/keys" \
        --cache-dir "$apk_cache" \
        --update-cache \
        --timeout 60 \
        --no-interactive \
        --force-no-chroot \
        --scripts=no \
        add gdb gcc musl-dev
}

compile_target() {
    local source="$1"
    local output="$2"
    shift 2

    install -d "$(dirname "$overlay_dir$output")"
    clang \
        --target=riscv64-linux-musl \
        --sysroot="$staging_root" \
        --gcc-toolchain="$staging_root/usr" \
        --ld-path="$lld_linker" \
        -static \
        "$@" \
        "$source" \
        -o "$overlay_dir$output"
}

copy_file_to_overlay() {
    local guest_path="$1"
    local mode="$2"
    local source="$staging_root${guest_path}"
    local target="$overlay_dir${guest_path}"

    if [[ ! -e "$source" ]]; then
        echo "error: missing guest file after gdb package install: $guest_path" >&2
        exit 1
    fi

    if [[ -L "$source" ]]; then
        source="$(readlink -f "$source")"
    fi

    install -Dm"$mode" "$source" "$target"
}

find_library_path() {
    local library="$1"
    local dir

    for dir in lib usr/lib usr/local/lib; do
        if [[ -e "$staging_root/$dir/$library" ]]; then
            printf '/%s/%s\n' "$dir" "$library"
            return 0
        fi
    done

    return 1
}

copy_runtime_dependencies() {
    local pending=("$@")
    local seen=" "
    local guest_path library

    while [[ ${#pending[@]} -gt 0 ]]; do
        guest_path="${pending[0]}"
        pending=("${pending[@]:1}")

        if [[ "$seen" == *" $guest_path "* ]]; then
            continue
        fi
        seen+="$guest_path "

        while IFS= read -r library; do
            local library_path
            if ! library_path="$(find_library_path "$library")"; then
                continue
            fi
            copy_file_to_overlay "$library_path" 0644
            pending+=("$library_path")
        done < <(
            readelf -d "$staging_root$guest_path" 2>/dev/null |
                sed -n 's/.*Shared library: \[\(.*\)\].*/\1/p'
        )
    done
}

populate_overlay() {
    compile_target \
        "$app_dir/native/src/main.c" \
        /usr/bin/gdb-native-smoke-target \
        -Wall -Wextra -Werror -O0 -g
    compile_target \
        "$app_dir/native-thread/src/main.c" \
        /usr/bin/gdb-native-thread-target \
        -Wall -Wextra -Werror -O0 -g -pthread
    compile_target \
        "$app_dir/gdbserver/src/main.c" \
        /usr/bin/gdbserver-smoke-target \
        -Wall -Wextra -Werror -O0 -g
    compile_target \
        "$app_dir/stress/src/thread-breakpoint-wall.c" \
        /usr/bin/gdb-ptrace-thread-breakpoint-stress \
        -Wall -Wextra -Werror -O0 -g -pthread
    install -Dm0755 "$overlay_dir/usr/bin/gdbserver-smoke-target" \
        "$host_artifact_dir/gdbserver-smoke-target"

    copy_file_to_overlay /usr/bin/gdb 0755
    copy_file_to_overlay /usr/bin/gdbserver 0755
    copy_runtime_dependencies /usr/bin/gdb /usr/bin/gdbserver

    if [[ -d "$staging_root/usr/share/gdb" ]]; then
        mkdir -p "$overlay_dir/usr/share"
        cp -a "$staging_root/usr/share/gdb" "$overlay_dir/usr/share/"
    fi
    if [[ -d "$staging_root/usr/lib/python3.12" ]]; then
        mkdir -p "$overlay_dir/usr/lib"
        cp -a "$staging_root/usr/lib/python3.12" "$overlay_dir/usr/lib/"
    fi

    install -Dm0755 "$app_dir/native/gdb-native-smoke.gdb" \
        "$overlay_dir/usr/bin/gdb-native-smoke.gdb"
    install -Dm0755 "$app_dir/native-thread/gdb-native-threads.gdb" \
        "$overlay_dir/usr/bin/gdb-native-threads.gdb"
    install -Dm0644 "$app_dir/gdbserver/gdbserver-smoke.gdb" \
        "$overlay_dir/usr/bin/gdbserver-smoke.gdb"
    install -Dm0644 "$app_dir/gdbserver/gdbserver-threads.gdb" \
        "$overlay_dir/usr/bin/gdbserver-threads.gdb"
    install -Dm0755 "$app_dir/gdbserver/gdbserver-smoke.sh" \
        "$overlay_dir/usr/bin/gdbserver-smoke.sh"
}

require_env STARRY_ROOTFS "$base_rootfs"
require_env STARRY_STAGING_ROOT "$staging_root"
require_env STARRY_OVERLAY_DIR "$overlay_dir"

ensure_host_packages
extract_base_rootfs
find_qemu_runner
install_guest_packages
populate_overlay
