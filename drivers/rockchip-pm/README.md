# Rockchip Power Management Driver (rockchip-pm)

A Rust power management driver library for Rockchip SoCs, providing basic power domain control functionality.

## Features

- 🔋 **Basic Power Domain Control**: Support for power domain on/off operations on RK3588
- 🛡️ **Memory Safety**: Leverages Rust's type system for memory and thread safety
- 📋 **No Standard Library**: `#![no_std]` design suitable for embedded environments
- 🎯 **Hardware Accurate**: Direct register access with no abstraction overhead
- 🔌 **Name Lookup**: Support for looking up power domains by name
- 📦 **Driver Framework**: Built on rdif-base driver framework

## Quick Start

```rust
use rockchip_pm::{RockchipPM, RkBoard, PowerDomain};
use core::ptr::NonNull;

// Initialize RK3588 PMU (base address typically from device tree)
let pmu_base = unsafe { NonNull::new_unchecked(0xfd8d8000 as *mut u8) };
let mut pm = RockchipPM::new(pmu_base, RkBoard::Rk3588);

// Control power domain by ID
let npu_domain = PowerDomain::new(8);  // NPU main domain
pm.power_domain_on(npu_domain)?;

// Look up power domain by name
if let Some(npu) = pm.get_power_dowain_by_name("npu") {
    pm.power_domain_on(npu)?;
}

// Turn off power domain
pm.power_domain_off(npu)?;
```

## API Documentation

### Core Structure

```rust
pub struct RockchipPM {
    // Private fields: board info, register interface, power domain configuration
}
```

### Board Support

```rust
pub enum RkBoard {
    Rk3588,  // Implemented
    Rk3568,  // Not implemented (placeholder)
}
```

### Power Domain Type

```rust
pub struct PowerDomain {
    // Power domain ID
}
impl PowerDomain {
    pub fn new(id: u32) -> Self
    pub fn id(&self) -> u32
}
```

### Error Handling

```rust
pub enum NpuError {
    DomainNotFound,  // Power domain does not exist
    Timeout,         // Operation timeout
    HardwareError,   // Hardware error
}

pub type NpuResult<T> = Result<T, NpuError>;
```

### Main Methods

```rust
impl RockchipPM {
    /// Create a new power manager instance
    pub fn new(base: NonNull<u8>, board: RkBoard) -> Self

    /// Look up power domain by name
    pub fn get_power_dowain_by_name(&self, name: &str) -> Option<PowerDomain>

    /// Turn on specified power domain
    pub fn power_domain_on(&mut self, domain: PowerDomain) -> NpuResult<()>

    /// Turn off specified power domain
    pub fn power_domain_off(&mut self, domain: PowerDomain) -> NpuResult<()>
}
```

## Supported Power Domains (RK3588)

### Compute Domains
- **NPU** (ID: 8) - Neural Processing Unit main domain
- **NPUTOP** (ID: 9) - NPU top domain
- **NPU1** (ID: 10) - NPU core 1
- **NPU2** (ID: 11) - NPU core 2

### Graphics Domains
- **GPU** (ID: 0) - Graphics Processing Unit
- **VOP** (ID: 26) - Video Output Processor
- **VO0** (ID: 27) - Video Output 0
- **VO1** (ID: 28) - Video Output 1

### Video Domains
- **VCODEC** (ID: 4) - Video Codec main domain
- **VENC0** (ID: 5) - Video Encoder 0
- **VENC1** (ID: 6) - Video Encoder 1
- **RKVDEC0** (ID: 7) - Rockchip Video Decoder 0
- **RKVDEC1** (ID: 12) - Rockchip Video Decoder 1
- **AV1** (ID: 18) - AV1 Decoder
- **VDPU** (ID: 2) - Video Processing Unit

### Image Domains
- **VI** (ID: 29) - Video Input
- **ISP1** (ID: 30) - Image Signal Processor
- **RGA30** (ID: 15) - Raster Graphics Accelerator 30
- **RGA31** (ID: 16) - Raster Graphics Accelerator 31

### Bus Domains
- **PHP** (ID: 17) - PHP Controller
- **GMAC** (ID: 19) - Gigabit Ethernet MAC
- **PCIE** (ID: 20) - PCIe Controller
- **SDIO** (ID: 21) - SDIO Controller
- **USB** (ID: 22) - USB Controller
- **SDMMC** (ID: 23) - SD/MMC Controller

