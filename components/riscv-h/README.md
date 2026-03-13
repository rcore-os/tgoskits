# riscv-h

[![CI](https://github.com/arceos-hypervisor/riscv-h/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/arceos-hypervisor/riscv-h/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/riscv-h.svg)](https://crates.io/crates/riscv-h)
[![Documentation](https://docs.rs/riscv-h/badge.svg)](https://docs.rs/riscv-h)

RISC-V Hypervisor Extension Register Support

A Rust crate providing low-level access to RISC-V hypervisor extension registers. This crate implements the hypervisor Control and Status Registers (CSRs) defined in the RISC-V Hypervisor Extension specification, enabling virtualization support on RISC-V processors. 

## Features

- **No-std compatible**: Designed for bare-metal and OS kernel development
- **Type-safe register access**: Bitfield manipulation with compile-time safety
- **Comprehensive CSR coverage**: All hypervisor and virtual supervisor registers
- **Well-tested**: Extensive unit tests for all register implementations

## Supported Registers

### Hypervisor Control Registers

| Register | Description | CSR Address |
|----------|-------------|-------------|
| `hstatus` | Hypervisor status register | 0x600 |
| `hedeleg` | Hypervisor exception delegation | 0x602 |
| `hideleg` | Hypervisor interrupt delegation | 0x603 |
| `hie` | Hypervisor interrupt enable | 0x604 |
| `hcounteren` | Hypervisor counter enable | 0x606 |
| `hgatp` | Hypervisor guest address translation and protection | 0x680 |

### Virtual Supervisor Registers

| Register | Description | CSR Address |
|----------|-------------|-------------|
| `vsstatus` | Virtual supervisor status | 0x200 |
| `vsie` | Virtual supervisor interrupt enable | 0x204 |
| `vstvec` | Virtual supervisor trap vector | 0x205 |
| `vsscratch` | Virtual supervisor scratch | 0x240 |
| `vsepc` | Virtual supervisor exception PC | 0x241 |
| `vscause` | Virtual supervisor cause | 0x242 |
| `vstval` | Virtual supervisor trap value | 0x243 |
| `vsatp` | Virtual supervisor address translation and protection | 0x280 |

### Additional Registers

- **Interrupt Management**: `hip`, `hvip`, `hgeie`, `hgeip`
- **Time Management**: `htimedelta`, `htimedeltah` 
- **Trap Information**: `htval`, `htinst`
- **Virtual Supervisor Interrupts**: `vsip`

## Quick Start

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
riscv-h = "0.1.0"
```

### Basic Usage

```rust
#![no_std]

use riscv_h::register::{hstatus, hgatp};

fn setup_hypervisor() {
    // Read current hypervisor status
    let hstatus = hstatus::read();
    
    // Check if we're in virtualized mode
    if hstatus.spv() {
        // Handle virtualized context
        setup_guest_translation();
    }
}

fn setup_guest_translation() {
    unsafe {
        // Configure guest address translation
        let mut hgatp = hgatp::Hgatp::from_bits(0);
        hgatp.set_mode(hgatp::HgatpValues::Sv48x4);
        hgatp.set_vmid(1);
        hgatp.set_ppn(0x1000); // Root page table PPN
        hgatp.write();
    }
}
```

### Register Field Access

```rust
use riscv_h::register::hstatus;

// Read register value
let hstatus_val = hstatus::read();

// Access individual fields
let vsxl = hstatus_val.vsxl();      // Virtual supervisor XLEN
let vtw = hstatus_val.vtw();        // Trap WFI
let vtsr = hstatus_val.vtsr();      // Trap SRET
let vgein = hstatus_val.vgein();    // Virtual guest external interrupt number

// Modify register (create new value)
let mut new_hstatus = hstatus::Hstatus::from_bits(0);
new_hstatus.set_vtw(true);          // Enable WFI trapping
new_hstatus.set_hu(true);           // Enable hypervisor user mode

unsafe {
    new_hstatus.write();             // Write to CSR
}
```

### Exception and Interrupt Delegation

```rust
use riscv_h::register::{hedeleg, hideleg};

unsafe {
    // Delegate common exceptions to VS-mode
    hedeleg::set_ex2(true);  // Illegal instruction
    hedeleg::set_ex8(true);  // Environment call from U-mode
    hedeleg::set_ex12(true); // Instruction page fault
    hedeleg::set_ex13(true); // Load page fault
    hedeleg::set_ex15(true); // Store page fault
    
    // Delegate timer and software interrupts to VS-mode
    hideleg::set_vstie(true);  // VS-mode timer interrupt
    hideleg::set_vssie(true);  // VS-mode software interrupt
}
```

## Architecture Support

- **RISC-V 64-bit (RV64)**: Full support for all hypervisor extension registers
- **Privilege Levels**: HS-mode, VS-mode, VU-mode register access
- **Memory Management**: Two-stage address translation support

## Safety

This crate provides `unsafe` functions for writing to CSRs, as register modifications can affect system behavior. Users must ensure:

- Proper privilege level (HS-mode) when accessing hypervisor CSRs
- Valid field values according to RISC-V specification
- Correct synchronization when modifying shared state

## License

Riscv-h is licensed under the Apache License, Version 2.0. See the [LICENSE](./LICENSE) file for details.