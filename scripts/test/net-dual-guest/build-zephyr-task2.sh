#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source "$repo_root/scripts/lib/task123-tools.sh"
zephyr_revision="dccb09599635bdff17633fa7e9dab014b91dce90"
default_zephyr_base="$repo_root/.deps/zephyr-$zephyr_revision"
zephyr_base="${ZEPHYR_BASE:-$default_zephyr_base}"
cross_prefix="$(resolve_task123_cross_prefix)"
out_dir="${OUT_DIR:-$repo_root/tmp/net-dual-guest/zephyr-task2}"
build_dir="${BUILD_DIR:-$out_dir/cargo-target}"
source_dir="$repo_root/scripts/test/net-dual-guest/zephyr-task2"
memory_base="${TASK2_ZEPHYR_MEMORY_BASE:-0xA0000000}"
memory_size="${TASK2_ZEPHYR_MEMORY_SIZE:-0x08000000}"
virtio_slot="${TASK2_ZEPHYR_VIRTIO_SLOT:-30}"
fault_mode="${TASK2_FAULT_MODE:-none}"
extra_overlay="${TASK2_ZEPHYR_EXTRA_OVERLAY:-}"
periodic_sample_count="${TASK1_ZEPHYR_SAMPLE_COUNT:-300}"
# Production defaults to no per-packet UART formatting or output. Board
# diagnostics can opt in, while the periodic probe always enforces a quiet
# sampling window.
runtime_trace="${TASK2_RUNTIME_TRACE:-0}"
# Leave this unset for qemu_cortex_a53's native counter frequency. RK3588
# board builds set 24000000 explicitly so one source tree remains valid for
# both physical-board and QEMU experiments.
timer_frequency_hz="${TASK2_TIMER_FREQUENCY_HZ:-}"
case "$virtio_slot" in
    0)
        device_overlay="$source_dir/app.overlay.switch"
        fdt_path="/virtio_mmio@b000000"
        host_hwirq=0
        guest_irq=32
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
if [[ "$runtime_trace" != 0 && "$runtime_trace" != 1 ]]; then
    printf 'error: TASK2_RUNTIME_TRACE must be 0 or 1\n' >&2
    exit 1
fi
if [[ -n "$timer_frequency_hz" && ! "$timer_frequency_hz" =~ ^[1-9][0-9]*$ ]]; then
    printf 'error: TASK2_TIMER_FREQUENCY_HZ must be a positive integer\n' >&2
    exit 1
fi
memory_base_value=$((memory_base))
memory_size_value=$((memory_size))
memory_base="$(printf '0x%08x' "$memory_base_value")"
memory_size="$(printf '0x%08x' "$memory_size_value")"

if [[ ! -d "$zephyr_base" ]]; then
    printf 'error: Zephyr source is missing: %s; set ZEPHYR_BASE or place revision %s under .deps\n' \
        "$zephyr_base" "$zephyr_revision" >&2
    exit 1
fi
if [[ -n "$extra_overlay" && ! -f "$extra_overlay" ]]; then
    printf 'error: Zephyr extra overlay does not exist: %s\n' "$extra_overlay" >&2
    exit 1
fi

mkdir -p "$out_dir" "$build_dir"
out_dir="$(realpath "$out_dir")"
build_dir="$(realpath "$build_dir")"
memory_overlay="$out_dir/memory.overlay"
printf '/* Generated; keep Zephyr physical, virtual and DMA addresses identical. */\n&sram0 {\n\treg = <0x0 %s 0x0 %s>;\n};\n' \
    "$memory_base" "$memory_size" > "$memory_overlay"
overlay_files=("$device_overlay" "$memory_overlay")
if [[ -n "$extra_overlay" ]]; then
    overlay_files+=("$(realpath "$extra_overlay")")
fi
overlay_list="$(IFS=";"; printf "%s" "${overlay_files[*]}")"

extra_conf_args=("-DEXTRA_CONF_FILE=")
if [[ -n "$timer_frequency_hz" ]]; then
    timer_conf="$out_dir/timer-frequency.conf"
    printf 'CONFIG_SYS_CLOCK_HW_CYCLES_PER_SEC=%s\n' "$timer_frequency_hz" > "$timer_conf"
    extra_conf_args=("-DEXTRA_CONF_FILE=$timer_conf")
fi

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
    "${extra_conf_args[@]}" \
    -DEXTRA_CFLAGS:STRING="-DCONFIG_MAX_IRQ_LINES=64 -DTASK2_FAULT_DROP_ACK_MODE=$fault_define -DTASK2_RUNTIME_TRACE=$runtime_trace"
cmake --build "$build_dir" --clean-first

zephyr_config="$build_dir/zephyr/.config"
if ! grep -Fqx 'CONFIG_PRINTK_SYNC=y' "$zephyr_config" ||
   ! grep -Fqx '# CONFIG_LOG is not set' "$zephyr_config"; then
    printf 'error: Zephyr console serialization contract is not active in %s\n' \
        "$zephyr_config" >&2
    exit 1
fi
configured_timer_frequency="$(sed -n 's/^CONFIG_SYS_CLOCK_HW_CYCLES_PER_SEC=//p' "$zephyr_config")"
if [[ -z "$configured_timer_frequency" ]]; then
    printf 'error: no system timer frequency found in %s\n' "$zephyr_config" >&2
    exit 1
fi
if [[ -n "$timer_frequency_hz" && "$configured_timer_frequency" != "$timer_frequency_hz" ]]; then
    printf 'error: requested timer frequency %s, configured %s in %s\n' \
        "$timer_frequency_hz" "$configured_timer_frequency" "$zephyr_config" >&2
    exit 1
fi

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
    printf 'runtime_trace = "%s"\n' "$runtime_trace"
    printf 'timer_frequency_hz = %s\n' "$configured_timer_frequency"
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