### Other Domains
- **AUDIO** (ID: 1) - Audio Subsystem
- **FEC** (ID: 24) - Forward Error Correction
- **NVM** (ID: 25) - Non-Volatile Memory
- **NVM0** (ID: 3) - NVM domain 0

## Project Structure

```
rockchip-pm/
├── src/
│   ├── lib.rs              # Main API and RockchipPM structure
│   ├── registers/mod.rs    # Register definitions and access abstraction
│   └── variants/           # Chip-specific implementations
│       ├── mod.rs          # PowerDomain type and common structures
│       ├── _macros.rs      # Power domain definition macros
│       └── rk3588.rs       # RK3588 power domain definitions
├── tests/
│   └── test.rs             # NPU power control integration tests
├── Cargo.toml              # Project configuration and dependencies
├── build.rs                # Build script
├── rust-toolchain.toml     # Rust toolchain configuration
└── README.md               # Project documentation
```

## Building and Testing

### Requirements

- Rust 1.75+ (nightly)
- aarch64-unknown-none-softfloat target support

### Build Steps

```bash
# Add target architecture support
rustup target add aarch64-unknown-none-softfloat

# Build library
cargo build

# Build release version
cargo build --release

# Check code
cargo check
```

### Running Tests

The project includes 1 integration test that validates NPU power domain control:

```bash
# Run tests on development board (requires U-Boot environment)
cargo uboot
```

**Test Coverage:**
- ✅ RK3588 NPU related power domain on/off
- ✅ Device tree power domain parsing
- ✅ Register access verification

## Dependencies

### Core Dependencies

- **rdif-base** (v0.7): Device driver framework
- **tock-registers** (v0.10): Type-safe register access and bit manipulation
- **mbarrier** (v0.1): Memory barrier primitives for register access ordering
- **dma-api** (v0.5): DMA API support
- **log** (v0.4): Logging

### Development Dependencies

- **bare-test** (v0.7): Bare-metal testing framework

### Build Dependencies

- **bare-test-macros** (v0.2): Test macro definitions

## Hardware Compatibility

### Supported Chips

- **RK3588**: ✅ Fully implemented
- **RK3568**: ❌ Not implemented (marked as `unimplemented!()` placeholder)

### Development Boards

- **RK3588 Boards**:
  - Orange Pi 5/5 Plus/5B
  - Rock 5A/5B/5C
  - NanoPC-T6
  - Other boards based on RK3588/RK3588S

### Memory Mapping Requirements

Before using this library, ensure:

1. **Correct PMU Base Address**:
   - RK3588: Typically `0xfd8d8000` (verify from device tree)
2. **Memory Mapping Permissions**: Read/write access to PMU register regions
3. **Clock Configuration**: Ensure PMU clocks are properly configured

## How It Works

### Power On Sequence

1. Write to power control register to enable the power domain
2. Poll status register waiting for the domain to stabilize (up to 10000 loops)
3. Verify that power state was successfully enabled

### Power Off Sequence

1. Write to power control register to disable the power domain
2. Poll status register waiting for the domain to stabilize (up to 10000 loops)
3. Verify that power state was successfully disabled

## Safety Notes

⚠️ **Important**: This library directly manipulates hardware registers. Before use, ensure:

- System PMU hardware is properly initialized
- No other drivers are controlling the same power domains
- Thorough testing before use on real hardware

## License

This project is licensed under the [MIT License](LICENSE).

## Contributing

Contributions are welcome! Please submit Issues and Pull Requests.

### Development Setup

```bash
# Clone the repository
git clone https://github.com/drivercraft/rockchip-pm.git
cd rockchip-pm

# Install development tools
rustup component add rustfmt clippy

# Format code
cargo fmt

# Run code checks
cargo clippy
```

## References

- Linux kernel `drivers/soc/rockchip/pm_domains.c`
- RK3588 Technical Reference Manual
- Rockchip Power Domain Device Tree Binding Documentation

---

**Note**: This driver is low-level system software. Ensure hardware register operations comply with chip specifications. Perform thorough testing before use in production environments.
