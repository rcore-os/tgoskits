#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(git -C "$script_dir" rev-parse --show-toplevel)
probe=$workspace/competition/results/axvisor-rt-reference/axvisor-rt-probe
base_rootfs=
output=$workspace/tmp/axvisor-rt/starry-rt-compat-rootfs.img
mode=compat
workload=idle
iterations=100
warmup=10
period_us=1000
measurement_cpu=0
stress_cpu=1
fifo_priority=80

usage() {
    cat <<'EOF'
usage: build-starry-rootfs.sh [options]

Options:
  --base-rootfs PATH      Base AArch64 BusyBox ext4 image (auto-detected by default)
  --probe PATH            Static AArch64 axvisor-rt-probe binary
  --output PATH           Output rootfs image
  --mode MODE             compat or capture (default: compat)
  --workload MODE         idle or cpu-stress for capture mode (default: idle)
  --iterations N          Recorded iterations per compatibility phase (default: 100)
  --warmup N              Warmup iterations per phase (default: 10)
  --period-us N           Probe period in microseconds (default: 1000)
  --measurement-cpu N     Guest CPU used for measured phases (default: 0)
  --stress-cpu N          Guest CPU used for CPU-stress smoke (default: 1)
  --fifo-priority N       SCHED_FIFO priority in 1..98 (default: 80)
EOF
}

require_nonnegative_integer() {
    local name=$1
    local value=$2

    if [[ ! "$value" =~ ^[0-9]+$ ]]; then
        echo "$name must be a non-negative integer: $value" >&2
        exit 2
    fi
}

find_base_rootfs() {
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

while (($# > 0)); do
    case "$1" in
        --base-rootfs) base_rootfs=${2:?--base-rootfs requires a value}; shift 2 ;;
        --probe) probe=${2:?--probe requires a value}; shift 2 ;;
        --output) output=${2:?--output requires a value}; shift 2 ;;
        --mode) mode=${2:?--mode requires a value}; shift 2 ;;
        --workload) workload=${2:?--workload requires a value}; shift 2 ;;
        --iterations) iterations=${2:?--iterations requires a value}; shift 2 ;;
        --warmup) warmup=${2:?--warmup requires a value}; shift 2 ;;
        --period-us) period_us=${2:?--period-us requires a value}; shift 2 ;;
        --measurement-cpu) measurement_cpu=${2:?--measurement-cpu requires a value}; shift 2 ;;
        --stress-cpu) stress_cpu=${2:?--stress-cpu requires a value}; shift 2 ;;
        --fifo-priority) fifo_priority=${2:?--fifo-priority requires a value}; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

case "$mode" in
    compat)
        guest_runner=$script_dir/guest/starry_rt_compat_run.sh
        ;;
    capture)
        guest_runner=$script_dir/guest/starry_rt_capture_run.sh
        ;;
    *)
        echo "--mode must be compat or capture: $mode" >&2
        exit 2
        ;;
esac
case "$workload" in
    idle|cpu-stress) ;;
    *)
        echo "--workload must be idle or cpu-stress: $workload" >&2
        exit 2
        ;;
esac

for value in \
    "iterations:$iterations" \
    "warmup:$warmup" \
    "period-us:$period_us" \
    "measurement-cpu:$measurement_cpu" \
    "stress-cpu:$stress_cpu" \
    "fifo-priority:$fifo_priority"; do
    require_nonnegative_integer "${value%%:*}" "${value#*:}"
done
if ((iterations == 0 || period_us == 0 || fifo_priority == 0 || fifo_priority > 98)); then
    echo "iterations/period-us must be positive and fifo-priority must be in 1..98" >&2
    exit 2
fi
if ((measurement_cpu == stress_cpu)); then
    echo "measurement-cpu and stress-cpu must be different" >&2
    exit 2
fi

if [[ -z "$base_rootfs" ]]; then
    base_rootfs=$(find_base_rootfs) || {
        echo "managed AArch64 BusyBox rootfs was not found; pass --base-rootfs" >&2
        exit 1
    }
