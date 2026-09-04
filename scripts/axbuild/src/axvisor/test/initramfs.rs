//! Minimal initramfs generation for Axvisor QEMU tests.
//!
//! Timer and interrupt-controller tests must not depend on a guest block
//! device reaching its interrupt handler. A build group can therefore request
//! a small BusyBox initramfs generated from the managed architecture rootfs.

use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, bail, ensure};
use flate2::{Compression, write::GzEncoder};
use ostool::build::config::Cargo;
use tempfile::NamedTempFile;

use crate::{axvisor::rootfs, context::ResolvedAxvisorRequest, rootfs::inject::read_binary_file};

const OUTPUT_ENV: &str = "AXVISOR_TEST_BUSYBOX_INITRAMFS";
const OVMF_OUTPUT_ENV: &str = "AXVISOR_TEST_X86_OVMF_OUTPUT";
const BUSYBOX_PATH: &str = "/bin/busybox";
// These bounds are the fixed Q35 guest aperture used by the x86 AxVM provider;
// the end bound is exclusive.
// Keep them synchronized with `virtualization/axvm/src/arch/x86_64/pci_config.rs`.
const X86_PCI_MEMORY_APERTURE_START: &str = "0xc0000000";
const X86_PCI_MEMORY_APERTURE_END: &str = "0xd0000000";
const INIT_SCRIPT_TEMPLATE: &str = r#"#!/bin/busybox sh
/bin/busybox mount -t devtmpfs devtmpfs /dev 2>/dev/null || true
/bin/busybox mount -t proc proc /proc 2>/dev/null || true
/bin/busybox mount -t sysfs sysfs /sys 2>/dev/null || true
export HOME=/root
export PATH=/bin
export TERM=vt100
export PS1='~ # '
cd /root

run_x86_acpi_check() {
  success_marker=$1
  failed=0
  # Linux uses the XSDT to discover these tables but does not export the root
  # XSDT itself through /sys/firmware/acpi/tables.
  for table in DSDT APIC FACP SPCR; do
    if [ ! -r "/sys/firmware/acpi/tables/$table" ]; then
      echo "missing readable ACPI table: $table"
      failed=1
    fi
  done
  online=$(/bin/busybox cat /sys/devices/system/cpu/online 2>/dev/null)
  if [ "$online" != "0" ]; then
    echo "unexpected online CPU set: $online"
    failed=1
  fi
  if [ ! -e /sys/class/tty/ttyS0 ]; then
    echo "missing ttyS0"
    failed=1
  fi
  if ! /bin/busybox grep -q 'IO-APIC' /proc/interrupts; then
    echo "Linux did not initialize an IOAPIC"
    failed=1
  fi
  if [ "$failed" -ne 0 ]; then
    echo AXVISOR_X86_ACPI_FAILED
  else
    echo "$success_marker"
  fi
}

__AXVISOR_PCI_CONFIG_READERS__
__AXVISOR_PCI_BAR_VALIDATOR__
__AXVISOR_PCI_CAPABILITY_VALIDATOR__

run_pci_enumeration_check() {
  success_marker=$1
  failed=0
  # The managed rootfs images ship no pciutils, so this check consumes the
  # kernel-published sysfs PCI state directly (the same source `lspci` reads).
  bdf=""
  count=0
  for dev in /sys/bus/pci/devices/*; do
    [ -d "$dev" ] || continue
    vendor=$(/bin/busybox cat "$dev/vendor")
    device=$(/bin/busybox cat "$dev/device")
    class=$(/bin/busybox cat "$dev/class")
    if [ "$vendor" = "0x1af4" ] && [ "$device" = "0x1110" ] && [ "$class" = "0x050000" ]; then
      bdf=${dev##*/}
      count=$((count + 1))
    fi
  done
  echo "guest kernel: $(/bin/busybox uname -r)"
  if [ "$count" -ne 1 ]; then
    echo "expected exactly one 0500:1af4:1110 function, found $count"
    failed=1
  else
    endpoint=/sys/bus/pci/devices/$bdf
    if [ -L "$endpoint/driver" ]; then
      echo "vPCI endpoint unexpectedly bound to a driver"
      failed=1
    fi
    # The kernel publishes `resource` entries as bare hex columns; tolerate an
    # optional 0x prefix and turn malformed input into an explicit failure so
    # the case reports FAILED instead of dying inside arithmetic.
    resource_line=$(/bin/busybox sed -n '3p' "$endpoint/resource")
    if ! validate_pci_bar_resource "$resource_line" __AXVISOR_PCI_MEMORY_APERTURE_START__ __AXVISOR_PCI_MEMORY_APERTURE_END__ 65536; then
      failed=1
      echo "BAR2 is not a valid 64 KiB memory resource"
    fi
    if ! validate_pci_capabilities "$endpoint/config"; then
      failed=1
    fi
    fi
  if [ "$failed" -ne 0 ]; then
    echo AXVISOR_X86_VPCI_ENUMERATION_FAILED
  else
    echo "$success_marker"
  fi
}

