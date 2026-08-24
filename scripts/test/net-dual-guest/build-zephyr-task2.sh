#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
default_zephyr_base="/home/huhu/toolchains/zephyrproject/zephyr-dccb09599635bdff17633fa7e9dab014b91dce90"
if [[ ! -d "$default_zephyr_base" && -d /tmp/zephyrproject/zephyr-4.4.2 ]]; then
    default_zephyr_base="/tmp/zephyrproject/zephyr-4.4.2"
fi
zephyr_base="${ZEPHYR_BASE:-$default_zephyr_base}"
cross_prefix="${CROSS_COMPILE:-/home/huhu/.local/toolchains/aarch64-linux-musl-cross/bin/aarch64-linux-musl-}"
out_dir="${OUT_DIR:-$repo_root/tmp/net-dual-guest/zephyr-task2}"
build_dir="${BUILD_DIR:-$out_dir/cargo-target}"
source_dir="$repo_root/scripts/test/net-dual-guest/zephyr-task2"
memory_base="${TASK2_ZEPHYR_MEMORY_BASE:-0xA0000000}"
memory_size="${TASK2_ZEPHYR_MEMORY_SIZE:-0x08000000}"
virtio_slot="${TASK2_ZEPHYR_VIRTIO_SLOT:-30}"
fault_mode="${TASK2_FAULT_MODE:-none}"
extra_overlay="${TASK2_ZEPHYR_EXTRA_OVERLAY:-}"
periodic_sample_count="${TASK1_ZEPHYR_SAMPLE_COUNT:-300}"
case "$virtio_slot" in
    0)
        device_overlay="$source_dir/app.overlay.switch"
        fdt_path="/virtio_mmio@a000000"
        host_hwirq=16
        guest_irq=48
        ;;
    30)
        device_overlay="$source_dir/app.overlay"
        fdt_path="/virtio_mmio@a003c00"
        host_hwirq=46
        guest_irq=78
        ;;
    *)  printf 'error: TASK2_ZEPHYR_VIRTIO_SLOT must be 0 or 30\n' >&2; exit 1 ;;
esac
case "$fault_mode" in
    none)            fault_define=0 ;;
    drop-ack-once)   fault_define=1 ;;
    drop-ack-always) fault_define=2 ;;
    *) printf 'error: TASK2_FAULT_MODE must be none, drop-ack-once, or drop-ack-always\n' >&2; exit 1 ;;
esac

case "$memory_base" in
    0x*) ;;
    *) printf 'error: TASK2_ZEPHYR_MEMORY_BASE must be hexadecimal\n' >&2; exit 1 ;;
esac
case "$memory_size" in
    0x*) ;;
    *) printf 'error: TASK2_ZEPHYR_MEMORY_SIZE must be hexadecimal\n' >&2; exit 1 ;;
esac
if [[ ! "$periodic_sample_count" =~ ^[1-9][0-9]*$ ]]; then
    printf 'error: TASK1_ZEPHYR_SAMPLE_COUNT must be a positive integer\n' >&2
    exit 1
fi
memory_base_value=$((memory_base))
memory_size_value=$((memory_size))
memory_base="$(printf '0x%08x' "$memory_base_value")"
memory_size="$(printf '0x%08x' "$memory_size_value")"

if [[ ! -d "$zephyr_base" || ! -x "${cross_prefix}gcc" ]]; then
    printf 'error: Zephyr source or cross compiler is missing (%s, %s)\n' \
        "$zephyr_base" "${cross_prefix}gcc" >&2
    exit 1
fi
if [[ -n "$extra_overlay" && ! -f "$extra_overlay" ]]; then
    printf 'error: Zephyr extra overlay does not exist: %s\n' "$extra_overlay" >&2
    exit 1
fi

mkdir -p "$out_dir"
memory_overlay="$out_dir/memory.overlay"
printf '/* Generated; keep Zephyr physical, virtual and DMA addresses identical. */\n&sram0 {\n\treg = <0x0 %s 0x0 %s>;\n};\n' \
    "$memory_base" "$memory_size" > "$memory_overlay"
overlay_files=("$device_overlay" "$memory_overlay")
if [[ -n "$extra_overlay" ]]; then
    overlay_files+=("$(realpath "$extra_overlay")")
fi
overlay_list="$(IFS=";"; printf "%s" "${overlay_files[*]}")"

# Zephyr 4.4.2's no-west path creates Kconfig.modules but omits this companion
# file. Seed the empty environment required by a standalone checkout; a real
# west/module discovery run overwrites it when modules are present.
mkdir -p "$build_dir/Kconfig"
printf 'set(kconfig_env_dirs)\n' > "$build_dir/Kconfig/kconfig_module_dirs.cmake"

