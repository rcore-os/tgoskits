#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$script_dir/../../.." && pwd)
toolchain=nightly-2026-07-15
output_dir=$workspace/tmp/competition/ivc/starry
target_dir=$output_dir/target
controller=$target_dir/aarch64-unknown-linux-musl/release/ivcproto

usage() {
    cat <<EOF
Usage: $0 [smoke|full] [options]

Options:
  --profile <smoke|full>             Select the workload size.
  --policy <neural|manual>           Select the controller policy.
  --backend <native|onnxruntime>      Select the inference backend.
  --fault-profile <none|error|restart> Select deterministic controller faults.
  --count <commands>                  Override the profile command count.
  --period-ms <milliseconds>          Set the control period (default: 100).
  --output <image>                    Override the generated rootfs path.
  -h, --help                          Show this help text.
EOF
}

require_positive_integer() {
    local name=$1
    local value=$2

    case "$value" in
        ''|*[!0-9]*)
            echo "$name must be a positive integer: $value" >&2
            exit 2
            ;;
    esac
    if ((value == 0)); then
        echo "$name must be a positive integer: $value" >&2
        exit 2
    fi
}

default_output_image() {
    local policy_suffix=
    local backend_suffix=
    local profile_suffix=
    local fault_suffix=

    if [[ "$policy" == manual ]]; then
        policy_suffix=-manual
    fi
    if [[ "$backend" == onnxruntime ]]; then
        backend_suffix=-onnx
    fi
    if [[ "$profile" == smoke ]]; then
        profile_suffix=-smoke
    fi
    case "$fault_profile" in
        error) fault_suffix=-error ;;
        restart) fault_suffix=-restart ;;
    esac
    printf '%s/starry-ivc-rootfs%s%s%s%s.img\n' \
        "$output_dir" "$policy_suffix" "$backend_suffix" "$profile_suffix" "$fault_suffix"
}

find_base_image() {
    local candidate
    for candidate in \
        "$workspace/.tgos-images/rootfs-aarch64-busybox.img/rootfs-aarch64-busybox.img" \
        "$workspace/tmp/axbuild/rootfs/rootfs-aarch64-busybox.img/rootfs-aarch64-busybox.img"; do
        if [[ -f "$candidate" ]]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done
    return 1
}

profile=full
policy=neural
backend=native
fault_profile=none
command_count=
period_ms=100
output_image=
ivc_restart_previous_session=286331153
ivc_restart_current_session=572662306
ivc_restart_first_count=20
ivc_restart_ack_timeout_ms=1000

if (($# > 0)) && [[ "$1" != -* ]]; then
    profile=$1
    shift
fi
while (($# > 0)); do
    case "$1" in
        --profile)
            profile=${2:?--profile requires a value}
            shift 2
            ;;
        --policy)
            policy=${2:?--policy requires a value}
            shift 2
            ;;
        --backend)
            backend=${2:?--backend requires a value}
            shift 2
            ;;
        --fault-profile)
            fault_profile=${2:?--fault-profile requires a value}
            shift 2
            ;;
        --count)
            command_count=${2:?--count requires a value}
            shift 2
            ;;
        --period-ms)
            period_ms=${2:?--period-ms requires a value}
            shift 2
            ;;
        --output)
            output_image=${2:?--output requires a value}
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "Unknown Starry rootfs option: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

case "$profile" in
    smoke)
        command_count=${command_count:-20}
        ;;
    full)
        command_count=${command_count:-1800}
        ;;
    *)
        echo "profile must be 'smoke' or 'full': $profile" >&2
        exit 2
        ;;
esac
case "$policy" in
    neural|manual) ;;
    *)
        echo "policy must be 'neural' or 'manual': $policy" >&2
        exit 2
        ;;
esac
case "$backend" in
    native|onnxruntime) ;;
    *)
        echo "backend must be 'native' or 'onnxruntime': $backend" >&2
        exit 2
        ;;
esac
case "$fault_profile" in
    none|error|restart) ;;
    *)
        echo "fault profile must be 'none', 'error', or 'restart': $fault_profile" >&2
        exit 2
        ;;