run_x86_pci_block_check() {
  mode=$1
  success_marker=$2
  failed=0
  dev=
  count=0
  for candidate in /sys/bus/pci/devices/*; do
    [ -d "$candidate" ] || continue
    vendor=$(read_config_le16 "$candidate/config" 0 2>/dev/null) || continue
    device=$(read_config_le16 "$candidate/config" 2 2>/dev/null) || continue
    if [ "$vendor" = "0x1af4" ] && [ "$device" = "0x1042" ]; then
      dev=$candidate
      count=$((count + 1))
    fi
  done
  if [ "$count" -ne 1 ]; then
    echo "expected exactly one VirtIO Block PCI function, found $count"
    echo AXVISOR_X86_PCI_BLOCK_FAILED
    return
  fi

  config_path=$dev/config
  vendor=$(read_config_le16 "$config_path" 0) || failed=1
  device=$(read_config_le16 "$config_path" 2) || failed=1
  revision=$(read_config_byte "$config_path" 8) || failed=1
  subsystem_vendor=$(read_config_le16 "$config_path" 44) || failed=1
  subsystem_device=$(read_config_le16 "$config_path" 46) || failed=1
  class=$(read_config_le24 "$config_path" 9) || failed=1
  command=$(read_config_le16 "$config_path" 4) || failed=1
  interrupt_line=$(read_config_byte "$config_path" 60) || failed=1
  interrupt_pin=$(read_config_byte "$config_path" 61) || failed=1
  revision_value=0
  case "$revision" in
    ''|*[!0-9a-fA-F]*)
      echo "VirtIO Block revision is unreadable: $revision"
      failed=1
      ;;
    *) revision_value=$((0x$revision)) ;;
  esac
  command_value=0
  if ! command_value=$(parse_config_hex "$command"); then
    echo "PCI command register is unreadable: $command"
    failed=1
  fi
  if [ "$vendor" != "0x1af4" ] || [ "$device" != "0x1042" ] || [ "$revision_value" -lt 1 ]; then
    echo "unexpected VirtIO Block identity or revision"
    failed=1
  fi
  if [ "$subsystem_vendor" != "0x1af4" ] || [ "$subsystem_device" != "0x1042" ]; then
    echo "unexpected VirtIO Block subsystem identity"
    failed=1
  fi
  if [ "$class" != "0x018000" ]; then
    echo "unexpected PCI class: $class"
    failed=1
  fi
  if [ $((command_value & 0x6)) -ne 6 ]; then
    echo "PCI memory and bus-master command bits are not enabled"
    failed=1
  fi
  if [ "$interrupt_pin" != "01" ]; then
    echo "VirtIO Block function does not advertise INTA"
    failed=1
  fi

  resource_line=$(/bin/busybox sed -n '1p' "$dev/resource" 2>/dev/null)
  if ! validate_pci_bar_resource "$resource_line" __AXVISOR_PCI_MEMORY_APERTURE_START__ __AXVISOR_PCI_MEMORY_APERTURE_END__ 4096; then
    failed=1
    echo "BAR0 is not a valid 4 KiB memory resource"
  fi
  if ! validate_pci_capabilities "$config_path"; then
    failed=1
  fi
  if ! validate_virtio_capabilities "$config_path"; then
    failed=1
  fi

  irq=$(/bin/busybox cat "$dev/irq" 2>/dev/null)
  case "$irq" in
    16|17|18|19) ;;
    *)
      echo "VirtIO Block endpoint has an unexpected IRQ: $irq"
      failed=1
      ;;
  esac
  if [ -n "$interrupt_line" ] && [ "$interrupt_line" != "$(printf '%02x' "$irq" 2>/dev/null)" ]; then
    echo "PCI Interrupt Line does not match resolved IRQ ($interrupt_line vs $irq)"
    failed=1
  fi
  if [ ! -L "$dev/driver" ]; then
    echo "virtio_pci/virtio_blk driver is not bound"
    failed=1
  fi

  block_path=
  for candidate in "$dev"/virtio*/block/*; do
    [ -d "$candidate" ] || continue
    block_path=$candidate
    break
  done
  if [ -z "$block_path" ]; then
    echo "PCI endpoint has no Linux block-device child"
    failed=1
  fi
  block_name=${block_path##*/}
  block_device=/dev/$block_name
  if [ ! -b "$block_device" ]; then
    echo "Linux block device is missing: $block_device"
    failed=1
  fi

  before_irq=
  after_irq=
  if [ -n "$block_path" ] && [ -b "$block_device" ] && [ -r "$block_device" ]; then
    before_irq=$(/bin/busybox awk -v n="$irq" '$1 + 0 == n { print $2 + 0; exit }' /proc/interrupts 2>/dev/null)
    pattern=/tmp/axvisor-virtio-pattern
    before=/tmp/axvisor-virtio-before
    after=/tmp/axvisor-virtio-after
    readback=/tmp/axvisor-virtio-readback
    : > "$pattern"
    repeat=0
    while [ "$repeat" -lt 32 ]; do
      printf '\245\136\001\177\022\244\070\311\000\255\125\252\063\147\201\376' >> "$pattern"
      repeat=$((repeat + 1))
    done
    if [ "$mode" = "rw" ]; then
      if ! /bin/busybox dd if="$pattern" of="$block_device" bs=512 seek=8 count=1 conv=fsync 2>/dev/null; then
        echo "writable VirtIO Block write/flush failed"
        failed=1
      elif ! /bin/busybox dd if="$block_device" of="$readback" bs=512 skip=8 count=1 2>/dev/null; then
        echo "writable VirtIO Block read failed"
        failed=1
      elif ! /bin/busybox cmp -s "$pattern" "$readback"; then
        echo "VirtIO Block read-after-write pattern mismatch"
        failed=1
      fi
      if [ "$(/bin/busybox cat "/sys/class/block/$block_name/ro" 2>/dev/null)" != "0" ]; then
        echo "writable VirtIO Block device is marked read-only"
        failed=1
      fi
    else
      if ! /bin/busybox dd if="$block_device" of="$before" bs=512 skip=8 count=1 2>/dev/null; then
        echo "read-only VirtIO Block initial read failed"
        failed=1
      elif /bin/busybox dd if="$pattern" of="$block_device" bs=512 seek=8 count=1 conv=fsync 2>/dev/null; then
        echo "read-only VirtIO Block write unexpectedly succeeded"
        failed=1
      elif ! /bin/busybox dd if="$block_device" of="$after" bs=512 skip=8 count=1 2>/dev/null; then
        echo "read-only VirtIO Block verification read failed"
        failed=1
      elif ! /bin/busybox cmp -s "$before" "$after"; then
        echo "read-only VirtIO Block sector changed after rejected write"
        failed=1
      fi
      if [ "$(/bin/busybox cat "/sys/class/block/$block_name/ro" 2>/dev/null)" != "1" ]; then
        echo "read-only VirtIO Block device is not marked read-only"
        failed=1
      fi
    fi
    after_irq=$(/bin/busybox awk -v n="$irq" '$1 + 0 == n { print $2 + 0; exit }' /proc/interrupts 2>/dev/null)
  else
    echo "Linux block device is not readable: $block_device"
    failed=1
  fi
  if [ -z "$after_irq" ] || [ -z "$before_irq" ] || [ "$after_irq" -le "$before_irq" ]; then
    echo "INTx completion count did not increase ($before_irq -> $after_irq)"
    failed=1
  fi
  if [ "$failed" -ne 0 ]; then
    echo AXVISOR_X86_PCI_BLOCK_FAILED
  else
    echo "$success_marker"
    echo "pci_block=$block_device mode=$mode irq=$irq"
  fi
}

