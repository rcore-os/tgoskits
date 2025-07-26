# ARM VGIC - Virtual Generic Interrupt Controller

A Virtual Generic Interrupt Controller (VGIC) implementation for ARM architecture, designed for the ArceOS hypervisor ecosystem.

## Overview

This crate provides a comprehensive implementation of ARM's Virtual Generic Interrupt Controller (VGIC), enabling virtualized interrupt management for guest operating systems running under a hypervisor. The VGIC emulates the behavior of ARM's Generic Interrupt Controller (GIC) hardware, allowing multiple virtual machines to share the underlying interrupt controller while maintaining isolation.

## Features

- **GICv2 Support**: Complete implementation of GIC version 2 virtualization
- **GICv3 Support**: Optional GIC version 3 support (enabled with `vgicv3` feature)
- **Interrupt Types**: Support for all ARM interrupt types:
  - Software Generated Interrupts (SGI) - IDs 0-15
  - Private Peripheral Interrupts (PPI) - IDs 16-31  
  - Shared Peripheral Interrupts (SPI) - IDs 32-511
- **Virtual Timer**: Virtualized timer implementation with system register emulation
- **Memory-Mapped I/O**: Complete MMIO interface emulation for guest access
- **Multi-VCPU Support**: Proper interrupt routing and distribution across virtual CPUs

## Architecture

The crate is organized into several key components:

### Core Components

- **VGIC (`vgic.rs`)**: Main VGIC controller implementation
- **VGICD (`vgicd.rs`)**: Virtual GIC Distributor for interrupt routing
- **Interrupts (`interrupt.rs`)**: Interrupt state management and operations
- **Registers (`registers.rs`)**: GIC register definitions and access
- **List Registers (`list_register.rs`)**: Hardware list register management

## Usage

### Basic Setup

```rust
use arm_vgic::Vgic;

// Create a new VGIC instance
let vgic = Vgic::new();

// The VGIC implements BaseDeviceOps for MMIO handling
// Register it with your hypervisor's device management system
```

### Feature Flags

Enable GICv3 support:

```toml
[dependencies]
arm_vgic = { version = "*", features = ["vgicv3"] }
```

### Integration with ArceOS

This crate is designed to integrate seamlessly with the ArceOS hypervisor ecosystem:

- Uses `axdevice_base` for device abstraction
- Integrates with `axaddrspace` for memory management
- Leverages `axvisor_api` for hypervisor operations

## Memory Layout

The VGIC exposes the following memory-mapped regions to guest VMs:

- **GIC Distributor (GICD)**: `0x0800_0000` - `0x0800_FFFF` (64KB)
- **GIC CPU Interface (GICC)**: `0x0801_0000` - `0x0801_FFFF` (64KB)
- **GICv3 Redistributor (GICR)**: `0x0802_0000+` (128KB per CPU)

## Register Support

### Distributor Registers (GICD)

- `GICD_CTLR` - Distributor Control Register
- `GICD_TYPER` - Interrupt Controller Type Register  
- `GICD_IIDR` - Distributor Implementer Identification Register
- `GICD_IGROUPR` - Interrupt Group Registers
- `GICD_ISENABLER` - Interrupt Set-Enable Registers
- `GICD_ICENABLER` - Interrupt Clear-Enable Registers
- `GICD_ISPENDR` - Interrupt Set-Pending Registers
- `GICD_ICPENDR` - Interrupt Clear-Pending Registers
- `GICD_ISACTIVER` - Interrupt Set-Active Registers
- `GICD_ICACTIVER` - Interrupt Clear-Active Registers
- `GICD_IPRIORITYR` - Interrupt Priority Registers
- `GICD_ITARGETSR` - Interrupt Processor Targets Registers
- `GICD_ICFGR` - Interrupt Configuration Registers
- `GICD_SGIR` - Software Generated Interrupt Register

### CPU Interface Registers (GICC)

- `GICC_CTLR` - CPU Interface Control Register
- `GICC_PMR` - Interrupt Priority Mask Register
- `GICC_BPR` - Binary Point Register
- `GICC_IAR` - Interrupt Acknowledge Register
- `GICC_EOIR` - End Of Interrupt Register
- `GICC_RPR` - Running Priority Register
- `GICC_HPPIR` - Highest Priority Pending Interrupt Register

### ArceOS Integration

- `axdevice_base`: Device abstraction layer
- `axaddrspace`: Address space management
- `axvisor_api`: Hypervisor API and operations

## Platform Support

- **Primary**: `aarch64-unknown-none-softfloat`
- **Architecture**: ARM64/AArch64 only
- **Hypervisor**: ArceOS hypervisor

## Examples

### Interrupt Injection

```rust
// Interrupt injection is handled through the ArceOS VMM API
use axvisor_api::vmm::InterruptVector;

// Hardware interrupt injection (handled by hypervisor)
let vector = InterruptVector::new(42); // IRQ 42
// The VGIC will handle the virtualization automatically
```

### Timer Management

```rust
use arm_vgic::vtimer::get_sysreg_device;

// Get virtual timer system register devices
let timer_devices = get_sysreg_device();

// Register with your system register handler
for device in timer_devices {
    // Register device with hypervisor's system register emulation
}
```

## Testing

```bash
# Run tests
cargo test

# Run tests with all features
cargo test --all-features
```

## Contributing

This project is part of the ArceOS hypervisor ecosystem. Contributions should follow the ArceOS project guidelines and maintain compatibility with the broader ecosystem.

## Related Projects

- [axdevice_crates](https://github.com/arceos-hypervisor/axdevice_crates) - Device abstraction layer
- [axvisor_api](https://github.com/arceos-hypervisor/axvisor_api) - Hypervisor API definitions
- [axaddrspace](https://github.com/arceos-hypervisor/axaddrspace) - Address space management
