# Rockchip Power Management Driver (rockchip-pm)

A Rust library for Rockchip SoC power management, based on the Linux kernel driver `pm_domains.c`.

## Features

- 🔋 **Complete Power Domain Control**: Support for RK3568 and RK3588 power domains
- 🚀 **Full-featured Implementation**: Memory power control, bus idle management, and complete power sequencing
- 🛡️ **Memory Safe**: Leveraging Rust's type system for memory safety and thread safety
- 📋 **No Standard Library**: `#![no_std]` design suitable for embedded environments
- 🎯 **Hardware Accurate**: Direct translation from Linux kernel implementation
- 🔌 **Multi-Chip Support**: Extensible architecture supporting multiple Rockchip SoC families

## Supported Power Domains

## Usage Example

```rust
use rockchip_pm::{RockchipPM, RkBoard, PowerDomain, RK3568, RK3588};
use core::ptr::NonNull;

// Initialize PMU for RK3588 (base address should be obtained from device tree)
let pmu_base = unsafe { NonNull::new_unchecked(0xfd8d8000 as *mut u8) };
let mut pm_rk3588 = RockchipPM::new(pmu_base, RkBoard::Rk3588);

// Method 1: Use chip-specific constants (Recommended)
pm_rk3588.power_domain_on(RK3588::NPU1)?;    // NPU core 1

// Method 2: Create PowerDomain directly with ID
let NPU1 = PowerDomain::new(10);
pm_rk3588.power_domain_on(NPU1)?;  // NPU1 (ID: 10)
```

### Choosing the Right Method

**Method 1 (Named Constants)** - Recommended for most use cases:
- ✅ Clear and self-documenting code
- ✅ Compile-time verification
- ✅ IDE autocomplete support
- ✅ Prevents using wrong IDs

**Method 2 (Direct ID)** - Use when:
- Dynamic power domain selection is needed
- Working with power domain IDs from configuration files
- Interfacing with external systems that use raw IDs
- Need to query power domain ID: `domain.id()`
```

## Architecture

The library implements a complete power management sequence:

### Power-On Sequence
1. **Memory Power**: Power on domain memory (if available)
2. **Bus Idle Cancel**: Cancel bus idle requests
3. **Main Power**: Power on the main domain
4. **Repair Wait**: Wait for repair operations to complete
5. **State Verification**: Verify power state is stable

### Power-Off Sequence  
1. **Bus Idle Request**: Request bus to enter idle state
2. **Main Power**: Power off the main domain
3. **State Verification**: Verify power state is stable
4. **Memory Power**: Power off domain memory (if available)

### Module Structure

```
rockchip-pm/
├── src/
│   ├── lib.rs              # Main API and RockchipPM struct
│   ├── power_sequencer.rs  # Complete power control sequencing
│   ├── memory_control.rs   # Memory power management
│   ├── idle_control.rs     # Bus idle control
│   ├── qos_control.rs      # QoS register save/restore
│   ├── registers/          # Register definitions and access
│   └── variants/           # Chip-specific implementations
│       ├── mod.rs          # Common structures
│       ├── _macros.rs      # Domain definition macros
│       ├── rk3568.rs       # RK3568-specific domains
│       └── rk3588.rs       # RK3588-specific domains
└── tests/
    └── test.rs             # Integration tests
```

## Advanced Features

### Dependency Management

Power domains may have parent-child relationships that must be respected during power transitions:

- **Parent Dependencies**: Child domains require their parent domain to be powered on first
- **Child Dependencies**: Parent domains require all child domains to be powered off first
- **Safe Sequencing**: Use `_with_deps` methods to enforce dependency checking

#### Usage Example

```rust
use rockchip_pm::{RockchipPM, RkBoard, RK3588};

let mut pm = RockchipPM::new(pmu_base, RkBoard::Rk3588);

// Power on with dependency checking
// Example: NPU1 requires NPUTOP to be powered on first
pm.power_domain_on_with_deps(RK3588::NPUTOP)?;  // Power on parent first
pm.power_domain_on_with_deps(RK3588::NPU1)?;    // Then power on child

// Power off with dependency checking  
// Child domains must be powered off before parent
pm.power_domain_off_with_deps(RK3588::NPU1)?;   // Power off child first
pm.power_domain_off_with_deps(RK3588::NPUTOP)?; // Then power off parent