cmdline=$(/bin/busybox cat /proc/cmdline)
case "$cmdline" in
  *axvisor.acpi_case=direct*) run_x86_acpi_check AXVISOR_X86_DIRECT_ACPI_PASSED; exec /bin/busybox sh -i ;;
  *axvisor.acpi_case=ovmf*) run_x86_acpi_check AXVISOR_X86_OVMF_ACPI_PASSED; exec /bin/busybox sh -i ;;
  *axvisor.pci_case=enumeration*) run_pci_enumeration_check AXVISOR_X86_VPCI_ENUMERATION_PASSED; exec /bin/busybox sh -i ;;
  *axvisor.pci_block_case=rw*) run_x86_pci_block_check rw AXVISOR_X86_PCI_BLOCK_RW_PASSED; exec /bin/busybox sh -i ;;
  *axvisor.pci_block_case=ro*) run_x86_pci_block_check ro AXVISOR_X86_PCI_BLOCK_RO_PASSED; exec /bin/busybox sh -i ;;
  *axvisor.acpi_case=off*)
    if [ -d /sys/firmware/acpi/tables ]; then
      echo AXVISOR_X86_ACPI_FAILED
    else
      echo AXVISOR_X86_MP_FALLBACK_PASSED
    fi
    exec /bin/busybox sh -i
    ;;
  *axvisor.timer_case=gicv3-its*) success_marker=AXVISOR_GICV3_ITS_TIMER_STRESS_PASSED; require_its=1 ;;
  *axvisor.timer_case=gicv2*) success_marker=AXVISOR_GICV2_TIMER_STRESS_PASSED; require_its=0 ;;
  *axvisor.timer_case=gicv3*) success_marker=AXVISOR_GICV3_TIMER_STRESS_PASSED; require_its=0 ;;
  *) echo AXVISOR_GUEST_ASSERTION_CASE_UNKNOWN; exec /bin/busybox sh -i ;;
esac


start=$(/bin/busybox date +%s)
last=$start
round=0
failed=0
while [ "$round" -lt 8 ]; do
  /bin/busybox sleep 1 & p1=$!
  /bin/busybox sleep 2 & p2=$!
  /bin/busybox sleep 3 & p3=$!
  wait "$p1"
  wait "$p2"
  wait "$p3"
  now=$(/bin/busybox date +%s)
  if [ "$now" -lt "$last" ]; then
    failed=1
  fi
  last=$now
  round=$((round + 1))
done
end=$(/bin/busybox date +%s)
elapsed=$((end - start))
if [ "$require_its" -eq 1 ] && ! /bin/busybox dmesg | /bin/busybox grep -q 'ITS'; then
  failed=1
fi
if [ "$failed" -ne 0 ] || [ "$elapsed" -lt 20 ] || [ "$elapsed" -gt 120 ]; then
  echo TIMER_STRESS_FAILED
else
  echo "$success_marker"
fi

exec /bin/busybox sh -i
"#;

