//! Native RISC-V EFI entry before the firmware-backed Rust allocator is used.

core::arch::global_asm!(include_str!("entry.S"));