fi
for input in "$base_rootfs" "$probe" "$guest_runner"; do
    if [[ ! -r "$input" ]]; then
        echo "required input is not readable: $input" >&2
        exit 1
    fi
    if [[ "$input" == *[[:space:]]* ]]; then
        echo "debugfs input paths must not contain whitespace: $input" >&2
        exit 1
    fi
done
for command in debugfs e2fsck file resize2fs sha256sum; do
    command -v "$command" >/dev/null 2>&1 || {
        echo "required command not found: $command" >&2
        exit 1
    }
done
if ! file "$probe" | grep -Eq 'ELF 64-bit .* ARM aarch64.*statically linked'; then
    echo "--probe must be a statically linked AArch64 ELF executable" >&2
    exit 1
fi

if [[ "$output" != /* ]]; then
    output=$workspace/$output
fi
mkdir -p "$(dirname -- "$output")"
if [[ "$output" == *[[:space:]]* ]]; then
    echo "debugfs output path must not contain whitespace: $output" >&2
    exit 1
fi

profile=$(mktemp "${TMPDIR:-/tmp}/axvisor-rt-starry-profile.XXXXXX")
cleanup() {
    rm -f -- "$profile"
}
trap cleanup EXIT HUP INT TERM
printf 'iterations=%s\nwarmup=%s\nperiod_us=%s\nmeasurement_cpu=%s\nstress_cpu=%s\nfifo_priority=%s\nworkload=%s\n' \
    "$iterations" "$warmup" "$period_us" "$measurement_cpu" "$stress_cpu" \
    "$fifo_priority" "$workload" >"$profile"

cp --reflink=auto --sparse=always "$base_rootfs" "$output"
truncate -s "${AXVISOR_RT_STARRY_ROOTFS_SIZE:-64M}" "$output"
set +e
e2fsck -fy "$output"
fsck_status=$?
set -e
if ((fsck_status > 1)); then
    echo "e2fsck failed before resizing $output" >&2
    exit "$fsck_status"
fi
resize2fs "$output"

for directory in /usr /usr/bin /usr/local /usr/local/bin /etc /tmp; do
    debugfs -w -R "mkdir $directory" "$output" >/dev/null 2>&1 || true
done
debugfs -w -R 'rm /usr/local/bin/axvisor-rt-probe' "$output" >/dev/null 2>&1 || true
debugfs -w -R "write $probe /usr/local/bin/axvisor-rt-probe" "$output"
debugfs -w -R 'set_inode_field /usr/local/bin/axvisor-rt-probe mode 0100755' "$output"
debugfs -w -R 'rm /usr/bin/starry-run-case-tests' "$output" >/dev/null 2>&1 || true
debugfs -w -R "write $guest_runner /usr/bin/starry-run-case-tests" "$output"
debugfs -w -R 'set_inode_field /usr/bin/starry-run-case-tests mode 0100755' "$output"
debugfs -w -R 'rm /etc/axvisor-rt-profile' "$output" >/dev/null 2>&1 || true
debugfs -w -R "write $profile /etc/axvisor-rt-profile" "$output"
debugfs -w -R 'set_inode_field /etc/axvisor-rt-profile mode 0100644' "$output"

set +e
e2fsck -fy "$output"
fsck_status=$?
set -e
if ((fsck_status > 1)); then
    echo "e2fsck failed after populating $output" >&2
    exit "$fsck_status"
fi

debugfs -R 'stat /usr/local/bin/axvisor-rt-probe' "$output"
debugfs -R 'stat /usr/bin/starry-run-case-tests' "$output"
debugfs -R 'cat /etc/axvisor-rt-profile' "$output"
sha256sum "$probe" "$guest_runner" "$output"
echo "AXVISOR_RT_STARRY_ROOTFS_READY path=$output mode=$mode workload=$workload iterations=$iterations"