const PCI_BAR_VALIDATOR: &str = r#"validate_pci_bar_resource() {
  resource_line=$1
  aperture_start=$2
  aperture_end=$3
  expected_size=$4
  aperture_start=${aperture_start#0x}
  aperture_end=${aperture_end#0x}
  set -- $resource_line
  if [ $# -lt 3 ]; then
    echo "PCI BAR resource entry is missing or unreadable"
    return 1
  fi
  v1=${1#0x}
  v2=${2#0x}
  v3=${3#0x}
  case "$v1$v2$v3$aperture_start$aperture_end" in
    *[!0-9a-fA-F]*)
      echo "PCI BAR resource entry is malformed: $resource_line"
      return 1
      ;;
  esac
  bar_start=$((0x$v1))
  bar_end=$((0x$v2))
  bar_flags=$((0x$v3))
  echo "vPCI endpoint BAR2 [$bar_start-$bar_end] flags $bar_flags"
  if [ "$bar_end" -lt "$bar_start" ]; then
    echo "PCI BAR resource range is inverted"
    return 1
  fi
  if [ "$bar_start" -eq 0 ]; then
    echo "PCI BAR has an unassigned base address"
    return 1
  fi
  aperture_start=$((0x$aperture_start))
  aperture_end=$((0x$aperture_end))
  if [ "$bar_start" -lt "$aperture_start" ] || [ "$bar_end" -ge "$aperture_end" ]; then
    echo "BAR2 is outside the PCI memory aperture"
    return 1
  fi
  bar_size=$((bar_end - bar_start + 1))
  if [ "$bar_size" -ne "$expected_size" ]; then
    echo "PCI BAR has unexpected size: $bar_size (expected $expected_size)"
    return 1
  fi
  if [ $((bar_flags & 0x200)) -eq 0 ]; then
    echo "PCI BAR is not a memory resource"
    return 1
  fi
  if [ $((bar_flags & 0x2000)) -ne 0 ]; then
    echo "PCI BAR is unexpectedly prefetchable"
    return 1
  fi
  if [ $((bar_flags & 0x100000)) -ne 0 ]; then
    echo "PCI BAR unexpectedly uses a 64-bit memory resource"
    return 1
  fi
  if [ $((bar_flags & 0x20000000)) -ne 0 ]; then
    echo "PCI BAR has an unassigned resource flag"
    return 1
  fi
  return 0
}
"#;

const PCI_CAPABILITY_VALIDATOR: &str = r#"validate_pci_capabilities() {
  config_path=$1
  if [ ! -r "$config_path" ]; then
    echo "PCI configuration space is missing or unreadable"
    return 1
  fi

  status_low=$(read_config_byte "$config_path" 6) || {
    echo "PCI status register is unreadable"
    return 1
  }
  if [ $((0x$status_low & 0x10)) -eq 0 ]; then
    return 0
  fi

  pointer_hex=$(read_config_byte "$config_path" 52) || {
    echo "PCI capability pointer is unreadable"
    return 1
  }
  pointer=$((0x$pointer_hex))
  if [ "$pointer" -eq 0 ]; then
    echo "PCI capability list is missing its first entry"
    return 1
  fi
  visited=:
  iteration=0
  while [ "$pointer" -ne 0 ] && [ "$iteration" -lt 48 ]; do
    case ":$visited:" in
      *":$pointer:"*)
        echo "PCI capability list contains a cycle"
        return 1
        ;;
    esac
    visited="$visited:$pointer"
    if [ "$pointer" -lt 64 ] || [ "$pointer" -gt 252 ] || [ $((pointer % 4)) -ne 0 ]; then
      echo "PCI capability pointer is invalid: $pointer"
      return 1
    fi
    capability_hex=$(read_config_byte "$config_path" "$pointer") || {
      echo "PCI capability ID is unreadable"
      return 1
    }
    capability=$((0x$capability_hex))
    if [ "$capability" -eq 5 ] || [ "$capability" -eq 17 ]; then
      echo "vPCI endpoint unexpectedly advertises MSI/MSI-X"
      return 1
    fi
    if [ "$capability" -eq 0 ]; then
      echo "PCI capability ID is invalid"
      return 1
    fi
    next_offset=$((pointer + 1))
    next_hex=$(read_config_byte "$config_path" "$next_offset") || {
      echo "PCI capability next pointer is unreadable"
      return 1
    }
    pointer=$((0x$next_hex))
    if [ "$pointer" -ne 0 ] &&
       { [ "$pointer" -lt 64 ] || [ "$pointer" -gt 252 ] || [ $((pointer % 4)) -ne 0 ]; }; then
      echo "PCI capability next pointer is invalid: $pointer"
      return 1
    fi
    iteration=$((iteration + 1))
  done
  if [ "$pointer" -ne 0 ]; then
    echo "PCI capability list is too long"
    return 1
  fi
  return 0
}
"#;

const PCI_CONFIG_READERS: &str = r#"read_config_byte() {
  bytes=$(/bin/od -An -tx1 -j "$2" -N 1 "$1" 2>/dev/null) || return 1
  set -- $bytes
  if [ $# -ne 1 ]; then
    return 1
  fi
  value=${1#0x}
  case "$value" in
    ''|*[!0-9a-fA-F]*) return 1 ;;
  esac
  printf '%s\n' "$value"
}

read_config_le16() {
  lo=$(read_config_byte "$1" "$2") || return 1
  hi=$(read_config_byte "$1" "$(( $2 + 1 ))") || return 1
  printf '0x%04x\n' "$((0x$lo | (0x$hi << 8)))"
}

parse_config_hex() {
  value=${1#0x}
  case "$value" in
    ''|*[!0-9a-fA-F]*) return 1 ;;
  esac
  printf '%u\n' "$((0x$value))"
}

read_config_le24() {
  b0=$(read_config_byte "$1" "$2") || return 1
  b1=$(read_config_byte "$1" "$(( $2 + 1 ))") || return 1
  b2=$(read_config_byte "$1" "$(( $2 + 2 ))") || return 1
  printf '0x%06x\n' "$((0x$b0 | (0x$b1 << 8) | (0x$b2 << 16)))"
}

read_config_le32() {
  b0=$(read_config_byte "$1" "$2") || return 1
  b1=$(read_config_byte "$1" "$(( $2 + 1 ))") || return 1
  b2=$(read_config_byte "$1" "$(( $2 + 2 ))") || return 1
  b3=$(read_config_byte "$1" "$(( $2 + 3 ))") || return 1
  printf '0x%08x\n' "$((0x$b0 | (0x$b1 << 8) | (0x$b2 << 16) | (0x$b3 << 24)))"
}

validate_virtio_capabilities() {
  config_path=$1
  status_low=$(read_config_byte "$config_path" 6) || return 1
  if [ $((0x$status_low & 0x10)) -eq 0 ]; then
    echo "VirtIO PCI capability list is not advertised"
    return 1
  fi
  pointer_hex=$(read_config_byte "$config_path" 52) || return 1
  pointer=$((0x$pointer_hex))
  count=0
  seen=:
  while [ "$pointer" -ne 0 ] && [ "$count" -lt 8 ]; do
    case ":$seen:" in
      *":$pointer:"*)
        echo "VirtIO PCI capability list contains a cycle"
        return 1
        ;;
    esac
    seen="$seen:$pointer"
    if [ "$pointer" -lt 64 ] || [ "$pointer" -gt 252 ] || [ $((pointer % 4)) -ne 0 ]; then
      echo "VirtIO PCI capability pointer is invalid: $pointer"
      return 1
    fi
    id=$(read_config_byte "$config_path" "$pointer") || return 1
    if [ "$((0x$id))" -ne 9 ]; then
      echo "VirtIO PCI capability is not vendor-specific"
      return 1
    fi
    cap_len=$(read_config_byte "$config_path" "$((pointer + 2))") || return 1
    cfg_type=$(read_config_byte "$config_path" "$((pointer + 3))") || return 1
    bar=$(read_config_byte "$config_path" "$((pointer + 4))") || return 1
    offset=$(read_config_le32 "$config_path" "$((pointer + 8))") || return 1
    length=$(read_config_le32 "$config_path" "$((pointer + 12))") || return 1
    multiplier=0x00000000
    if [ "$((0x$cfg_type))" -eq 2 ]; then
      multiplier=$(read_config_le32 "$config_path" "$((pointer + 16))") || return 1
    fi
    case "$((0x$cfg_type))" in
      1) expected_len=16; expected_offset=0x00000000; expected_length=0x00000038; expected_multiplier=0x00000000 ;;
      2) expected_len=20; expected_offset=0x00000100; expected_length=0x00000004; expected_multiplier=0x00000004 ;;
      3) expected_len=16; expected_offset=0x00000200; expected_length=0x00000001; expected_multiplier=0x00000000 ;;
      4) expected_len=16; expected_offset=0x00000300; expected_length=0x00000010; expected_multiplier=0x00000000 ;;
      5) expected_len=20; expected_offset=0x00000000; expected_length=0x00000000; expected_multiplier=0x00000000 ;;
      *) echo "unexpected VirtIO capability type: $cfg_type"; return 1 ;;
    esac
    if [ "$((0x$cap_len))" -ne "$expected_len" ] || [ "$bar" != "00" ] ||
       [ "$offset" != "$expected_offset" ] || [ "$length" != "$expected_length" ] ||
       [ "$multiplier" != "$expected_multiplier" ]; then
      echo "unexpected VirtIO capability fields at $pointer"
      return 1
    fi
    count=$((count + 1))
    next_hex=$(read_config_byte "$config_path" "$((pointer + 1))") || return 1
    pointer=$((0x$next_hex))
  done
  if [ "$pointer" -ne 0 ] || [ "$count" -ne 5 ]; then
    echo "expected five VirtIO PCI capabilities, found $count"
    return 1
  fi
  return 0
}
"#;