esac
require_positive_integer --count "$command_count"
require_positive_integer --period-ms "$period_ms"
if [[ "$fault_profile" == restart && "$command_count" != 100 ]]; then
    echo "restart fault profile requires exactly 100 post-reset commands" >&2
    exit 2
fi

output_image=${output_image:-$(default_output_image)}
if [[ "$output_image" != /* ]]; then
    output_image=$workspace/$output_image
fi

cd "$workspace"
if ! base_image=$(find_base_image); then
    cargo "+$toolchain" xtask image pull rootfs-aarch64-busybox.img
    base_image=$(find_base_image) || {
        echo "Managed AArch64 BusyBox rootfs was not found after pull" >&2
        exit 1
    }
fi

rustup "+$toolchain" target add aarch64-unknown-linux-musl
CARGO_TARGET_DIR="$target_dir" \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld \
    cargo "+$toolchain" build --release --target aarch64-unknown-linux-musl -p ivcproto

mkdir -p "$output_dir" "$(dirname -- "$output_image")"
cp --reflink=auto --sparse=always "$base_image" "$output_image"
truncate -s "${IVC_STARRY_ROOTFS_SIZE:-64M}" "$output_image"
set +e
e2fsck -fy "$output_image"
fsck_status=$?
set -e
if ((fsck_status > 1)); then
    echo "e2fsck failed before resizing $output_image" >&2
    exit "$fsck_status"
fi
resize2fs "$output_image"

profile_file=$(mktemp "$output_dir/profile.XXXXXX")
cleanup() {
    rm -f -- "$profile_file"
}
trap cleanup EXIT HUP INT TERM
printf 'ivc_mode=%s\nivc_backend=%s\nivc_fault_profile=%s\nivc_profile=%s\nivc_count=%s\nivc_period_ms=%s\nivc_raw_csv=/var/lib/ivc/raw.csv\nivc_restart_previous_session=%s\nivc_restart_current_session=%s\nivc_restart_first_count=%s\nivc_restart_ack_timeout_ms=%s\n' \
    "$policy" "$backend" "$fault_profile" "$profile" "$command_count" "$period_ms" \
    "$ivc_restart_previous_session" "$ivc_restart_current_session" \
    "$ivc_restart_first_count" "$ivc_restart_ack_timeout_ms" \
    >"$profile_file"
for directory in /root /usr /usr/bin /usr/local /usr/local/bin /etc /var /var/lib /var/lib/ivc; do
    debugfs -w -R "mkdir $directory" "$output_image" >/dev/null 2>&1 || true
done

debugfs -w -R "rm /usr/local/bin/ivcproto" "$output_image" >/dev/null 2>&1 || true
debugfs -w -R "write $controller /usr/local/bin/ivcproto" "$output_image"
debugfs -w -R "set_inode_field /usr/local/bin/ivcproto mode 0100755" "$output_image"

debugfs -w -R "rm /usr/bin/starry-run-case-tests" "$output_image" >/dev/null 2>&1 || true
debugfs -w -R "write $script_dir/autorun.sh /usr/bin/starry-run-case-tests" "$output_image"
debugfs -w -R "set_inode_field /usr/bin/starry-run-case-tests mode 0100755" "$output_image"

debugfs -w -R "rm /etc/ivc-profile" "$output_image" >/dev/null 2>&1 || true
debugfs -w -R "write $profile_file /etc/ivc-profile" "$output_image"
debugfs -w -R "set_inode_field /etc/ivc-profile mode 0100644" "$output_image"

set +e
e2fsck -fy "$output_image"
fsck_status=$?
set -e
if ((fsck_status > 1)); then
    echo "e2fsck failed after populating $output_image" >&2
    exit "$fsck_status"
fi

debugfs -R "stat /usr/local/bin/ivcproto" "$output_image"
debugfs -R "stat /usr/bin/starry-run-case-tests" "$output_image"
debugfs -R "cat /etc/ivc-profile" "$output_image"
sha256sum "$controller" "$output_image"
echo "IVC StarryOS profile=$profile policy=$policy backend=$backend fault_profile=$fault_profile rootfs ready at $output_image"
