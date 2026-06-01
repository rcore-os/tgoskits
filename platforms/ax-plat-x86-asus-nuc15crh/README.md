# axplat-x86-asus-nuc15crh

Hardware platform implementation for ASUS NUC15CRH x86_64 machines used by
AxVisor HTTP Boot.

## Overview

This crate keeps real-machine HTTP Boot behavior separate from the QEMU Q35
platform. It supports the normal x86_64 multiboot path and adds an `httpboot`
entry path used by the ostool UEFI loader.

The HTTP Boot path expects the loader to:

- download the AxVisor image from `ostool-server`;
- pass an ostool boot-info pointer in `rdi`;
- jump to the exported `httpboot_entry` symbol;
- leave the kernel image loaded at the configured physical address.

## Platform Notes

- Serial console: COM1, 115200 baud.
- Boot protocol: multiboot for the legacy path, ostool boot-info for HTTP Boot.
- Memory discovery: uses ostool boot-info when present, otherwise falls back to
  multiboot memory information.
- Shutdown behavior: `system_off` reboots the machine when
  `reboot-on-system-off` is enabled.

## Feature Flags

- `default`: enables `irq`, `smp`, and `reboot-on-system-off`.
- `irq`: interrupt request support.
- `smp`: symmetric multiprocessing support.
- `rtc`: real-time clock support.
- `fp-simd`: floating-point and SIMD support.
- `reboot-on-system-off`: reboot instead of attempting ACPI-less poweroff.

## Building

Use the dedicated AxVisor board config:

```bash
cargo axvisor httpboot \
  --config os/axvisor/configs/board/asus-nuc15crh-x86_64.toml \
  --vmconfigs os/axvisor/configs/vms/linux-x86_64-qemu-smp1.toml
```
