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
  aarch64     TAP+vhost-net (qemu/vhost-aarch64-kvm.toml 拓扑)
  x86_64      TAP+vhost-net (qemu/vhost-x86_64-kvm.toml 拓扑)

scenario:
  vhost       TAP+vhost-net, smp=1 (主力拓扑)
  vhost-smp4  TAP+vhost-net, smp=4 (多核扩展)

options:
  --repeat N         重复测试 N 次（默认 1）
  --rebuild-rootfs   强制重建 Linux baseline initramfs（忽略已有缓存）

前置条件:
  1. vhost 环境已配置: sudo bash apps/starry/net-bench/bin/setup
  2. iperf3 已安装在 guest（通过 init script 或 rootfs）
  3. guest 内核：$LINUX_DIR/vmlinuz；x86_64 缺失时自动
     通过 apt-get download 拉取 Ubuntu generic 内核（virtio 均为 built-in）

测试拓扑对齐 (qemu-plan §6.1):
  - 相同的 vhost-net + KVM + virtio-net-pci
  - 相同的网络 IP 配置（192.168.100.1/24）
  - 相同的测试负载与参数（镜像 net-bench-common.sh：iperf3 -t 5，
    tcp1/tcp4/tcp1r/udp1g/udp64，warmup 1 + 测量 5）
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
# 兼容平铺 (rootfs/<name>) 与嵌套 (rootfs/<name>/<name>) 两种受管布局；
# 缺失时尝试通过 xtask 拉取；仍失败则明确报错。
locate_alpine_image() {
    local image_name="rootfs-${ARCH}-alpine.img"
    local flat="$WORKSPACE/tmp/axbuild/rootfs/$image_name"
    local nested="$WORKSPACE/tmp/axbuild/rootfs/$image_name/$image_name"

    if [[ ! -f "$flat" && ! -f "$nested" ]]; then
        echo "=== Alpine rootfs image missing, fetching via xtask ===" >&2
        ( cd "$WORKSPACE" && cargo xtask starry rootfs --arch "$ARCH" ) >&2 || {
            echo "error: failed to ensure Alpine rootfs via 'cargo xtask starry rootfs --arch $ARCH'" >&2
            return 1
        }
    fi
    local img
    for img in "$flat" "$nested"; do
        if [[ -f "$img" ]]; then
            printf '%s\n' "$img"
            return 0
        fi
    done
    echo "error: managed Alpine rootfs not found at $flat (or $nested)" >&2
    return 1
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
PORT=5201
DURATION=5

# Mirror net-bench-common.sh run_test — keep in sync (duration/port/loads).
run_test() {
    test_id="$1"
    shift
    local warmup_iters=1
    local measured_iters=5
    local iter=0
    local total=$((warmup_iters + measured_iters))
    while [ "$iter" -lt "$total" ]; do
        if [ "$iter" -lt "$warmup_iters" ]; then
            warm=1
        else
            warm=0
        fi
        echo "NET_BENCH_BEGIN test=$test_id iter=$iter warmup=$warm"
        echo "NET_STATS_BEGIN warmup=$warm"
        cat /proc/net/dev
        echo "NET_STATS_END"
        if iperf3 -c "$HOST_IP" -p "$PORT" -t "$DURATION" -J "$@"; then
            echo "NET_BENCH_END test=$test_id iter=$iter"
        else
            echo "NET_BENCH_END test=$test_id iter=$iter"
            if [ "$warm" -eq 0 ]; then
                echo "NET_BENCH_FAILED: $test_id iteration $iter"
                exit 1
            fi
            echo "NET_BENCH_WARN: $test_id warmup iteration $iter failed (ignored)"
        fi
        echo "NET_STATS_BEGIN warmup=$warm"
        cat /proc/net/dev
        echo "NET_STATS_END"
        iter=$((iter + 1))
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

run_test tcp1
run_test tcp4  -P 4
run_test tcp1r -R
run_test udp1g -u -b 1G
run_test udp64 -u -l 64 -b 100M

echo "NET_BENCH_PASSED"
poweroff -f
INIT_SCRIPT

    chmod +x "$tmpdir/init"

    # 仅打包基准必需的最小集合。完整 Alpine 测试 rootfs（含 /opt、/guest 等
    # 测试资产，>1.5G）会超出 initramfs 解包内存预算：-m 2G 下内核报
    # "Initramfs unpacking failed: write error" 并 panic。最小集合 =
    # busybox(+applet 符号链接) + iperf3 及其共享库 + /etc 基础配置。
    local staging="$LINUX_DIR/initramfs-staging"
    rm -rf "$staging"
    mkdir -p "$staging"/{dev,proc,sys,tmp,run,root,var,usr/bin,usr/lib}
    cp -a "$tmpdir/bin" "$tmpdir/sbin" "$tmpdir/lib" "$tmpdir/etc" "$staging/"
    cp -a "$tmpdir/usr/bin/iperf3" "$staging/usr/bin/"
    [[ -e "$tmpdir/usr/bin/nc" ]] && cp -a "$tmpdir/usr/bin/nc" "$staging/usr/bin/"
    # libiperf.so.0 的 NEEDED 依赖链：libcrypto.so.3 + musl（musl 已随 /lib 拷贝）。
    cp -a "$tmpdir"/usr/lib/libiperf.so* "$tmpdir"/usr/lib/libcrypto.so* "$staging/usr/lib/"
    cp -a "$tmpdir/init" "$staging/init"

    # 打包成 cpio.gz
    ( cd "$staging" && find . | cpio -o -H newc 2>/dev/null | gzip ) > "$rootfs"

    # 自校验：确保新生成的 initramfs 真实可用
    validate_initramfs "$rootfs" || {
        echo "error: freshly built initramfs failed validation: $rootfs" >&2
        exit 1
    }

    echo "=== Linux initramfs created: $rootfs ($(du -h "$rootfs" | cut -f1)) ==="
}

# 确保 x86_64 guest 内核可用：优先 $LINUX_DIR/vmlinuz；缺失时通过
# apt-get download 拉取 Ubuntu generic 内核并解包（该内核 virtio_net/
# virtio_pci/failover 均为 built-in，无需向 initramfs 注入模块）。
ensure_x86_kernel() {
    local vmlinuz="$LINUX_DIR/vmlinuz"
    [[ -f "$vmlinuz" ]] && return 0

    command -v apt-get  >/dev/null 2>&1 || { echo "error: apt-get unavailable; place an x86_64 kernel at $vmlinuz" >&2; return 1; }
    command -v dpkg-deb >/dev/null 2>&1 || { echo "error: dpkg-deb unavailable; place an x86_64 kernel at $vmlinuz" >&2; return 1; }

    local pkg
    pkg="$(apt-cache search --names-only '^linux-image-unsigned-[0-9.-]+-generic$' \
        | awk '{print $1}' | sort -V | tail -1)"
    [[ -n "$pkg" ]] || { echo "error: no linux-image-unsigned-*-generic candidate found" >&2; return 1; }

    echo "=== Fetching x86_64 guest kernel: $pkg ===" >&2
    local dl_dir="$LINUX_DIR/kernel-pkg"
    rm -rf "$dl_dir"
    mkdir -p "$dl_dir"
    ( cd "$dl_dir" && apt-get download "$pkg" >&2 && dpkg-deb -x "$pkg"_*.deb extract/ ) || {
        echo "error: failed to download/extract $pkg" >&2
        return 1
    }
    local bz
    bz="$(find "$dl_dir/extract/boot" -maxdepth 1 -name 'vmlinuz-*' | head -1)"
    [[ -n "$bz" ]] || { echo "error: vmlinuz not found inside $pkg" >&2; return 1; }
    cp "$bz" "$vmlinuz"
    basename "$bz" | sed 's/^vmlinuz-//' > "$LINUX_DIR/kernel-version"
    rm -rf "$dl_dir"
    echo "=== x86_64 guest kernel ready: $vmlinuz ($(cat "$LINUX_DIR/kernel-version")) ===" >&2
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
    elif [[ "$(uname -m)" == "$ARCH" && -f "/boot/vmlinuz-$(uname -r)" ]]; then
        kernel_img="/boot/vmlinuz-$(uname -r)"
    fi
    if [[ -z "$kernel_img" ]]; then
        echo "error: no usable $ARCH Linux kernel found" >&2
        echo "hint: place a $ARCH kernel at $LINUX_DIR/vmlinuz, or run on a $ARCH host" >&2
        return 1
    fi
    echo "=== Using kernel: $kernel_img ===" | tee -a "$log_file"

    # QEMU 参数：网络设备/后端与 Starry 对应 vhost-<arch>-kvm.toml 完全一致
    # （plain virtio-net-pci + tap,vhost=on，不额外附加 mq/vectors/offload）。
    local qemu_cmd=(
        "qemu-system-$ARCH"
        -cpu host
        -accel kvm
        -m 2G
        -smp "$smp"
        -kernel "$kernel_img"
        -initrd "$LINUX_DIR/initramfs.cpio.gz"
        -device "virtio-net-pci,netdev=net0,mac=52:54:00:12:34:57"
        -netdev "tap,id=net0,ifname=tap0,script=no,downscript=no,vhost=on"
        -nographic
        -serial mon:stdio
        -no-reboot
    )
    # panic=-1: 内核 panic（如 init 失败退出）立即触发重启请求，配合
    # -no-reboot 让 QEMU 直接退出，避免失败场景空耗 300s 超时。
    case "$ARCH" in
        aarch64)
            qemu_cmd+=(-machine virt -append "console=ttyAMA0 quiet panic=-1")
            ;;
        x86_64)
            qemu_cmd+=(-append "console=ttyS0 quiet panic=-1")
            ;;
    esac

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

    case "$ARCH" in
        aarch64|x86_64) ;;
        *)
            echo "error: unsupported arch '$ARCH' (aarch64|x86_64)" >&2
            exit 1
            ;;
    esac

    check_prereq
    if [[ "$ARCH" == "x86_64" ]]; then
        ensure_x86_kernel || exit 1
    fi
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
        echo "guest_kernel: $(cat "$LINUX_DIR/kernel-version" 2>/dev/null || echo 'external (see run log)')"
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
