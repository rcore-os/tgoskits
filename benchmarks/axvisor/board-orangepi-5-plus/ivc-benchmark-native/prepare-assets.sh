#!/bin/sh
set -eu

: "${AXBUILD_TARGET_DIR:?host_prepare did not provide AXBUILD_TARGET_DIR}"

workspace=$(pwd -P)
case_rel=benchmarks/axvisor/board-orangepi-5-plus/ivc-benchmark-native
output_dir="$AXBUILD_TARGET_DIR/axbuild/axvisor/board/orangepi-5-plus/ivc-benchmark-native"
bar2_cache=${AXIVC_BENCH_BAR2_CACHE-non-cacheable}

case "$bar2_cache" in
    non-cacheable | write-back) ;;
    *)
        echo "invalid AXIVC_BENCH_BAR2_CACHE value: $bar2_cache (expected non-cacheable or write-back)" >&2
        exit 1
        ;;
esac

case "$output_dir" in
    "$AXBUILD_TARGET_DIR"/axbuild/axvisor/board/*) ;;
    *)
        echo "refusing to replace unexpected output path: $output_dir" >&2
        exit 1
        ;;
esac

for command in \
    awk clang cp cargo dirname fdtget fdtput install mkdir mktemp readelf \
    rust-lld sed sha256sum
do
    if ! command -v "$command" >/dev/null 2>&1; then
        echo "missing required command: $command" >&2
        exit 1
    fi
done

mkdir -p "$(dirname "$output_dir")"
work=$(mktemp -d "$(dirname "$output_dir")/.prepare.XXXXXX")
trap 'rm -rf "$work"' EXIT HUP INT TERM

artifacts="$work/tgosimages"
mkdir -p "$artifacts"

if [ -n "${AXIVC_BENCH_ARTIFACT_DIR:-}" ]; then
    artifact_dir=$(CDPATH= cd -- "$AXIVC_BENCH_ARTIFACT_DIR" && pwd -P)
    echo "using AXIVC benchmark artifacts from $artifact_dir"
    cp -R "$artifact_dir/." "$artifacts/"
elif [ -n "${TGOSIMAGES_DIR:-${TGOSIMAGES_ROOT:-}}" ]; then
    tgosimages_dir=${TGOSIMAGES_DIR:-${TGOSIMAGES_ROOT:-}}
    tgosimages_dir=$(CDPATH= cd -- "$tgosimages_dir" && pwd -P)
    builder="$tgosimages_dir/scripts/apps/ivc-rk3588.sh"
    if [ ! -x "$builder" ]; then
        echo "tgosimages IVC builder is missing or not executable: $builder" >&2
        exit 1
    fi
    echo "building AXIVC benchmark artifacts with $builder"
    "$builder" \
        --tgoskits-src-dir "$workspace" \
        --stage-dir "$artifacts" \
        --bench-iterations 100
else
    echo "set AXIVC_BENCH_ARTIFACT_DIR to tgosimages output, or TGOSIMAGES_DIR to a checkout with scripts/apps/ivc-rk3588.sh" >&2
    exit 1
fi

if [ ! -f "$artifacts/SHA256SUMS" ]; then
    echo "AXIVC benchmark artifacts do not contain SHA256SUMS" >&2
    exit 1
fi

zephyr_elf="$artifacts/guest/zephyr/zephyr-ivc-benchmark.elf"
zephyr_bin="$artifacts/guest/zephyr/zephyr-ivc-benchmark.bin"
zephyr_dtb="$artifacts/guest/zephyr/zephyr-ivc-benchmark.dtb"
starry_bin="$artifacts/guest/starry/orangepi-5-plus"
bench_bin="$artifacts/usr/bin/ivc-starry-bench"
required_artifacts='artifact-manifest.toml
guest/starry/orangepi-5-plus
guest/zephyr/zephyr-ivc-benchmark.bin
guest/zephyr/zephyr-ivc-benchmark.elf
guest/zephyr/zephyr-ivc-benchmark.dtb
usr/bin/ivc-starry-bench'

while IFS= read -r path; do
    if ! awk -v path="$path" '$2 == path { found = 1 } END { exit found ? 0 : 1 }' \
        "$artifacts/SHA256SUMS"
    then
        echo "SHA256SUMS does not cover required AXIVC input: $path" >&2
        exit 1
    fi
done <<EOF
$required_artifacts
EOF
(cd "$artifacts" && sha256sum -c SHA256SUMS)

staged_zephyr_bin="$output_dir/tgosimages/guest/zephyr/zephyr-ivc-benchmark.bin"
staged_starry_bin="$output_dir/tgosimages/guest/starry/orangepi-5-plus"
staged_runtime_dtb="$output_dir/zephyr-runtime-template.dtb"
staged_initramfs="$output_dir/starry-ivc-benchmark-initramfs.cpio.gz"

for input in "$zephyr_elf" "$zephyr_bin" "$zephyr_dtb" "$starry_bin" "$bench_bin"; do
    if [ ! -s "$input" ]; then
        echo "missing or empty AXIVC benchmark artifact: $input" >&2
        exit 1
    fi
done

zephyr_entry=$(readelf -h "$zephyr_elf" |
    awk '/Entry point address:/ {print $4; exit}')
zephyr_load=$(readelf -W -l "$zephyr_elf" |
    awk '$1 == "LOAD" {print $4; exit}')
zephyr_vaddr=$(readelf -W -l "$zephyr_elf" |
    awk '$1 == "LOAD" {print $3; exit}')

if [ -z "$zephyr_entry" ] || [ -z "$zephyr_load" ] || [ -z "$zephyr_vaddr" ]; then
    echo "failed to parse Zephyr ELF entry/load address: $zephyr_elf" >&2
    exit 1
fi
if [ "$zephyr_load" != "$zephyr_vaddr" ]; then
    echo "Zephyr first LOAD physical/virtual addresses differ: $zephyr_load/$zephyr_vaddr" >&2
    exit 1
fi

runtime_dtb="$work/zephyr-runtime-template.dtb"
cp "$zephyr_dtb" "$runtime_dtb"
# The Zephyr build links a compile-time PCI bridge for its own driver layout.
# AxVisor installs the runtime bridge from the resolved device graph and rejects
# a pre-existing bridge, so pass a template without that node.
if fdtget -p "$runtime_dtb" /pci@b000000 >/dev/null 2>&1; then
    fdtput -r "$runtime_dtb" /pci@b000000
fi
if fdtget -p "$runtime_dtb" /pci@b000000 >/dev/null 2>&1; then
    echo "failed to remove compile-time PCI bridge from $runtime_dtb" >&2
    exit 1
fi
# The compile-time PCI node backs this chosen property. AxVisor recreates the
# bridge, and leaving the old handle would preserve a dangling reference.
if fdtget "$runtime_dtb" /chosen zephyr,pcie-controller >/dev/null 2>&1; then
    fdtput -d "$runtime_dtb" /chosen zephyr,pcie-controller
fi
if fdtget "$runtime_dtb" /chosen zephyr,pcie-controller >/dev/null 2>&1; then
    echo "failed to remove compile-time chosen PCI reference from $runtime_dtb" >&2
    exit 1
fi
for node in \
    /interrupt-controller@fe600000 \
    /serial@feb50000 \
    /memory@40000000
do
    if ! fdtget -p "$runtime_dtb" "$node" >/dev/null 2>&1; then
        echo "Zephyr runtime template lacks Orange Pi node $node" >&2
        exit 1
    fi
done

initramfs="$work/starry-ivc-benchmark-initramfs.cpio.gz"
init_stage="$work/initramfs"
init_bin="$work/init"
mkdir -p "$init_stage/dev" "$init_stage/usr/bin"

clang \
    --target=aarch64-unknown-linux-musl \
    -std=gnu11 \
    -O2 \
    -Wall \
    -Wextra \
    -Werror \
    -ffreestanding \
    -fno-builtin \
    -fno-stack-protector \
    -fno-pic \
    -fno-pie \
    -fno-asynchronous-unwind-tables \
    -fno-unwind-tables \
    -nostdlib \
    -c "$case_rel/guest-init.c" \
    -o "$work/init.o"

rust-lld \
    -flavor gnu \
    -static \
    -e _start \
    "$work/init.o" \
    -o "$init_bin"

install -m 0755 "$init_bin" "$init_stage/init"
install -m 0755 "$bench_bin" "$init_stage/usr/bin/ivc-starry-bench"

CARGO_TARGET_DIR="$AXBUILD_TARGET_DIR" \
    cargo xtask image pack-initramfs "$init_stage" "$initramfs" --gzip

toml_quote() {
    printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'
}

cat >"$work/guest-starry.toml" <<EOF
[base]
id = 1
name = "starry-ivshmem-benchmark"
guest_type = "virtualized"
cpu_num = 1
phys_cpu_ids = [0x100]

[kernel]
entry_point = 0x4008_0000
kernel_load_addr = 0x4008_0000
kernel_path = "$(toml_quote "$staged_starry_bin")"
dtb_load_addr = 0x4000_0000
ramdisk_path = "$(toml_quote "$staged_initramfs")"
ramdisk_load_addr = 0x5000_0000
cmdline = "loglevel=7 rdinit=/init"
memory_regions = [
  [0x4000_0000, 0x4000_0000, 0x7, 0],
]

[devices]
passthrough = []
disabled = []

[[devices.virtual]]
id = "ivshmem0"
model = "ivshmem-pci"
link_id = 6
peer_id = 1
max_peers = 2
common_size = 0
output_size = 0xc00000
bar2_cache = "$bar2_cache"
EOF

cat >"$work/guest-zephyr.toml" <<EOF
[base]
id = 2
name = "zephyr-ivshmem-benchmark"
guest_type = "virtualized"
cpu_num = 1
phys_cpu_ids = [0x200]

[kernel]
entry_point = $zephyr_entry
kernel_load_addr = $zephyr_load
kernel_path = "$(toml_quote "$staged_zephyr_bin")"
dtb_load_addr = 0x4700_0000
dtb_path = "$(toml_quote "$staged_runtime_dtb")"
memory_regions = [
  [0x4000_0000, 0x0800_0000, 0x7, 0],
]

[devices]
passthrough = []
disabled = []

[[devices.virtual]]
id = "ivshmem0"
model = "ivshmem-pci"
link_id = 6
peer_id = 0
max_peers = 2
common_size = 0
output_size = 0xc00000
bar2_cache = "$bar2_cache"
EOF

rm -rf "$output_dir"
mv "$work" "$output_dir"
trap - EXIT HUP INT TERM

echo "prepared native AXIVC benchmark fixture at $output_dir"
echo "Zephyr entry=$zephyr_entry load=$zephyr_load"
echo "BAR2 cache=$bar2_cache"