// Query currently active domains
let active = pm.get_active_domains();
for domain in active {
    println!("Domain {} is active", domain.id());
}
```

#### Dependency Error Handling

If dependencies are not met, operations will fail with `PowerError::DependencyNotMet`:

```rust
// This will fail if NPUTOP is not powered on
match pm.power_domain_on_with_deps(RK3588::NPU1) {
    Ok(()) => println!("NPU1 powered on successfully"),
    Err(PowerError::DependencyNotMet) => {
        println!("Parent domain NPUTOP must be powered on first");
        // Power on parent first
        pm.power_domain_on_with_deps(RK3588::NPUTOP)?;
        pm.power_domain_on_with_deps(RK3588::NPU1)?;
    }
    Err(e) => return Err(e),
}
```

### QoS (Quality of Service) Management

The library includes comprehensive QoS infrastructure for managing hardware QoS settings:

- **Automatic QoS Preservation**: QoS settings (priority, mode, bandwidth, saturation, extcontrol) are saved before power domain shutdown
- **Seamless Restoration**: QoS configuration is automatically restored when the domain powers back on
- **Multi-port Support**: Each power domain can have multiple QoS ports (up to 8)
- **Five Register Types**: 
  - Priority (`0x08`): Bus access priority
  - Mode (`0x0c`): QoS mode control
  - Bandwidth (`0x10`): Bandwidth limitation
  - Saturation (`0x14`): Saturation threshold
  - ExtControl (`0x18`): Extended control

#### Configured QoS Domains (RK3588)

The following domains have QoS configuration:
- **GPU**: 2 QoS ports @ 0xFDF35000
- **NPU**: 4 QoS ports @ 0xFDF40000
- **VCODEC**: 3 QoS ports @ 0xFDF78000
- **VENC0**: 2 QoS ports @ 0xFDF50000
- **RKVDEC0**: 2 QoS ports @ 0xFDF48000
- **VOP**: 4 QoS ports @ 0xFDF60000
- **VI**: 2 QoS ports @ 0xFDF70000

#### QoS State Persistence

QoS states are maintained across power cycles:

```rust
// Check if domain has saved QoS state
if pm.has_qos_state(RK3588::GPU) {
    println!("GPU has saved QoS configuration");
}

// Clear QoS state for a specific domain
pm.clear_qos_state(RK3588::GPU);

// Clear all QoS states
pm.clear_all_qos_states();
```

#### QoS Integration

QoS save/restore is automatically integrated into the power sequencing:

```rust
// Power off sequence includes QoS save
pm.power_domain_off(RK3588::GPU)?;  // QoS automatically saved

// Power on sequence includes QoS restore  
pm.power_domain_on(RK3588::GPU)?;   // QoS automatically restored
```

**Note**: QoS operations are performed transparently during power transitions. No explicit QoS management is required from the user code. The integration ensures that performance-critical QoS settings are preserved across power cycles.

### RK3588 Domain Dependencies

The following parent-child relationships are configured:

| Parent Domain | Child Domains                  | Description                            |
| ------------- | ------------------------------ | -------------------------------------- |
| **NPUTOP**    | NPU1, NPU2                     | Neural Processing Unit hierarchy       |
| **VCODEC**    | VENC0, VENC1, RKVDEC0, RKVDEC1 | Video codec hierarchy                  |
| **VOP**       | VO0, VO1                       | Video Output Processor hierarchy       |
| **VI**        | ISP1                           | Video Input and Image Signal Processor |

**Power-On Rule**: Parent must be powered on before any children  
**Power-Off Rule**: All children must be powered off before parent

## Memory Mapping Requirements

To use this library, ensure:

1. **Correct PMU Base Address**: 
   - RK3588: Usually `0xfd8d8000` (verify from device tree)
   - RK3568: Usually `0xfdd90000` (verify from device tree)
2. **Memory Mapping Permissions**: Read/write access to PMU register region
3. **Clock Configuration**: Ensure PMU clocks are properly configured

## Important Notes

⚠️ **CRITICAL**: This library directly manipulates hardware registers. Before use:

- System PMU hardware must be properly initialized
- No other drivers should control the same power domains concurrently
- Perform thorough validation before testing on real hardware

## Build and Test

### Environment Setup

```bash
# Install required tools
cargo install ostool