ZEPHYR_BASE="$zephyr_base" cmake -S "$source_dir" -B "$build_dir" -G Ninja \
    -DBOARD=qemu_cortex_a53 \
    -DZEPHYR_TOOLCHAIN_VARIANT=cross-compile \
    -DCROSS_COMPILE="$cross_prefix" \
    -DDTC_OVERLAY_FILE="$overlay_list" \
    -DRT_SAMPLE_COUNT="$periodic_sample_count" \
    -DEXTRA_CFLAGS:STRING="-DCONFIG_MAX_IRQ_LINES=64 -DTASK2_FAULT_DROP_ACK_MODE=$fault_define"
cmake --build "$build_dir" --clean-first

elf="$out_dir/zephyr-task2.elf"
binary="$out_dir/zephyr-task2.bin"
cp "$build_dir/zephyr/zephyr.elf" "$elf"
objcopy_bin="${OBJCOPY:-${cross_prefix}objcopy}"
if [[ ! -x "$objcopy_bin" ]]; then
    printf 'error: objcopy is not executable: %s\n' "$objcopy_bin" >&2
    exit 1
fi
"$objcopy_bin" -O binary "$elf" "$binary"
chmod 0755 "$elf" "$binary"
readelf_bin="${READELF:-${cross_prefix}readelf}"
if [[ ! -x "$readelf_bin" ]]; then
    readelf_bin="$(command -v readelf || printf '')"
fi
if [[ -z "$readelf_bin" ]]; then
    printf 'error: no usable readelf found\n' >&2
    exit 1
fi
entry_point="$(LC_ALL=C "$readelf_bin" -hW "$elf" | awk '/Entry point address:/ {print $4}')"
linked_base="$(LC_ALL=C "$readelf_bin" -lW "$elf" | awk '$1 == "LOAD" {print $3; exit}')"
if [[ -z "$entry_point" || -z "$linked_base" ]]; then
    printf 'error: failed to extract Zephyr ELF entry/link base\n' >&2
    exit 1
fi
linked_base_value=$((linked_base))
if (( linked_base_value != memory_base_value )); then
    printf 'error: Zephyr linked base %s does not match requested %s\n' "$linked_base" "$memory_base" >&2
    exit 1
fi
linked_base="$(printf '0x%08x' "$linked_base_value")"
sha256="$(sha256sum "$binary" | awk '{print $1}')"
elf_sha256="$(sha256sum "$elf" | awk '{print $1}')"
git_sha="$(git -C "$repo_root" rev-parse HEAD)"
zephyr_version="$(git -C "$zephyr_base" rev-parse HEAD 2>/dev/null || printf 'unversioned')"
manifest="$out_dir/manifest.toml"
{
    printf '# Generated by scripts/test/net-dual-guest/build-zephyr-task2.sh\n'
    printf 'git_head = "%s"\n' "$git_sha"
    printf 'zephyr_base = "%s"\n' "$zephyr_base"
    printf 'zephyr_commit = "%s"\n' "$zephyr_version"
    printf 'board = "qemu_cortex_a53"\n'
    printf 'image_format = "raw"\n'
    printf 'memory_base = "%s"\n' "$memory_base"
    printf 'memory_size = "%s"\n' "$memory_size"
    printf 'elf_entry = "%s"\n' "$entry_point"
    printf 'linked_base = "%s"\n' "$linked_base"
    printf 'fdt_path = "%s"\n' "$fdt_path"
    printf 'host_hwirq = %s\n' "$host_hwirq"
    printf 'guest_irq = %s\n' "$guest_irq"
    printf 'fault_mode = "%s"\n' "$fault_mode"
    printf 'periodic_probe = "enabled"\n'
    printf 'periodic_sample_count = "%s"\n' "$periodic_sample_count"
    printf 'extra_overlay = "%s"\n' "${extra_overlay:-none}"
    if [[ -n "$extra_overlay" ]]; then
        printf 'extra_overlay_sha256 = "%s"\n' "$(sha256sum "$extra_overlay" | awk '{print $1}')"
    fi
    printf 'toolchain = "%s"\n' "$("${cross_prefix}gcc" -dumpfullversion)"
    printf 'sha256 = "%s"\n' "$sha256"
    printf 'elf_sha256 = "%s"\n' "$elf_sha256"
    printf 'elf_path = "%s"\n' "$elf"
    printf 'path = "%s"\n' "$binary"
} > "$manifest"

printf 'elf=%s entry=%s linked_base=%s\n' "$elf" "$entry_point" "$linked_base"
printf 'binary=%s sha256=%s\n' "$binary" "$sha256"
printf 'manifest=%s\n' "$manifest"
