#!/usr/bin/env bash
# apps/starry/net-bench/run-linux-baseline.sh — Linux 基线性能测试
#
# 用法: bash apps/starry/net-bench/run-linux-baseline.sh [arch] [scenario] [--repeat N]
#
# 目标：在与 Starry 完全相同的 QEMU+vhost 拓扑下运行 Linux guest，
# 以建立公平的性能基线对比。遵循 methodology §4.1 和 qemu-plan §6.1 的纪律。
#
# 前置条件：
#   1. br0 + tap0 已配置（sudo bash apps/starry/net-bench/bin/setup）
#   2. 需要一个 Linux aarch64 内核和 rootfs（使用 Alpine Linux initramfs）
set -euo pipefail

ARCH="aarch64"
SCENARIO="vhost"
REPEAT=1
REBUILD_ROOTFS=0

# 解析参数
positional=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --repeat)
            REPEAT="${2:-}"; shift 2
            [[ "$REPEAT" =~ ^[1-9][0-9]*$ ]] || { echo "error: --repeat needs a positive integer" >&2; exit 1; }
            ;;
        --repeat=*)
            REPEAT="${1#*=}"; shift
            [[ "$REPEAT" =~ ^[1-9][0-9]*$ ]] || { echo "error: --repeat needs a positive integer" >&2; exit 1; }
            ;;
        --rebuild-rootfs)
            REBUILD_ROOTFS=1; shift ;;
        -h|--help|help)
            positional+=("help"); shift ;;
        *)
            positional+=("$1"); shift ;;
    esac