fn init_script() -> Vec<u8> {
    INIT_SCRIPT_TEMPLATE
        .replace("__AXVISOR_PCI_CONFIG_READERS__", PCI_CONFIG_READERS)
        .replace("__AXVISOR_PCI_BAR_VALIDATOR__", PCI_BAR_VALIDATOR)
        .replace(
            "__AXVISOR_PCI_CAPABILITY_VALIDATOR__",
            PCI_CAPABILITY_VALIDATOR,
        )
        .replace(
            "__AXVISOR_PCI_MEMORY_APERTURE_START__",
            X86_PCI_MEMORY_APERTURE_START,
        )
        .replace(
            "__AXVISOR_PCI_MEMORY_APERTURE_END__",
            X86_PCI_MEMORY_APERTURE_END,
        )
        .into_bytes()
}

pub(super) async fn prepare_configured_busybox_initramfs(
    request: &ResolvedAxvisorRequest,
    cargo: &Cargo,
    workspace_root: &Path,
) -> anyhow::Result<()> {
    if let Some(configured_output) = cargo.env.get(OUTPUT_ENV) {
        let output_path = resolve_output_path(workspace_root, configured_output, OUTPUT_ENV)?;
        let rootfs_path = rootfs::qemu_rootfs_path(request, workspace_root, None)?;
        prepare_busybox_initramfs(&rootfs_path, &output_path, &request.arch)?;
        println!(
            "prepared Axvisor QEMU test initramfs: {}",
            output_path.display()
        );
    }
    if let Some(configured_output) = cargo.env.get(OVMF_OUTPUT_ENV) {
        ensure!(
            request.arch == "x86_64",
            "{OVMF_OUTPUT_ENV} is only valid for x86_64 Axvisor tests"
        );
        let output_path = resolve_output_path(workspace_root, configured_output, OVMF_OUTPUT_ENV)?;
        let evidence = super::ovmf::prepare_x86_ovmf(&output_path).await?;
        println!("{evidence}");
    }
    Ok(())
}

fn resolve_output_path(
    workspace_root: &Path,
    configured_output: &str,
    variable: &str,
) -> anyhow::Result<PathBuf> {
    let configured_output = Path::new(configured_output);
    ensure!(
        !configured_output.is_absolute()
            && configured_output
                .components()
                .all(|component| matches!(component, Component::CurDir | Component::Normal(_))),
        "{variable} must be a workspace-relative path without parent traversal"
    );
    Ok(workspace_root.join(configured_output))
}

fn prepare_busybox_initramfs(
    rootfs_path: &Path,
    output_path: &Path,
    arch: &str,
) -> anyhow::Result<()> {
    let busybox = required_rootfs_file(rootfs_path, BUSYBOX_PATH)?;
    let loader_path = musl_loader_path(arch)?;
    let loader = required_rootfs_file(rootfs_path, loader_path)?;
    let archive = build_busybox_initramfs(&busybox, loader_path, &loader)?;

    let output_parent = output_path.parent().with_context(|| {
        format!(
            "initramfs output path has no parent: {}",
            output_path.display()
        )
    })?;
    fs::create_dir_all(output_parent).with_context(|| {
        format!(
            "failed to create initramfs output directory {}",
            output_parent.display()
        )
    })?;
    let mut temporary = NamedTempFile::new_in(output_parent).with_context(|| {
        format!(
            "failed to create temporary initramfs in {}",
            output_parent.display()
        )
    })?;
    temporary
        .write_all(&archive)
        .with_context(|| format!("failed to write {}", output_path.display()))?;
    temporary
        .persist(output_path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to install {}", output_path.display()))?;
    Ok(())
}

fn required_rootfs_file(rootfs_path: &Path, guest_path: &str) -> anyhow::Result<Vec<u8>> {
    read_binary_file(rootfs_path, guest_path)?.with_context(|| {
        format!(
            "managed rootfs {} does not contain required file {guest_path}",
            rootfs_path.display()
        )
    })
}

fn musl_loader_path(arch: &str) -> anyhow::Result<&'static str> {
    match arch {
        "aarch64" => Ok("/lib/ld-musl-aarch64.so.1"),
        "loongarch64" => Ok("/lib/ld-musl-loongarch64.so.1"),
        "riscv64" => Ok("/lib/ld-musl-riscv64.so.1"),
        "x86_64" => Ok("/lib/ld-musl-x86_64.so.1"),
        unsupported => {
            bail!("BusyBox test initramfs does not support architecture `{unsupported}`")
        }
    }
}

fn build_busybox_initramfs(
    busybox: &[u8],
    loader_path: &str,
    loader: &[u8],
) -> anyhow::Result<Vec<u8>> {
    let init_script = init_script();
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    {
        let mut archive = NewcArchive::new(&mut encoder);
        let mut directories = BTreeSet::from([
            "bin".to_string(),
            "dev".to_string(),
            "proc".to_string(),
            "root".to_string(),
            "sys".to_string(),
            "tmp".to_string(),
        ]);
        let loader_archive_path = archive_path(loader_path)?;
        add_parent_directories(loader_archive_path, &mut directories);
        for directory in directories {
            archive.append_directory(&directory)?;
        }

        archive.append_regular("bin/busybox", busybox)?;
        archive.append_regular(loader_archive_path, loader)?;
        archive.append_regular("init", &init_script)?;
        for applet in [
            "awk", "cat", "cmp", "date", "dd", "dmesg", "grep", "mount", "od", "sed", "sh", "sleep",
        ] {
            archive.append_symlink(&format!("bin/{applet}"), "busybox")?;
        }
        archive.finish()?;
    }
    encoder
        .finish()
        .context("failed to finish initramfs gzip stream")
}

