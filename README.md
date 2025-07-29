# **riscv_vcpu**

riscv64 virtual CPU (vCPU) implementation for hypervisors. This crate provides the core vCPU structure and virtualization-related interface support specifically designed for the riscv64 architecture.

[![CI](https://github.com/arceos-hypervisor/riscv_vcpu/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/arceos-hypervisor/riscv_vcpu/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)]

## Overview

riscv_vcpu implements a minimal RISC-V Virtual CPU (VCPU) abstraction layer compliant with the RISC-V Hypervisor Extension (RVH). Designed for embedded hypervisors and educational use, it can operates in no_std environments.

## Features

- **Complete vCPU Implementation**: Full virtual CPU structure for riscv64 guests
- **Exception Handling**: Comprehensive trap and exception handling for virtualized environments
- **EPT (Extended Page Tables)**: Memory virtualization support
- **VMCS Management**: Virtual Machine Control Structure operations
- **Per-CPU Support**: Efficient per-CPU data structures and management
- **No-std Compatible**: Works in bare-metal and embedded environments

## Usage

Add this to your `Cargo.toml`:

```toml
[dependencies]
riscv_vcpu = "0.1"
```

## Basic Usage

```rust
use riscv_vcpu::{RISCVVCpu, RISCVVCpuCreateConfig, has_hardware_support};

// Check if hardware virtualization is supported
if has_hardware_support() {
    // Create vCPU configuration
    let config = RISCVVCpuCreateConfig::default();
    
    // Create and configure the virtual CPU
    let vcpu = RISCVVCpu::new(config)?;
    
    // Run the virtual CPU
    vcpu.run()?;
}
```

## Related Projects 

+ [ArceOS](https://github.com/arceos-org/arceos) - An experimental modular OS (or Unikernel)
+ [AxVisor](https://github.com/arceos-hypervisor/axvisor) - Hypervisor implementation

## License

This project is dual-licensed under either:

- MIT License ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)

at your option.