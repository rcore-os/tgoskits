# x86_vcpu

[![CI](https://github.com/arceos-hypervisor/x86_vcpu/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/arceos-hypervisor/x86_vcpu/actions/workflows/ci.yml)

Definition of the vCPU structure and virtualization-related interface support for x86_64 architecture.

The crate user must implement the `AxVCpuHal` trait to provide the required low-level implementantion, 
relevant implementation can refer to [axvcpu](https://github.com/arceos-hypervisor/axvcpu/blob/main/src/hal.rs). **[Work in Progress]**

## Features

- **VMX Support**: Complete Intel VT-x virtualization support
- **SVM Support**: AMD SVM (Secure Virtual Machine) virtualization support (feature flag `svm`)
- **Register Management**: Comprehensive x86 register state management
- **EPT (Extended Page Tables)**: Memory virtualization support
- **MSR Handling**: Model-Specific Register access and management
- **VMCS Management**: Virtual Machine Control Structure operations
- **Interrupt Handling**: Virtual interrupt and exception processing
- **Tracing Support**: Optional tracing for debugging (feature flag `tracing`)

## Architecture

The library is structured into several key modules:

### Core Components

- **`vmx/`**: Intel VMX virtualization implementation
  - `vcpu.rs`: Virtual CPU implementation ([`VmxArchVCpu`](src/vmx/vcpu.rs))
  - `vmcs.rs`: VMCS (Virtual Machine Control Structure) management
  - `percpu.rs`: Per-CPU state management ([`VmxArchPerCpuState`](src/vmx/percpu.rs))
  - `definitions.rs`: VMX constants and exit reasons
  - `instructions.rs`: VMX instruction wrappers
  - `structs.rs`: VMX data structures

- **`regs/`**: Register management
  - `accessors.rs`: Register access utilities
  - `diff.rs`: Register state comparison
  - `mod.rs`: General-purpose registers ([`GeneralRegisters`](src/regs/mod.rs))

- **`ept.rs`**: Extended Page Tables implementation
- **`msr.rs`**: Model-Specific Register handling

### Key Types

- [`VmxArchVCpu`](src/vmx/vcpu.rs): Main virtual CPU implementation
- [`VmxArchPerCpuState`](src/vmx/percpu.rs): Per-CPU virtualization state
- [`GeneralRegisters`](src/regs/mod.rs): x86-64 general-purpose registers
- [`VmxExitReason`](src/vmx/definitions.rs): VM exit reason enumeration
- [`GuestPageWalkInfo`](src/ept.rs): Guest page walk information

### Basic Example

```rust
use x86_vcpu::{has_hardware_support, GeneralRegisters};

// Check if VMX is supported on this hardware
if has_hardware_support() {
    println!("VMX hardware support detected");
} else {
    println!("VMX hardware support not available");
}

// Create and initialize guest registers
let mut regs = GeneralRegisters::default();
regs.rax = 0x1234;
regs.rbx = 0x5678;

println!("Guest registers initialized:");
println!("RAX: {:#x}", regs.rax);
println!("RBX: {:#x}", regs.rbx);

// Display register names
for (i, name) in GeneralRegisters::REGISTER_NAMES.iter().enumerate() {
    println!("Register {}: {}", i, name);
}
```

## Features

The library supports the following Cargo features:

- **`default`**: Enables VMX support by default
- **`vmx`**: Intel VMX (VT-x) support
- **`svm`**: AMD SVM support
- **`tracing`**: Enable tracing for debugging

## Related Projects 

+ [ArceOS](https://github.com/arceos-org/arceos) - An experimental modular OS (or Unikernel)
+ [AxVisor](https://github.com/arceos-hypervisor/axvisor) - Hypervisor implementation

## License

This project is licensed under multiple licenses. You may choose to use this project under any of the following licenses:

- **[GPL-3.0-or-later](LICENSE.GPLv3)** - GNU General Public License v3.0 or later
- **[Apache-2.0](LICENSE.Apache2)** - Apache License 2.0
- **[MulanPubL-2.0](LICENSE.MulanPubL2)** - Mulan Public License 2.0
- **[MulanPSL2](LICENSE.MulanPSL2)** - Mulan Permissive Software License v2

You may use this software under the terms of any of these licenses at your option.