fn archive_path(guest_path: &str) -> anyhow::Result<&str> {
    let archive_path = guest_path
        .strip_prefix('/')
        .context("initramfs guest path must be absolute")?;
    ensure!(
        !archive_path.is_empty()
            && Path::new(archive_path)
                .components()
                .all(|component| matches!(component, Component::Normal(_))),
        "invalid initramfs guest path `{guest_path}`"
    );
    Ok(archive_path)
}

fn add_parent_directories(path: &str, directories: &mut BTreeSet<String>) {
    let mut parent = Path::new(path).parent();
    while let Some(path) = parent {
        if path.as_os_str().is_empty() {
            break;
        }
        directories.insert(path.to_string_lossy().into_owned());
        parent = path.parent();
    }
}

struct NewcArchive<W> {
    writer: W,
    inode: u32,
}

impl<W: Write> NewcArchive<W> {
    fn new(writer: W) -> Self {
        Self { writer, inode: 1 }
    }

    fn append_directory(&mut self, path: &str) -> anyhow::Result<()> {
        self.append(path, 0o040755, 2, &[])
    }

    fn append_regular(&mut self, path: &str, contents: &[u8]) -> anyhow::Result<()> {
        self.append(path, 0o100755, 1, contents)
    }

    fn append_symlink(&mut self, path: &str, target: &str) -> anyhow::Result<()> {
        self.append(path, 0o120777, 1, target.as_bytes())
    }

    fn finish(&mut self) -> anyhow::Result<()> {
        self.append("TRAILER!!!", 0, 1, &[])
    }

    fn append(
        &mut self,
        path: &str,
        mode: u32,
        link_count: u32,
        contents: &[u8],
    ) -> anyhow::Result<()> {
        ensure!(!path.as_bytes().contains(&0), "cpio path contains NUL");
        let file_size = u32::try_from(contents.len()).context("cpio entry is larger than 4 GiB")?;
        let name_size = u32::try_from(path.len() + 1).context("cpio path is too long")?;
        write!(
            self.writer,
            "070701{:08x}{mode:08x}{:08x}{:08x}{link_count:08x}{:08x}{file_size:08x}{:08x}{:08x}{:\
             08x}{:08x}{name_size:08x}{:08x}",
            self.inode, 0, 0, 0, 0, 0, 0, 0, 0,
        )
        .context("failed to write cpio header")?;
        self.writer
            .write_all(path.as_bytes())
            .context("failed to write cpio path")?;
        self.writer
            .write_all(&[0])
            .context("failed to terminate cpio path")?;
        write_padding(&mut self.writer, 110 + path.len() + 1)?;
        self.writer
            .write_all(contents)
            .context("failed to write cpio contents")?;
        write_padding(&mut self.writer, contents.len())?;
        self.inode = self.inode.checked_add(1).context("cpio inode overflow")?;
        Ok(())
    }
}