# Add target architecture support
rustup target add aarch64-unknown-none-softfloat
```

### Building

```bash
# Build library
cargo build

# Build release version
cargo build --release

# Check for errors
cargo check
```

### Running Tests

The test suite includes comprehensive unit and integration tests:

**Test Categories:**
- **Unit Tests**: DependencyManager functionality (4 tests)
- **QoS State Tests**: QoS state management (1 test)
- **Dependency Enforcement**: Parent-child power sequencing (4 tests)
- **Complex Hierarchies**: Multi-level dependencies (2 tests)
- **Edge Cases**: Error handling and invalid inputs (3 tests)
- **Integration Tests**: Real hardware testing (3 tests)

**Total: 17 comprehensive test cases**

```bash
# Run on development board (requires U-Boot environment)
cargo uboot
```

**Test Coverage:**
- ✅ DependencyManager state tracking
- ✅ Parent-child power sequencing enforcement
- ✅ Multi-level dependency hierarchies (VCODEC with 4 children)
- ✅ QoS state persistence
- ✅ Independent domain operations
- ✅ Error handling for invalid domains
- ✅ Active domain tracking
- ✅ Real NPU hardware verification

## Technical Features

### 🔒 Safety

- **Memory Safety**: Compile-time guarantees prevent dangling pointers
- **Type Safety**: Strong typing for power domains and states
- **Thread Safety**: Built-in synchronization mechanisms
- **Boundary Checks**: Automatic prevention of buffer overflows

### 🚀 Extensibility

- **Modular Design**: Trait-based register access abstraction
- **Easy Extension**: Simple addition of new power domains
- **Plugin Support**: Custom power policies and optimization algorithms
- **Platform Adaptation**: Easy porting to other Rockchip series chips

### 📱 Embedded Friendly

- **no-std Support**: Suitable for bare-metal environments
- **Small Memory Footprint**: Optimized memory usage
- **Efficient Access**: Direct memory-mapped I/O with minimal overhead
- **Real-time Response**: Low-latency power control

## Dependencies

### Core Dependencies

- **rdif-base**: Device driver framework
- **tock-registers**: Type-safe register access and bitfield operations
- **mbarrier**: Memory barrier primitives for register access ordering

### Development Dependencies

- **bare-test**: Bare-metal testing framework

### System Requirements

- **Rust Version**: 1.75.0 or higher
- **Target Architecture**: aarch64-unknown-none-softfloat
- **Development Environment**: Linux/macOS/Windows + Rust toolchain
- **Deployment Environment**: RK3588/RK3588S development board

## Hardware Compatibility

### Supported Chips
- **RK3568**: Quad-core ARM Cortex-A55 SoC with integrated NPU and GPU
- **RK3588**: Octa-core ARM Cortex-A55/A76 flagship SoC
- **RK3588S**: Cost-optimized variant of RK3588

### Development Boards
- **RK3568 Boards**:
  - NanoPi R5S/R5C
  - ROCK 3A/3B/3C
  - Radxa E25
  - Other RK3568-based boards
- **RK3588 Boards**:
  - Orange Pi 5/5 Plus/5B
  - Rock 5A/5B/5C
  - NanoPC-T6
  - Other RK3588/RK3588S-based boards

### Hardware Features
- **CPU Architecture**: ARM Cortex-A55/A76 heterogeneous cores
- **GPU**: Mali-G52 (RK3568) / Mali-G610 MP4 (RK3588)
- **NPU**: 1 TOPS (RK3568) / 6 TOPS (RK3588) AI accelerator

## License

This project is based on the same GPL-2.0 license as the Linux kernel.

## Contributing

Contributions are welcome! Please submit Issues and Pull Requests.

### Development Setup

```bash
# Clone the project
git clone <repository-url>
cd rockchip-pm

# Install dependencies
rustup component add rustfmt clippy

# Format code
cargo fmt

# Run linter
cargo clippy
```

## References

- Linux Kernel `drivers/soc/rockchip/pm_domains.c`
- RK3588 Technical Reference Manual
- Device Tree Bindings for Rockchip Power Domains

---

**Note**: This driver is low-level system software. Ensure that hardware register operations comply with chip specifications. Perform thorough testing before use in production environments.