done
[[ ${#positional[@]} -ge 1 ]] && ARCH="${positional[0]}"
[[ ${#positional[@]} -ge 2 ]] && SCENARIO="${positional[1]}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE="$(cd "$SCRIPT_DIR/../../.." && pwd)"
RESULTS_DIR="$SCRIPT_DIR/results"
LINUX_DIR="$SCRIPT_DIR/linux-baseline"
TIMESTAMP="$(date +%Y%m%d-%H%M%S)"
TAP_HOST_IP="${TAP_HOST_IP:-192.168.100.1}"
TAP_GUEST_IP="${TAP_GUEST_IP:-192.168.100.2}"

mkdir -p "$RESULTS_DIR" "$LINUX_DIR"

usage() {
    cat >&2 <<EOF
usage: bash apps/starry/net-bench/run-linux-baseline.sh [arch] [scenario] [--repeat N]

目的：使用与 Starry 相同的 QEMU+vhost 配置运行 Linux guest，建立性能基线。

arch:
  aarch64     仅支持 aarch64（当前限制）

scenario:
  vhost       TAP+vhost-net, smp=1 (主力拓扑)
  vhost-smp4  TAP+vhost-net, smp=4 (多核扩展)

options:
  --repeat N         重复测试 N 次（默认 1）
  --rebuild-rootfs   强制重建 Linux baseline initramfs（忽略已有缓存）

前置条件:
  1. vhost 环境已配置: sudo bash apps/starry/net-bench/bin/setup
  2. iperf3 已安装在 guest（通过 init script 或 rootfs）
  
测试拓扑对齐 (qemu-plan §6.1):
  - 相同的 vhost-net + KVM + virtio-net-pci
  - 相同的 mq/vectors/offload 参数
  - 相同的网络 IP 配置（192.168.100.1/24）
  - 相同的测试负载（iperf3 tcp1/tcp4/tcp1r/udp1g/udp64）
EOF
}

check_prereq() {
    # 检查 vhost 环境
    if [[ ! -e /dev/kvm ]]; then
        echo "error: /dev/kvm not found. KVM acceleration required." >&2
        exit 1
    fi
    if [[ ! -e /dev/vhost-net ]]; then
        echo "error: /dev/vhost-net not found. Run: sudo modprobe vhost_net" >&2
        exit 1
    fi
    
    # 检查网络配置
    if ! ip addr show br0 &>/dev/null; then
        echo "error: br0 not found. Run: sudo bash apps/starry/net-bench/bin/setup" >&2
        exit 1
    fi
    
    # 检查 Linux 内核
    if [[ ! -f "$LINUX_DIR/vmlinuz" ]]; then
        echo "info: Linux kernel not found, will use host kernel if available" >&2
    fi
}

# 校验 initramfs 是否为完整可用的 gzip+cpio 归档。
# 返回 0 表示有效，非 0 表示缺失或损坏（调用方据此决定是否重建）。
validate_initramfs() {
    local rootfs="$1"
    [[ -f "$rootfs" ]] || return 1
    # gzip 完整性
    gzip -t "$rootfs" 2>/dev/null || return 1
    # cpio 归档可完整列出，并且包含 init 入口
    local listing
    listing="$(gzip -dc "$rootfs" 2>/dev/null | cpio -it 2>/dev/null)" || return 1
    [[ -n "$listing" ]] || return 1
    grep -qx "./init\|init" <<<"$listing" || return 1
    return 0
}

# 定位与 Starry 同源的受管 Alpine ext4 rootfs 镜像（内含 busybox/iperf3/ip/nc）。
# 缺失时尝试通过 xtask 拉取；仍失败则明确报错。
locate_alpine_image() {
    local image_name="rootfs-${ARCH}-alpine.img"
    local img="$WORKSPACE/tmp/axbuild/rootfs/$image_name/$image_name"

    if [[ ! -f "$img" ]]; then
        echo "=== Alpine rootfs image missing, fetching via xtask ===" >&2
        ( cd "$WORKSPACE" && cargo xtask starry rootfs --arch "$ARCH" ) >&2 || {
            echo "error: failed to ensure Alpine rootfs via 'cargo xtask starry rootfs --arch $ARCH'" >&2
            return 1
        }
    fi
    [[ -f "$img" ]] || {
        echo "error: managed Alpine rootfs not found at $img" >&2
        return 1
    }
    printf '%s\n' "$img"
}

# 准备 Linux guest initramfs：从受管 Alpine rootfs 提取真实的
# busybox/iperf3/ip/nc 及依赖库，注入 init 脚本后打包为 cpio.gz。
# 这样得到的 initramfs 真实可启动，且与 Starry 测试使用同一镜像来源，可复现。
prepare_linux_rootfs() {
    local rootfs="$LINUX_DIR/initramfs.cpio.gz"

    if [[ "$REBUILD_ROOTFS" -eq 0 ]] && validate_initramfs "$rootfs"; then
        echo "=== Reusing valid Linux baseline initramfs: $rootfs ==="
        return 0
    fi

    if [[ -f "$rootfs" ]] && [[ "$REBUILD_ROOTFS" -eq 0 ]]; then
        echo "warning: existing initramfs is missing/corrupt, rebuilding: $rootfs" >&2
    fi
    rm -f "$rootfs"

    command -v debugfs >/dev/null 2>&1 || { echo "error: install e2fsprogs (debugfs)" >&2; exit 1; }
    command -v cpio    >/dev/null 2>&1 || { echo "error: install cpio" >&2; exit 1; }

    local alpine_img
    alpine_img="$(locate_alpine_image)" || exit 1

    echo "=== Building Linux baseline initramfs from $alpine_img ==="

    local tmpdir="$LINUX_DIR/rootfs-build"
    rm -rf "$tmpdir"
    mkdir -p "$tmpdir"

    # 从 Alpine ext4 镜像完整提取根文件系统（含 busybox/iperf3/ip/nc 及 musl 库）。
    debugfs -R "rdump / $tmpdir" "$alpine_img" >/dev/null 2>&1 || {
        echo "error: debugfs rdump failed on $alpine_img" >&2
        exit 1
    }

    # 校验关键依赖确实存在，避免打包出无法运行测试的 initramfs。
    local missing=0 dep
    for dep in bin/busybox usr/bin/iperf3; do
        [[ -e "$tmpdir/$dep" ]] || { echo "error: $dep missing in extracted rootfs" >&2; missing=1; }
    done
    [[ "$missing" -eq 0 ]] || exit 1

    # 注入 init 脚本（busybox ash 兼容：无 brace 展开，循环用 seq）。
    cat > "$tmpdir/init" <<'INIT_SCRIPT'
#!/bin/sh
# Linux baseline init script (busybox ash compatible)

mount -t proc none /proc 2>/dev/null
mount -t sysfs none /sys 2>/dev/null
mount -t devtmpfs none /dev 2>/dev/null

# 配置网络（拓扑对齐 Starry：192.168.100.2/24，网关 .1）
ip link set lo up
ip link set eth0 up
ip addr add 192.168.100.2/24 dev eth0
ip route add default via 192.168.100.1

echo "=== Linux baseline: starting net-bench tests ==="
HOST_IP="192.168.100.1"

run_test() {
    test_id="$1"
    shift
    echo "NET_BENCH_BEGIN test=$test_id iter=0 warmup=1"
    iperf3 -c "$HOST_IP" -t 10 -J "$@" || true
    echo "NET_BENCH_END test=$test_id iter=0"

    for iter in 1 2 3 4 5; do
        echo "NET_BENCH_BEGIN test=$test_id iter=$iter warmup=0"
        iperf3 -c "$HOST_IP" -t 10 -J "$@"
        echo "NET_BENCH_END test=$test_id iter=$iter"
    done
}

# 等待 host iperf3 server 就绪
for i in $(seq 1 15); do
    if nc -z "$HOST_IP" 5201 2>/dev/null; then
        echo "=== iperf3 server ready ==="
        break
    fi
    sleep 1
done

run_test tcp1  -P 1
run_test tcp4  -P 4
run_test tcp1r -P 1 -R
run_test udp1g -u -b 1G
run_test udp64 -u -b 0 -l 64

echo "NET_BENCH_PASSED"
poweroff -f
INIT_SCRIPT

    chmod +x "$tmpdir/init"

    # 打包成 cpio.gz
    ( cd "$tmpdir" && find . | cpio -o -H newc 2>/dev/null | gzip ) > "$rootfs"

    # 自校验：确保新生成的 initramfs 真实可用
    validate_initramfs "$rootfs" || {
        echo "error: freshly built initramfs failed validation: $rootfs" >&2
        exit 1
    }

    echo "=== Linux initramfs created: $rootfs ($(du -h "$rootfs" | cut -f1)) ==="
}

# 运行 Linux baseline 测试
run_linux_test() {
    local repeat_id="$1"
    local smp="${2:-1}"
    local log_file="$RESULTS_DIR/linux-baseline-${ARCH}-${SCENARIO}-${TIMESTAMP}-r${repeat_id}.txt"
    
    echo "=== Linux baseline test (repeat $repeat_id/$REPEAT, smp=$smp) ===" | tee "$log_file"
    
    # 解析 Linux 内核镜像：优先使用 baseline 目录内显式放置的内核，
    # 否则在 host 架构与目标架构一致时回退到 host 内核；不一致则明确报错。
    local kernel_img=""
    if [[ -f "$LINUX_DIR/vmlinuz" ]]; then
        kernel_img="$LINUX_DIR/vmlinuz"
    elif [[ "$(uname -m)" == "aarch64" && -f "/boot/vmlinuz-$(uname -r)" ]]; then
        kernel_img="/boot/vmlinuz-$(uname -r)"
    fi
    if [[ -z "$kernel_img" ]]; then
        echo "error: no usable aarch64 Linux kernel found" >&2
        echo "hint: place an aarch64 kernel at $LINUX_DIR/vmlinuz, or run on an aarch64 host" >&2
        return 1
    fi
    echo "=== Using kernel: $kernel_img ===" | tee -a "$log_file"
    
    # QEMU 参数（对齐 qemu/vhost-aarch64-kvm.toml 的拓扑）
    local qemu_cmd=(
        qemu-system-aarch64
        -machine virt -cpu host
        -accel kvm
        -m 2G
        -smp "$smp"
        -kernel "$kernel_img"
        -initrd "$LINUX_DIR/initramfs.cpio.gz"
        -append "console=ttyAMA0 quiet"
        -device "virtio-net-pci,netdev=net0,mac=52:54:00:12:34:57,mq=on,vectors=10,csum=on,gso=on,host_tso4=on,host_tso6=on,guest_tso4=on,guest_tso6=on"
        -netdev "tap,id=net0,ifname=tap0,script=no,downscript=no,vhost=on,queues=4"
        -nographic
        -serial mon:stdio
    )
    
    # 启动 iperf3 server
    echo "=== Starting iperf3 server on $TAP_HOST_IP:5201 ===" | tee -a "$log_file"
    iperf3 -s -p 5201 -B "$TAP_HOST_IP" &
    local iperf_pid=$!
    trap "kill $iperf_pid 2>/dev/null || true" EXIT
    
    sleep 2
    
    # 运行 QEMU
    echo "=== Running Linux guest ===" | tee -a "$log_file"
    timeout 300 "${qemu_cmd[@]}" 2>&1 | tee -a "$log_file" || {
        local ret=$?
        if [[ $ret -eq 124 ]]; then
            echo "error: QEMU timeout after 300s" >&2
        fi
        kill $iperf_pid 2>/dev/null || true
        return 1
    }
    
    kill $iperf_pid 2>/dev/null || true
    trap - EXIT
    
    echo "=== Test complete: $log_file ===" | tee -a "$log_file"
}

# 主流程
main() {
    if [[ "${positional[0]:-}" == "help" ]]; then
        usage
        exit 0
    fi
    
    if [[ "$ARCH" != "aarch64" ]]; then
        echo "error: only aarch64 is supported" >&2
        exit 1
    fi
    
    check_prereq
    prepare_linux_rootfs
    
    # 确定 smp 参数
    local smp=1
    if [[ "$SCENARIO" == "vhost-smp4" ]]; then
        smp=4
    fi
    
    # 写环境指纹
    local fingerprint="$RESULTS_DIR/fingerprint-linux-baseline-${ARCH}-${SCENARIO}-${TIMESTAMP}.txt"
    {
        echo "# Linux baseline environment fingerprint"
        echo "timestamp   : $TIMESTAMP"
        echo "arch        : $ARCH"
        echo "scenario    : $SCENARIO (Linux guest)"
        echo "repeat      : $REPEAT"
        echo "smp         : $smp"
        echo "host_uname  : $(uname -a)"
        echo "host_nproc  : $(nproc 2>/dev/null || echo '?')"
        echo "qemu        : $(qemu-system-"$ARCH" --version 2>/dev/null | head -1)"
        echo "iperf3      : $(iperf3 --version 2>/dev/null | head -1)"
        echo "kvm         : present"
        echo "vhost_net   : present"
        echo "topology    : same as Starry (vhost-net + KVM + virtio-net-pci)"
    } > "$fingerprint"
    echo "=== Linux baseline fingerprint -> $fingerprint ==="
    cat "$fingerprint"
    
    # 运行测试
    for r in $(seq 1 "$REPEAT"); do
        run_linux_test "$r" "$smp"
    done
    
    # 汇总结果
    echo "=== Summarizing Linux baseline results ==="
    python3 "$SCRIPT_DIR/core/summarize.py" "$RESULTS_DIR"/linux-baseline-${ARCH}-${SCENARIO}-${TIMESTAMP}-r*.txt \
        > "$RESULTS_DIR/summary-linux-baseline-${ARCH}-${SCENARIO}-${TIMESTAMP}.txt"
    
    cat "$RESULTS_DIR/summary-linux-baseline-${ARCH}-${SCENARIO}-${TIMESTAMP}.txt"
    
    echo ""
    echo "=== Linux baseline test complete ==="
    echo "Results: $RESULTS_DIR/summary-linux-baseline-${ARCH}-${SCENARIO}-${TIMESTAMP}.txt"
    echo ""
    echo "Next: Compare with Starry baseline using the comparison script"
}

main "$@"
