#!/usr/bin/env bash
# Build and run the OrangePi board case: the four ArceOS virtio-net peers that
# AxVisor runs as guests, AxVisor itself, `axvisor.bin`, then the board test.
#
# Every peer comes from the same package but bakes a different identity into the
# binary (`AXVIRTIO_VM_TAG` / `AXVIRTIO_LOCAL_IP`, read with `option_env!`), and a
# second build of the same package overwrites the first artifact. Each peer is
# therefore built separately and copied to its own `${vm}.bin` name, which is the
# path arceos-virtio-net-peer-${vm}.toml embeds into AxVisor via `include_bytes!`
# (`image_location = "memory"`).
#
# The guest identity follows apps/arceos/virtio-net-peer/run.sh: the tag is the
# upper-case VM name, and the address is the first address of the chain plus the
# VM index, so vm1..vm4 use 10.0.2.15 .. 10.0.2.18. Each peer connects to its own
# address plus one, which is why the guests form a chain of sessions.
#
# The guests are built first because AxVisor embeds them at build time, so a
# guest rebuild always has to be followed by an AxVisor rebuild.
set -euo pipefail

case_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# vcpus/ sits one level deeper than a plain case directory.
workspace=$(cd "${case_dir}/../../../../../.." && pwd)
cd "$workspace"

objcopy="${LLVM_OBJCOPY:-}"
if [[ -z "$objcopy" ]]; then
    objcopy="$(rustc --print sysroot)/lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-objcopy"
fi
if [[ ! -x "$objcopy" ]]; then
    echo "llvm-objcopy not found: $objcopy (set LLVM_OBJCOPY)" >&2
    exit 1
fi

# Address of guest N is the first address plus N-1, same mapping as
# apps/arceos/virtio-net-peer/run.sh. Keep this in sync with the chain documented
# in arceos-virtio-net-peer-vm{1..4}.toml.
local_ip_of_vm() {
    case "$1" in
        vm1) echo "10.0.2.15" ;;
        vm2) echo "10.0.2.16" ;;
        vm3) echo "10.0.2.17" ;;
        vm4) echo "10.0.2.18" ;;
        *)
            echo "no AXVIRTIO_LOCAL_IP mapping for guest '$1'" >&2
            return 1
            ;;
    esac
}

# ArceOS apps with `ax-std` are built for the std/PIE target, so their artifacts
# live under the musl target directory even though the config declares
# `aarch64-unknown-none-softfloat`.
artifact_dir="target/aarch64-unknown-linux-musl/release"

build_guest() {
    local vm="$1"
    local tag="${vm^^}"
    local local_ip
    local_ip="$(local_ip_of_vm "$vm")"
    local config="apps/arceos/build-aarch64-virtio-net-peer.toml"
    local output="${artifact_dir}/arceos-virtio-net-peer-${vm}.bin"

    AXVIRTIO_VM_TAG="$tag" \
    AXVIRTIO_LOCAL_IP="$local_ip" \
        cargo xtask arceos build -p arceos-virtio-net-peer -c "$config"
    "$objcopy" --strip-all -O binary \
        "${artifact_dir}/arceos-virtio-net-peer" \
        "$output"
    echo "installed guest image: ${output} (tag=${tag} local_ip=${local_ip})"
}

build_guest vm1
build_guest vm2
build_guest vm3
build_guest vm4

# AxVisor with the four guest images embedded (see the vm{1..4}.toml configs in
# this directory; the case-level build config belongs to the Linux peers).
cargo xtask axvisor build \
    -c test-suit/axvisor/normal/board-orangepi-5-plus/virtio-net-peer/vcpus/build-aarch64-unknown-none-softfloat.toml
"$objcopy" --strip-all -O binary "${artifact_dir}/axvisor" "${artifact_dir}/axvisor.bin"

# Build, upload and run on the board. The board name of this variant is
# `orangepi-5-plus-virtio-net-peer-vcpus`: `<board>-<guest>` for the U-Boot flow,
# `<board>` for the board-service flow.
# cargo xtask axvisor test board --board orangepi-5-plus-virtio-net-peer-vcpus
cargo xtask axvisor test uboot --board orangepi-5-plus --guest virtio-net-peer-vcpus
