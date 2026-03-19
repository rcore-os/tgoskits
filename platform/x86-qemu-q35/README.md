# axplat-x86-qemu-q35

Hardware platform implementation for x86_64 QEMU Q35 chipset, designed for the Axvisor hypervisor framework.

## Overview

This crate provides a hardware abstraction layer for the x86_64 architecture targeting QEMU's Q35 chipset. It implements the `axplat` interface to support multiboot boot protocol and provides essential platform services for running hypervisors and operating systems.

## Features

- **Multiboot Support**: Boot protocol compatible with GRUB and other multiboot-compliant bootloaders
- **SMP (Symmetric Multiprocessing)**: Multi-core CPU support with AP (Application Processor) startup
- **Interrupt Handling**: Full interrupt request (IRQ) support via APIC
- **Serial Console**: UART 16550 serial port for console I/O
- **Memory Management**: Physical-virtual memory mapping support
- **Timer**: High-resolution timer support
- **Power Management**: System power-off and reboot capabilities
- **RTC Support** (optional): Real-time clock via the `rtc` feature
- **FP/SIMD** (optional): Floating-point and SIMD support via the `fp-simd` feature

## Usage

Add this to your `Cargo.toml`:

```toml
[dependencies]
axplat-x86-qemu-q35 = "0.1"
```

### Feature Flags

- `default`: Enables `irq`, `smp`, and `reboot-on-system-off`
- `irq`: Interrupt request support
- `smp`: Symmetric multiprocessing support
- `rtc`: Real-time clock support
- `fp-simd`: Floating-point and SIMD support
- `reboot-on-system-off`: Automatically reboot when system is powered off

## Platform Configuration

The platform provides several configuration constants:

### Memory Layout
- `PHYS_VIRT_OFFSET`: Physical to virtual memory offset (`0xffff_8000_0000_0000`)
- `BOOT_STACK_SIZE`: Boot stack size (256KB)

### Timer
- `TIMER_FREQUENCY`: Timer frequency (100 MHz)

## Building

This crate targets `x86_64-unknown-none` and requires a bare-metal Rust toolchain.

```bash
cargo build --target x86_64-unknown-none
```

## Running with QEMU

To run a system using this platform implementation:

```bash
qemu-system-x86_64 -machine q35 -kernel your_kernel_binary -serial stdio
```

## Documentation

The crate is documented with rustdoc comments. Build the documentation with:

```bash
cargo doc --target x86_64-unknown-none
```

## Safety and `unsafe` Code

This crate contains `unsafe` code as it directly interfaces with hardware. All safety invariants are documented in comments and the code follows Rust's unsafe code guidelines.

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) file for details.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be licensed under Apache-2.0, without any additional terms or conditions.

## Credits

Developed by the Axvisor Team.

