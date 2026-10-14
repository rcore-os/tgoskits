#!/usr/bin/env bash
set -euo pipefail

workspace=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
cd "$workspace"

case "${1:-}" in
    "")
        vcpus_mode=0
        board_config=os/axvisor/configs/board/qemu-aarch64-virtio-net-peer.toml
        qemu_config=os/axvisor/configs/qemu/qemu-aarch64-virtio-net-peer.toml
        ;;
    --vcpus)
        vcpus_mode=1
        board_config=os/axvisor/configs/board/qemu-aarch64-vcpus.toml
        qemu_config=os/axvisor/configs/qemu/qemu-aarch64-vcpus.toml
        ;;
    *)
        echo "usage: $0 [--vcpus]" >&2
        exit 2
        ;;
esac
if [[ $# -gt 1 ]]; then
    echo "usage: $0 [--vcpus]" >&2
    exit 2
fi

objcopy="${LLVM_OBJCOPY:-}"
if [[ -z "$objcopy" ]]; then
    objcopy="$(rustc --print sysroot)/lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-objcopy"
fi
if [[ ! -x "$objcopy" ]]; then
    echo "llvm-objcopy not found: $objcopy (set LLVM_OBJCOPY)" >&2
    exit 1
fi

local_ip_of_vm() {
    case "$1" in
        vm1) echo "10.0.2.15" ;;
        vm2) echo "10.0.2.16" ;;
        vm3) echo "10.0.2.17" ;;
        vm4) echo "10.0.2.18" ;;
        vm5) echo "10.0.2.19" ;;
        *)
            echo "no AXVIRTIO_LOCAL_IP mapping for guest '$1'" >&2
            return 1
            ;;
    esac
}

# Default for the client connect start delay (`AXVIRTIO_CONNECT_START_DELAY_MS`).
# The two QEMU scenarios sequence guests differently:
#   * default mode: the QEMU config attaches consoles head first
#     (VM1 -> VM2 -> VM3 -> VM4) while AxVisor auto-starts every guest before the
#     shell is available, so those steps only choose which console is attached
#     and foregrounded; attaching neither starts a guest nor changes boot order.
#   * `--vcpus` mode: the QEMU config enables `no-auto-start` and issues explicit
#     `vm start` commands, so it genuinely controls the guest start and attach
#     order (tail first: VM4, then VM3, then VM2, then VM1).
# In both modes the delay gives the just-attached guest time to finish its
# network setup before its client dials the next hop.
# VM4 is the tail of the chain and its peer 10.0.2.19 is intentionally absent.
# Dialing it inside either test window (240 s default, 360 s `--vcpus`, 600 s
# CI wrapper) floods the virtual switch with ARP pending frames for an address
# nobody owns, which is what filled the pending queue in #2599. 600000 ms sits
# past all three windows, so VM4 never dials here.
# That also means neither QEMU scenario executes the unreachable-peer retry
# (`connect_retry_backoff`, `VMx_VIRTIO_NET_CONNECT_RETRY`). The queue behavior
# that retry used to expose is covered by the ax-net pending-TTL test; the
# backoff itself is a pure function of the failure count.
connect_start_delay_of_vm() {
    case "$1" in
        vm1 | vm2 | vm3) echo "10000" ;;
        vm4) echo "600000" ;;
        *)
            echo "no AXVIRTIO_CONNECT_START_DELAY_MS mapping for guest '$1'" >&2
            return 1
            ;;
    esac
}

build_guest() {
    local vm="$1"
    local tag="${vm^^}"
    local config="apps/arceos/build-aarch64-virtio-net-peer.toml"
    local output="target/aarch64-unknown-linux-musl/release/arceos-virtio-net-peer-${vm}.bin"
    local local_ip
    local_ip="$(local_ip_of_vm "$vm")"
    local connect_start_delay
    connect_start_delay="$(connect_start_delay_of_vm "$vm")"
    local smp_args=()
    # VM1 in --vcpus mode boots guest IDs 0 and 2 on the same host pCPU;
    # the other images stay single-core, including the default QEMU mode.
    if [[ "$vm" == vm1 && "$vcpus_mode" == 1 ]]; then
        smp_args=(--smp 2)
    fi

    # The guests read the tag and the local address at compile time
    # (`option_env!`); the peer address is derived from the local address inside
    # the app (last octet plus one), so no peer variable is needed here.
    # `AXVIRTIO_HEARTBEAT_PERIOD_MS` (default 4000),
    # `AXVIRTIO_HEARTBEAT_PASS_AFTER` (default 3),
    # `AXVIRTIO_CONNECT_TIMEOUT_MS` (default 4000) and
    # `AXVIRTIO_CONNECT_START_DELAY_MS` (per-VM here: 10000 for VM1..VM3 and
    # 600000 for VM4) can be exported to override the tuning; a non-empty
    # exported `AXVIRTIO_CONNECT_START_DELAY_MS` replaces the per-VM default for
    # every guest.
    AXVIRTIO_VM_TAG="$tag" \
    AXVIRTIO_LOCAL_IP="$local_ip" \
    AXVIRTIO_CONNECT_START_DELAY_MS="${AXVIRTIO_CONNECT_START_DELAY_MS:-$connect_start_delay}" \
        cargo xtask arceos build -p arceos-virtio-net-peer -c "$config" "${smp_args[@]}"
    "$objcopy" --strip-all -O binary \
        target/aarch64-unknown-linux-musl/release/arceos-virtio-net-peer \
        "$output"
}

build_guest vm1
build_guest vm2
build_guest vm3
build_guest vm4

exec cargo xtask axvisor qemu \
    --config "$board_config" \
    --qemu-config "$qemu_config"