fn write_padding(writer: &mut impl Write, written: usize) -> anyhow::Result<()> {
    const ZEROES: [u8; 3] = [0; 3];
    let padding = (4 - written % 4) % 4;
    writer
        .write_all(&ZEROES[..padding])
        .context("failed to write cpio alignment")
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::fs;
    use std::io::Read;
    #[cfg(unix)]
    use std::process::Command;

    use flate2::read::GzDecoder;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn configured_output_must_stay_inside_workspace() {
        let root = tempdir().unwrap();

        assert_eq!(
            resolve_output_path(root.path(), "tmp/initramfs.cpio.gz", OUTPUT_ENV).unwrap(),
            root.path().join("tmp/initramfs.cpio.gz")
        );
        assert!(resolve_output_path(root.path(), "../outside", OUTPUT_ENV).is_err());
        assert!(resolve_output_path(root.path(), "/tmp/outside", OUTPUT_ENV).is_err());
    }

    #[test]
    fn generated_archive_contains_busybox_loader_and_shell_applets() {
        let compressed =
            build_busybox_initramfs(b"busybox", "/lib/ld-musl-test.so.1", b"loader").unwrap();
        let mut archive = Vec::new();
        GzDecoder::new(compressed.as_slice())
            .read_to_end(&mut archive)
            .unwrap();
        let entries = parse_newc_entries(&archive);

        assert_eq!(entries.get("bin/busybox").unwrap(), b"busybox");
        assert_eq!(entries.get("lib/ld-musl-test.so.1").unwrap(), b"loader");
        let init = entries.get("init").unwrap();
        assert!(init.starts_with(b"#!/bin/busybox sh"));
        assert!(
            init.windows(b"validate_pci_bar_resource".len())
                .any(|window| window == b"validate_pci_bar_resource")
        );
        assert!(
            init.windows(b"validate_pci_capabilities".len())
                .any(|window| window == b"validate_pci_capabilities")
        );
        assert!(
            !init
                .windows(b"__AXVISOR_PCI_MEMORY_APERTURE_START__".len())
                .any(|window| window == b"__AXVISOR_PCI_MEMORY_APERTURE_START__")
        );
        assert!(
            !init
                .windows(b"__AXVISOR_PCI_CAPABILITY_VALIDATOR__".len())
                .any(|window| window == b"__AXVISOR_PCI_CAPABILITY_VALIDATOR__")
        );
        assert!(
            init.windows(b"AXVISOR_GICV2_TIMER_STRESS_PASSED".len())
                .any(|window| window == b"AXVISOR_GICV2_TIMER_STRESS_PASSED")
        );
        assert!(
            init.windows(b"AXVISOR_GICV3_TIMER_STRESS_PASSED".len())
                .any(|window| window == b"AXVISOR_GICV3_TIMER_STRESS_PASSED")
        );
        assert!(
            init.windows(b"AXVISOR_GICV3_ITS_TIMER_STRESS_PASSED".len())
                .any(|window| window == b"AXVISOR_GICV3_ITS_TIMER_STRESS_PASSED")
        );
        assert!(
            init.windows(b"AXVISOR_X86_DIRECT_ACPI_PASSED".len())
                .any(|window| window == b"AXVISOR_X86_DIRECT_ACPI_PASSED")
        );
        assert!(
            init.windows(b"AXVISOR_X86_OVMF_ACPI_PASSED".len())
                .any(|window| window == b"AXVISOR_X86_OVMF_ACPI_PASSED")
        );
        assert!(
            init.windows(b"AXVISOR_X86_VPCI_ENUMERATION_PASSED".len())
                .any(|window| window == b"AXVISOR_X86_VPCI_ENUMERATION_PASSED")
        );
        assert!(
            init.windows(b"AXVISOR_X86_VPCI_ENUMERATION_FAILED".len())
                .any(|window| window == b"AXVISOR_X86_VPCI_ENUMERATION_FAILED")
        );
        assert!(
            init.windows(b"AXVISOR_X86_PCI_BLOCK_RW_PASSED".len())
                .any(|window| window == b"AXVISOR_X86_PCI_BLOCK_RW_PASSED")
        );
        assert!(
            init.windows(b"AXVISOR_X86_PCI_BLOCK_RO_PASSED".len())
                .any(|window| window == b"AXVISOR_X86_PCI_BLOCK_RO_PASSED")
        );
        for applet in [
            "awk", "cat", "cmp", "date", "dd", "dmesg", "grep", "mount", "od", "sed", "sh", "sleep",
        ] {
            assert_eq!(entries.get(&format!("bin/{applet}")).unwrap(), b"busybox");
        }
    }

    #[cfg(unix)]
    #[test]
    fn generated_init_script_is_valid_shell() {
        let script = String::from_utf8(init_script()).unwrap();
        let output = Command::new("sh")
            .arg("-n")
            .arg("-c")
            .arg(script)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "generated init script is not valid POSIX shell: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(unix)]
    #[test]
    fn pci_bar_validator_rejects_unassigned_zero_based_resource() {
        let output = run_pci_bar_validator(
            "0000000000000000 000000000000ffff 00000200",
            X86_PCI_MEMORY_APERTURE_START,
            X86_PCI_MEMORY_APERTURE_END,
        );
        assert!(!output.status.success());
    }

    #[cfg(unix)]
    #[test]
    fn pci_bar_validator_rejects_resource_outside_aperture() {
        let output = run_pci_bar_validator(
            "00000000b0000000 00000000b000ffff 00000200",
            X86_PCI_MEMORY_APERTURE_START,
            X86_PCI_MEMORY_APERTURE_END,
        );
        assert!(!output.status.success());
    }

    #[cfg(unix)]
    #[test]
    fn pci_bar_validator_rejects_unassigned_resource_flag() {
        let output = run_pci_bar_validator(
            "00000000c0000000 00000000c000ffff 20000200",
            X86_PCI_MEMORY_APERTURE_START,
            X86_PCI_MEMORY_APERTURE_END,
        );
        assert!(!output.status.success());
    }

    #[cfg(unix)]
    #[test]
    fn pci_bar_validator_rejects_64_bit_memory_resource() {
        let output = run_pci_bar_validator(
            "00000000c0000000 00000000c000ffff 00100200",
            X86_PCI_MEMORY_APERTURE_START,
            X86_PCI_MEMORY_APERTURE_END,
        );
        assert!(!output.status.success());
    }

    #[cfg(unix)]
    #[test]
    fn pci_bar_validator_accepts_assigned_resource_inside_aperture() {
        let output = run_pci_bar_validator(
            "00000000c0000000 00000000c000ffff 00000200",
            X86_PCI_MEMORY_APERTURE_START,
            X86_PCI_MEMORY_APERTURE_END,
        );
        assert!(output.status.success());
    }

    #[cfg(unix)]
    #[test]
    fn pci_capability_validator_rejects_msi_capability() {
        let mut config = vec![0; 256];
        config[0x06] = 0x10;
        config[0x34] = 0x40;
        config[0x40] = 0x05;
        let output = run_pci_capability_validator(&config);
        assert!(!output.status.success());
    }

    #[cfg(unix)]
    #[test]
    fn pci_capability_validator_rejects_msix_capability() {
        let mut config = vec![0; 256];
        config[0x06] = 0x10;
        config[0x34] = 0x40;
        config[0x40] = 0x11;
        let output = run_pci_capability_validator(&config);
        assert!(!output.status.success());
    }

    #[cfg(unix)]
    #[test]
    fn pci_capability_validator_rejects_msi_after_another_capability() {
        let mut config = vec![0; 256];
        config[0x06] = 0x10;
        config[0x34] = 0x40;
        config[0x40] = 0x01;
        config[0x41] = 0x44;
        config[0x44] = 0x05;
        let output = run_pci_capability_validator(&config);
        assert!(!output.status.success());
    }

    #[cfg(unix)]
    #[test]
    fn pci_capability_validator_rejects_invalid_capability_pointer() {
        let mut config = vec![0; 256];
        config[0x06] = 0x10;
        config[0x34] = 0x42;
        let output = run_pci_capability_validator(&config);
        assert!(!output.status.success());
    }

    #[cfg(unix)]
    #[test]
    fn pci_capability_validator_rejects_capability_cycle() {
        let mut config = vec![0; 256];
        config[0x06] = 0x10;
        config[0x34] = 0x40;
        config[0x40] = 0x01;
        config[0x41] = 0x40;
        let output = run_pci_capability_validator(&config);
        assert!(!output.status.success());
    }

    #[cfg(unix)]
    #[test]
    fn pci_capability_validator_accepts_config_without_capability_list() {
        let output = run_pci_capability_validator(&[0; 256]);
        assert!(output.status.success());
    }

    #[cfg(unix)]
    #[test]
    fn pci_capability_validator_accepts_a_non_msi_capability_list() {
        let mut config = vec![0; 256];
        config[0x06] = 0x10;
        config[0x34] = 0x40;
        config[0x40] = 0x01;
        let output = run_pci_capability_validator(&config);
        assert!(output.status.success());
    }

    #[cfg(unix)]
    #[test]
    fn pci_capability_validator_rejects_missing_first_capability() {
        let mut config = vec![0; 256];
        config[0x06] = 0x10;
        let output = run_pci_capability_validator(&config);
        assert!(!output.status.success());
    }

    #[cfg(unix)]
    #[test]
    fn virtio_capability_validator_accepts_the_modern_pci_layout() {
        let config = modern_virtio_pci_config();
        let output = run_virtio_capability_validator(&config);
        assert!(
            output.status.success(),
            "modern VirtIO capability layout was rejected: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(unix)]
    #[test]
    fn pci_command_parser_accepts_prefixed_le16_value() {
        let mut config = vec![0; 256];
        config[4] = 0x06;
        let output = run_pci_command_parser(&config);
        assert!(
            output.status.success(),
            "PCI command parser rejected 0x0006: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "6");
    }

    #[cfg(unix)]
    #[test]
    fn virtio_capability_validator_ignores_pci_cfg_effect_window() {
        let mut config = modern_virtio_pci_config();
        config[0x84 + 16..0x84 + 20].copy_from_slice(&0xa5_a5_5a_5a_u32.to_le_bytes());
        let output = run_virtio_capability_validator(&config);
        assert!(
            output.status.success(),
            "PCI_CFG effect bytes must not be validated as a notify multiplier: stdout={} \
             stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(unix)]
    #[test]
    fn virtio_capability_validator_rejects_wrong_notify_length() {
        let mut config = modern_virtio_pci_config();
        config[0x50 + 2] = 16;
        let output = run_virtio_capability_validator(&config);
        assert!(!output.status.success());
    }

    #[cfg(unix)]
    #[test]
    fn virtio_capability_validator_rejects_non_vendor_capability() {
        let mut config = modern_virtio_pci_config();
        config[0x64] = 1;
        let output = run_virtio_capability_validator(&config);
        assert!(!output.status.success());
    }

    #[cfg(unix)]
    fn run_pci_bar_validator(
        resource_line: &str,
        aperture_start: &str,
        aperture_end: &str,
    ) -> std::process::Output {
        Command::new("sh")
            .arg("-c")
            .arg(format!(
                "{PCI_BAR_VALIDATOR}\nvalidate_pci_bar_resource \"$1\" \"$2\" \"$3\" 65536"
            ))
            .arg("pci-bar-test")
            .arg(resource_line)
            .arg(aperture_start)
            .arg(aperture_end)
            .output()
            .unwrap()
    }

    #[cfg(unix)]
    fn run_pci_capability_validator(config: &[u8]) -> std::process::Output {
        let directory = tempdir().unwrap();
        let config_path = directory.path().join("config");
        fs::write(&config_path, config).unwrap();
        Command::new("sh")
            .arg("-c")
            .arg(format!(
                "{PCI_CONFIG_READERS}\n{PCI_CAPABILITY_VALIDATOR}\nvalidate_pci_capabilities \
                 \"$1\""
            ))
            .arg("pci-capability-test")
            .arg(config_path)
            .output()
            .unwrap()
    }

    #[cfg(unix)]
    fn run_virtio_capability_validator(config: &[u8]) -> std::process::Output {
        let directory = tempdir().unwrap();
        let config_path = directory.path().join("config");
        fs::write(&config_path, config).unwrap();
        Command::new("sh")
            .arg("-c")
            .arg(format!(
                "{PCI_CONFIG_READERS}\nvalidate_virtio_capabilities \"$1\""
            ))
            .arg("virtio-capability-test")
            .arg(config_path)
            .output()
            .unwrap()
    }

    #[cfg(unix)]
    fn run_pci_command_parser(config: &[u8]) -> std::process::Output {
        let directory = tempdir().unwrap();
        let config_path = directory.path().join("config");
        fs::write(&config_path, config).unwrap();
        Command::new("sh")
            .arg("-c")
            .arg(format!(
                "{PCI_CONFIG_READERS}\ncommand=$(read_config_le16 \"$1\" 4) || exit \
                 1\ncommand_value=$(parse_config_hex \"$command\") || exit 1\nprintf '%s\\n' \
                 \"$command_value\""
            ))
            .arg("pci-command-test")
            .arg(config_path)
            .output()
            .unwrap()
    }

    fn modern_virtio_pci_config() -> Vec<u8> {
        let mut config = vec![0; 256];
        config[0x06] = 0x10;
        config[0x34] = 0x40;
        let capabilities: [(usize, u8, u8, u8, u32, u32, u32); 5] = [
            (0x40, 0x50, 16, 1, 0, 0x38, 0),
            (0x50, 0x64, 20, 2, 0x100, 4, 4),
            (0x64, 0x74, 16, 3, 0x200, 1, 0),
            (0x74, 0x84, 16, 4, 0x300, 0x10, 0),
            (0x84, 0, 20, 5, 0, 0, 0),
        ];
        for (offset, next, length, cfg_type, bar_offset, region_length, multiplier) in capabilities
        {
            config[offset] = 9;
            config[offset + 1] = next;
            config[offset + 2] = length;
            config[offset + 3] = cfg_type;
            config[offset + 4] = 0;
            config[offset + 8..offset + 12].copy_from_slice(&bar_offset.to_le_bytes());
            config[offset + 12..offset + 16].copy_from_slice(&region_length.to_le_bytes());
            if length == 20 {
                config[offset + 16..offset + 20].copy_from_slice(&multiplier.to_le_bytes());
            }
        }
        config
    }

    fn parse_newc_entries(archive: &[u8]) -> std::collections::BTreeMap<String, Vec<u8>> {
        let mut entries = std::collections::BTreeMap::new();
        let mut offset = 0;
        loop {
            assert_eq!(&archive[offset..offset + 6], b"070701");
            let file_size = parse_hex(&archive[offset + 54..offset + 62]);
            let name_size = parse_hex(&archive[offset + 94..offset + 102]);
            offset += 110;
            let name = std::str::from_utf8(&archive[offset..offset + name_size - 1]).unwrap();
            offset = align4(offset + name_size);
            if name == "TRAILER!!!" {
                break;
            }
            entries.insert(
                name.to_string(),
                archive[offset..offset + file_size].to_vec(),
            );
            offset = align4(offset + file_size);
        }
        entries
    }

    fn parse_hex(bytes: &[u8]) -> usize {
        usize::from_str_radix(std::str::from_utf8(bytes).unwrap(), 16).unwrap()
    }

    fn align4(value: usize) -> usize {
        (value + 3) & !3
    }
}
