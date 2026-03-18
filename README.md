<h1 align="center">aarch64_sysreg</h1>

<p align="center">AArch64 System Register Type Definitions</p>

<div align="center">
[![Crates.io](https://img.shields.io/crates/v/aarch64_sysreg.svg)](https://crates.io/crates/aarch64_sysreg)
[![Docs.rs](https://docs.rs/aarch64_sysreg/badge.svg)](https://docs.rs/aarch64_sysreg)
[![Rust](https://img.shields.io/badge/edition-2021-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/arceos-org/aarch64_sysreg/blob/main/LICENSE)
</div>

English | [中文](README_CN.md)

# Introduction

A library providing type definitions for AArch64 system registers, including operation types, register types, and system register enumerations for the ARM64 architecture.

## Features

- `#![no_std]` - Compatible with bare-metal environments
- `OperationType` - AArch64 instruction operation type enumeration
- `RegistersType` - General register type enumeration (W/X/V/B/H/S/D/Q registers, etc.)
- `SystemRegType` - System register type enumeration

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
aarch64_sysreg = "0.1"
```

## Usage

```rust
use aarch64_sysreg::{OperationType, RegistersType, SystemRegType};

fn main() {
    // Operation type
    let op = OperationType::ADD;
    println!("Operation: {}", op);           // ADD
    println!("Value: 0x{:x}", op);           // 0x6

    // Convert from value
    let op_from = OperationType::from(0x6);
    assert_eq!(op_from, OperationType::ADD);

    // Register type
    let reg = RegistersType::X0;
    println!("Register: {}", reg);           // X0

    // System register
    let sys_reg = SystemRegType::MDSCR_EL1;
    println!("System Register: {}", sys_reg); // MDSCR_EL1
}
```

## Type Reference

### OperationType

Defines AArch64 instruction operation types, including:
- Arithmetic: `ADD`, `SUB`, `MUL`, `DIV`, etc.
- Logical: `AND`, `ORR`, `EOR`, `BIC`, etc.
- Branch: `B`, `BL`, `BR`, `RET`, etc.
- Load/Store: `LDR`, `STR`, `LDP`, `STP`, etc.
- System: `MSR`, `MRS`, `SVC`, `HVC`, etc.

### RegistersType

Defines AArch64 general-purpose and vector registers:
- 32-bit GPR: `W0` - `W30`, `WZR`, `WSP`
- 64-bit GPR: `X0` - `X30`, `XZR`, `SP`
- Vector registers: `V0` - `V31`, `B0` - `B31`, `H0` - `H31`, `S0` - `S31`, `D0` - `D31`, `Q0` - `Q31`
- SVE registers: `Z0` - `Z31`, `P0` - `P15`

### SystemRegType

Defines AArch64 system registers with encoding format `<op0><op2><op1><CRn>00000<CRm>0`:
- Debug registers: `DBGBCR*_EL1`, `DBGBVR*_EL1`, etc.
- Trace registers: `TRC*` series
- Performance registers: `PMEVCNTR*_EL0`, `PMEVTYPER*_EL0`, etc.
- System control registers: `SCTLR_EL1`, `TTBR*_EL1`, etc.

# Verification

Run local tests quickly:

```bash
./scripts/test.sh
```

# Documentation

## API Documentation

```bash
cargo doc --no-deps --open
```

# Contributing

1. Run local check: `./scripts/check.sh`
2. Run local tests: `./scripts/test.sh`
3. Submit PR and pass CI checks

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for details.
