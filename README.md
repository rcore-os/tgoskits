# AxVCpu

[![CI](https://github.com/arceos-hypervisor/x86_vcpu/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/arceos-hypervisor/x86_vcpu/actions/workflows/ci.yml)

AxVCpu is a virtual CPU abstraction library for ArceOS hypervisors, providing a unified, architecture-independent interface for managing virtual CPUs in hypervisor environments.

## Features

- **Architecture Agnostic**: Unified interface supporting multiple architectures (x86_64, ARM64, RISC-V)
- **State Management**: Robust VCpu lifecycle management with clear state transitions
- **Per-CPU Virtualization**: Efficient per-CPU state management and resource isolation  
- **Hardware Abstraction**: Clean separation between architecture-specific and common operations
- **CPU Affinity**: Support for CPU binding and affinity management
- **Exit Handling**: Comprehensive VM exit reason handling and processing

## Architecture

AxVCpu follows a layered architecture design:

```
┌─────────────────────────────────────────┐
│            Application Layer            │  ← Hypervisor/VMM
├─────────────────────────────────────────┤
│         AxVCpu Core Interface           │  ← Main API
├─────────────────────────────────────────┤
│         Architecture Abstraction        │  ← AxArchVCpu trait
├─────────────────────────────────────────┤
│       Hardware Abstraction Layer        │  ← AxVCpuHal trait
├─────────────────────────────────────────┤
│     Architecture-Specific Backends      │  ← x86_64, ARM64, etc.
└─────────────────────────────────────────┘
```

## Core Components

### VCpu State Machine

```
Created → Free → Ready → Running → Blocked
    ↓       ↓      ↓        ↓        ↓
    └───────┴──────┴────────┴────────┘
                 Invalid
```

- **Created**: Initial state after VCpu creation
- **Free**: Initialized and ready to be bound to a physical CPU
- **Ready**: Bound to a physical CPU and ready for execution
- **Running**: Currently executing on a physical CPU
- **Blocked**: Execution blocked (waiting for I/O, etc.)
- **Invalid**: Error state when transitions fail

### Key Traits

- `AxArchVCpu`: Architecture-specific VCpu implementation interface
- `AxVCpuHal`: Hardware abstraction layer for hypervisor operations
- `AxVCpuExitReason`: VM exit reason enumeration and handling

## Quick Start

Add AxVCpu to your `Cargo.toml`:

```toml
[dependencies]
axvcpu = "0.1.0"
```

### Basic Usage

```rust
use axvcpu::{AxVCpu, VCpuState};

// Mock implementation for example
struct MyArchVCpu;
impl AxArchVCpu for MyArchVCpu {
    // Implement required methods...
}

// Create a new virtual CPU
let vcpu = AxVCpu::<MyArchVCpu>::new(
    vm_id,       // VM identifier
    vcpu_id,     // VCpu identifier  
    favor_cpu,   // Preferred physical CPU
    cpu_set,     // CPU affinity mask
    config       // Architecture-specific config
)?;

// Check VCpu state
assert_eq!(vcpu.state(), VCpuState::Created);

// Setup the VCpu
vcpu.setup(entry_addr, ept_root, setup_config)?;

// Bind to current physical CPU and run
vcpu.bind()?;
let exit_reason = vcpu.run()?;

// Handle VM exit
match exit_reason {
    AxVCpuExitReason::Halt => {
        println!("Guest halted");
    },
    AxVCpuExitReason::Io { port, is_write, .. } => {
        println!("I/O access on port {}", port);
    },
    // ... handle other exit reasons
}
```

## Architecture Implementation

To implement AxVCpu for a new architecture:

```rust
use axvcpu::AxArchVCpu;

struct MyArchVCpu {
    // Architecture-specific fields
}

impl AxArchVCpu for MyArchVCpu {
    type CreateConfig = MyCreateConfig;
    type SetupConfig = MySetupConfig;

    fn new(vm_id: VMId, vcpu_id: VCpuId, config: Self::CreateConfig) -> AxResult<Self> {
        // Initialize architecture-specific VCpu
        Ok(Self { /* ... */ })
    }

    fn set_entry(&mut self, entry: GuestPhysAddr) -> AxResult {
        // Set guest entry point
        Ok(())
    }

    fn set_ept_root(&mut self, ept_root: HostPhysAddr) -> AxResult {
        // Configure memory virtualization
        Ok(())
    }

    fn setup(&mut self, config: Self::SetupConfig) -> AxResult {
        // Complete VCpu initialization
        Ok(())
    }

    fn run(&mut self) -> AxResult<AxVCpuExitReason> {
        // Execute guest code until VM exit
        Ok(AxVCpuExitReason::Halt)
    }

    // Implement other required methods...
}
```

## Related Projects

- [ArceOS](https://github.com/arceos-org/arceos) - A component-based OS kernel
- [AxVisor](https://github.com/arceos-hypervisor/axvisor) - A hypervisor implemented based on the ArceOS unikernel framework.

## License

This project is licensed under multiple licenses. You may choose to use this project under any of the following licenses:

- **[GPL-3.0-or-later](LICENSE.GPLv3)** - GNU General Public License v3.0 or later
- **[Apache-2.0](LICENSE.Apache2)** - Apache License 2.0
- **[MulanPubL-2.0](LICENSE.MulanPubL2)** - Mulan Public License 2.0
- **[MulanPSL2](LICENSE.MulanPSL2)** - Mulan Permissive Software License v2

You may use this software under the terms of any of these licenses at your option.
