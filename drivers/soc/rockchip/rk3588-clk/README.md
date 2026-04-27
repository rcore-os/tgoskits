# RK3588 CRU Driver Library 🦀


|[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
|[![Rust](https://img.shields.io/badge/rust-2024+-orange.svg)](https://www.rust-lang.org/)
|[![Platform](https://img.shields.io/badge/platform-ARM64-green.svg)](#)


---

## 📋 Table of Contents

- [Project Introduction](#project-introduction)
- [Features](#features)
- [Quick Start](#quick-start)
  - [Requirements](#requirements)
  - [Installation](#installation)
  - [Basic Usage](#basic-usage)
- [Project Structure](#project-structure)
- [API Documentation](#api-documentation)
- [Usage Examples](#usage-examples)
- [Test Results](#test-results)
- [License](#license)

---

## 📖 Project Introduction

RK3588 CRU (Clock and Reset Unit) driver library is a Rust clock control unit driver library specifically designed for the RK3588 chip. This library provides comprehensive clock management functionality, including MMC (Memory Card Controller) clock configuration, NPU (Neural Processing Unit) clock management, and clock gating features.

This project uses a `no_std` design, making it fully suitable for bare-metal and embedded environments, with special optimizations for U-Boot bootloader environments. Through type-safe register access based on `tock-registers`, it ensures the reliability and security of hardware operations.

---

## ✨ Features

- **Complete MMC Clock Support**: Supports clock configuration and frequency management for storage controllers such as EMMC, SDIO, and SFC
- **NPU Clock Management**: Provides comprehensive NPU clock control, including frequency setting, gate enable, and status monitoring
- **Clock Gating**: Precise control over the enable/disable status of clock gates for various modules to optimize power management
- **Type-Safe Register Access**: Provides type-safe hardware register operations based on `tock-registers`
- **no_std Compatible**: Completely independent of the standard library, suitable for bare-metal and embedded environments
- **ARM64 Architecture Optimization**: Specifically optimized for the RK3588 ARM64 platform
- **U-Boot Environment Support**: Provides stable and reliable clock management features in U-Boot bootloader environment
- **Rich Clock Source Support**: Supports multiple clock sources such as PLL and OSC, with flexible divider configurations

---

## 🚀 Quick Start

### Requirements

- Rust 2024 Edition
- ARM64 development environment
- RK3588 hardware platform with U-Boot support
- ostool (for testing)

### Installation

1. Install the `ostool` dependency:

```bash
cargo install ostool
```

2. Add the project to your `Cargo.toml`:

```toml
[dependencies]
rk3588-clk = { git = "https://github.com/drivercraft/rk3588-clk.git" }
```

### Basic Usage

```rust
use rk3588_clk::{Rk3588Cru, constant::*};
use core::ptr::NonNull;

// Create CRU instance
let cru_addr = 0xfd7c0000; // RK3588 CRU base address
let cru = Rk3588Cru::new(NonNull::new(cru_addr as *mut u8).unwrap());

// Initialize CRU
cru.init();

// Configure EMMC clock frequency
let emmc_rate = 200_000_000; // 200MHz
match cru.mmc_set_clk(CCLK_EMMC, emmc_rate) {
    Ok(actual_rate) => println!("EMMC clock set to: {} Hz", actual_rate),
    Err(_) => println!("EMMC clock setting failed"),
}

// Enable NPU clock gate
match cru.npu_gate_enable(ACLK_NPU0) {
    Ok(enabled) => println!("NPU ACLK0 gate status: {}", enabled),
    Err(e) => println!("NPU gate enable failed: {}", e),
}
```

---

## 📁 Project Structure

```
src/
├── lib.rs              # Main entry and Rk3588Cru struct definition
├── autocs.rs           # Automatic clock selection functionality
├── clksel.rs           # Clock selection register definitions
├── constant.rs         # Hardware constants and clock ID definitions
├── gate.rs             # Clock gate control registers
├── pll.rs              # PLL phase-locked loop control registers
├── softrst.rs          # Software reset registers
└── tools.rs            # Utility functions (divider calculations, etc.)

tests/
└── test.rs             # Integration tests, including MMC and NPU functionality tests
```

---

## 📚 API Documentation

### Core Structures

- **`Rk3588Cru`**: Main CRU interface structure providing all clock control functions
- **`Rk3588CruRegisters`**: CRU register mapping structure containing all hardware register definitions

### Main Interfaces

#### MMC Clock Control

- `Rk3588Cru::new(addr)`: Create a new CRU instance
- `Rk3588Cru::init()`: Initialize CRU
- `Rk3588Cru::mmc_get_clk(clk_id)`: Get the current frequency of the specified MMC clock
- `Rk3588Cru::mmc_set_clk(clk_id, rate)`: Set the frequency of the specified MMC clock

**Supported MMC Clock IDs:**
- `CCLK_EMMC`: EMMC controller clock
- `CCLK_SRC_SDIO`: SDIO controller clock
- `SCLK_SFC`: SFC (SPI Flash Controller) clock
- `BCLK_EMMC`: EMMC bus clock

#### NPU Clock Management

- `Rk3588Cru::npu_get_clk(clk_id)`: Get NPU clock frequency
- `Rk3588Cru::npu_set_clk(clk_id, rate)`: Set NPU clock frequency
- `Rk3588Cru::npu_gate_enable(gate_id)`: Enable NPU clock gate
- `Rk3588Cru::npu_gate_disable(gate_id)`: Disable NPU clock gate
- `Rk3588Cru::npu_gate_status(gate_id)`: Query NPU clock gate status

**Supported NPU Clock IDs:**
- `HCLK_NPU_ROOT`: NPU root clock
- `CLK_NPU_DSU0`: NPU DSU0 clock
- `PCLK_NPU_ROOT`: NPU peripheral clock
- `CLK_NPUTIMER_ROOT`: NPU timer clock

**Supported NPU Gate IDs:**
- `ACLK_NPU0/1/2`: NPU module ACLK clocks
- `HCLK_NPU0/1/2`: NPU module HCLK clocks
- `PCLK_NPU_*`: NPU peripheral clocks
- `CLK_NPUTIMER*`: NPU timer clocks

---

## 💡 Usage Examples

### MMC Clock Control Example

```rust
use rk3588_clk::{Rk3588Cru, constant::*};
use core::ptr::NonNull;

fn configure_emmc_clock(cru: &Rk3588Cru) -> Result<(), &'static str> {

    // Set EMMC clock to 200MHz
    let target_rate = 200_000_000;
    match cru.mmc_set_clk(CCLK_EMMC, target_rate) {
        Ok(actual_rate) => {
            println!("EMMC clock set successfully: {} Hz", actual_rate);

            // Verify clock setting
            match cru.mmc_get_clk(CCLK_EMMC) {
                Ok(read_rate) => {
                    println!("EMMC clock read: {} Hz", read_rate);
                    if read_rate == actual_rate {
                        println!("Clock setting verification successful");
                    }
                }
                Err(e) => return Err("Clock read failed"),
            }
        }
        Err(e) => return Err("Clock setting failed"),
    }

    Ok(())
}
```

### NPU Clock Management Example

```rust
use rk3588_clk::{Rk3588Cru, constant::*};
use core::ptr::NonNull;

fn configure_npu_clocks(cru: &Rk3588Cru) -> Result<(), &'static str> {

    // Enable NPU-related clock gates
    let npu_gates = [
        ACLK_NPU0, HCLK_NPU0,
        ACLK_NPU1, HCLK_NPU1,
        ACLK_NPU2, HCLK_NPU2,
        PCLK_NPU_GRF, PCLK_NPU_TIMER,
    ];

    for &gate_id in &npu_gates {
        match cru.npu_gate_enable(gate_id) {
            Ok(enabled) => {
                println!("Gate {} enable status: {}", gate_id, enabled);
                if !enabled {
                    return Err("Gate enable failed");
                }
            }
            Err(e) => return Err("Gate operation failed"),
        }
    }

    // Set NPU root clock to 200MHz
    match cru.npu_set_clk(HCLK_NPU_ROOT, 200_000_000) {
        Ok(actual_rate) => {
            println!("NPU root clock set: {} Hz", actual_rate);
        }
        Err(e) => return Err("NPU clock setting failed"),
    }

    // Set NPU DSU0 clock to 500MHz
    match cru.npu_set_clk(CLK_NPU_DSU0, 500_000_000) {
        Ok(actual_rate) => {
            println!("NPU DSU0 clock set: {} Hz", actual_rate);
        }
        Err(e) => return Err("NPU DSU0 clock setting failed"),
    }

    println!("NPU clock configuration completed");
    Ok(())
}
```

### Complete Usage Example

```rust
use rk3588_clk::{Rk3588Cru, constant::*};
use core::ptr::NonNull;

fn main() -> Result<(), &'static str> {
    // Initialize CRU
    let cru_addr = 0xfd7c0000;
    let cru = Rk3588Cru::new(NonNull::new(cru_addr as *mut u8).unwrap());
    cru.init();

    // Configure storage clock
    println!("Configuring storage clock...");
    if let Err(e) = configure_emmc_clock(&cru) {
        println!("Storage clock configuration failed: {}", e);
        return Err(e);
    }

    // Configure NPU clock
    println!("Configuring NPU clock...");
    if let Err(e) = configure_npu_clocks(&cru) {
        println!("NPU clock configuration failed: {}", e);
        return Err(e);
    }

    // Run system clock diagnostics
    println!("System clock diagnostics:");
    if let Err(e) = clock_diagnostics(&cru) {
        println!("Clock diagnostics failed: {}", e);
        return Err(e);
    }

    Ok(())
}

fn clock_diagnostics(cru: &Rk3588Cru) -> Result<(), &'static str> {
    // Check critical clock status
    let critical_clocks = [
        (CCLK_EMMC, "EMMC"),
        (HCLK_NPU_ROOT, "NPU_ROOT"),
        (CLK_NPU_DSU0, "NPU_DSU0"),
    ];

    for &(clk_id, name) in &critical_clocks {
        match cru.npu_get_clk(clk_id) {
            Ok(rate) => println!("{} clock: {} Hz", name, rate),
            Err(_) => println!("{} clock read failed", name),
        }
    }

    Ok(())
}
```

---

## 🧪 Test Results

### Running Tests

#### Hardware Testing with U-Boot Environment

```bash
# Test with U-Boot enabled development board
cargo test --test test -- tests --show-output --uboot
```

### Test Output Example

<details>
<summary>Click to view test results</summary>

```
     _____                                         __
    / ___/ ____   ____ _ _____ _____ ___   ____ _ / /
    \__ \ / __ \ / __ `// ___// ___// _ \ / __ `// /
   ___/ // /_/ // /_/ // /   / /   /  __// /_/ // /
  /____// .___/ \__,_//_/   /_/    \___/ \__,_//_/
       /_/

Version                       : 0.12.2
Platfrom                      : RK3588 OPi 5 Plus
Start CPU                     : 0x0
FDT                7000
🐛 0.000ns    [sparreal_kernel::driver:16] add registers
🐛 0.000ns    [rdrive::probe::fdt:168] Probe [interrupt-controller@fe600000]->[GICv3]
🐛 0.000ns    [somehal::arch::mem::mmu:181] Map `iomap       `: RW- | [0xffff9000fe600000, 0xffff9000fe610000) -> [0xfe600000, 0xfe610000)
🐛 0.000ns    [somehal::arch::mem::mmu:181] Map `iomap       `: RW- | [0xffff9000fe680000, 0xffff9000fe780000) -> [0xfe680000, 0xfe780000)
🐛 0.000ns    [rdrive::probe::fdt:168] Probe [timer]->[ARMv8 Timer]
🐛 0.000ns    [sparreal_rt::arch::timer:78] ARMv8 Timer IRQ: IrqConfig { irq: 0x1e, trigger: LevelHigh, is_private: true }
🐛 0.000ns    [rdrive::probe::fdt:168] Probe [psci]->[ARM PSCI]
🐛 0.000ns    [sparreal_rt::arch::power:76] PCSI [Smc]
🐛 0.000ns    [sparreal_kernel::irq:39] [GICv3](405) open
🔍 0.000ns    [arm_gic_driver::version::v3:342] Initializing GICv3 Distributor@0xffff9000fe600000, security state: NonSecure...
🔍 0.000ns    [arm_gic_driver::version::v3:356] GICv3 Distributor disabled
🔍 0.000ns    [arm_gic_driver::version::v3:865] CPU interface initialization for CPU: 0x0
🔍 0.000ns    [arm_gic_driver::version::v3:921] CPU interface initialized successfully
🐛 0.000ns    [sparreal_kernel::irq:64] [GICv3](405) init cpu: CPUHardId(0)
🐛 0.000ns    [sparreal_rt::arch::timer:30] ARMv8 Timer: Enabled
🐛 17.977s    [sparreal_kernel::irq:136] Enable irq 0x1e on chip 405
🐛 17.977s    [sparreal_kernel::hal_al::run:33] Driver initialized
🐛 18.599s    [rdrive:132] probe pci devices
begin test
Run test: test_platform
💡 18.653s    [test::tests:338] Found node: mmc@fe2e0000
💡 18.654s    [test::tests:343] Syscon address range: 0xfe2e0000 - 0xfe2f0000
💡 18.655s    [test::tests:346] Aligned Syscon address range: 0xfe2e0000 - 0xfe2f0000
🐛 18.656s    [somehal::arch::mem::mmu:181] Map `iomap       `: RW- | [0xffff9000fe2e0000, 0xffff9000fe2f0000) -> [0xfe2e0000, 0xfe2f0000)
💡 18.685s    [test::tests:338] Found node: clock-controller@fd7c0000
💡 18.686s    [te000) -> [0xfd7c0000, 0xfd81c000)
💡 18.716s    [test::tests:338] Found node: syscon@fd5a2000
💡 18.717s    [test::tests:343] Syscon address range: 0xfd5a2000 - 0xfd5a2100
💡 18.718s    [test::tests:346] Aligned Syscon address range: 0xfd5a2000 - 0xfd5a3000
🐛 18.718s    [somehal::arch::mem::mmu:181] Map `iomap       `: RW- | [0xffff9000fd5a2000, 0xffff9000fd5a3000) -> [0xfd5a2000, 0xfd5a3000)
💡 18.752s    [test::tests:338] Found node: npu@fdab0000
💡 18.752s    [test::tests:3430m💡 18.753s    [test::tests:346] Aligned Sysc: 0xfdab0000 - 0xfdac0000
🐛 18.754s   ::arch::mem::mmu:181] Map `iomap       `: RW- | [0xffff9000fdab0xffff9000fdac0000) -> [0xfdab0000, 0xfdac0000)
💡 18.785s    [test::tests:338] Found node: power-management@fd8d8000
💡 18.785s    [tescon address range: 0xfd8d8000 -
💡 18.786   [test::tests:346] Aligned Sys range: 0xfd8d8000 - 0xfd8d9000
🐛 18.787s    [somehal::arch::mem::mmu:181] Map `iomap       `: RW- | [0xffff9000fd8d8000, 0xffff9000fd8d9000) -> [0xfd8d8000, 0xfd8d9000)
💡 18.788s    [test::tests:61] emmc ptr: 0xffff9000fe2e0000
💡 18.789s    [test::tests:62] clk ptr: 0xffff9000fd7c0000
💡 18.790s    [test::tests:63] npu grf ptr: 0xffff9000fd5a2000
💡 18.791s    [test::tests:64] npu ptr: 0xffff9000fdab0000
💡 18.791s    [test::tests:65] pmu ptr: 0xffff9000fd8d8000
💡 18.792s    [test::tests:73] emmc addr: 0xffff9000fe2e0000
💡 18.793s    [test::tests:74] clk addr: 0xffff9000fd7c0000
💡 18.793s    [test::tests:75] npu grf addr: 0xffff9000fd5a2000
💡 18.794s    [test::tests:76] npu addr: 0xffff9000fdab0000
💡 18.795s    [test::tests:77] pmu addr: 0xffff9000fd8d8000
💡 18.827s    [test::tests:296] Found node: npu@fdab0000
💡 18.828s    [test::tests:301] NPU0 address range: 0xfdab0000 - 0xfdac0000
💡 18.829s    [test::tests:304] Aligned NPU0 address range: 0xfdab0000 - 0xfdac0000
🐛 18.829s    [somehal::arch::mem::mmu:181] Map `iomap       `: RW- | [0xffff9000fdab0000, 0xffff9000fdac0000) -> [0xfdab0000, 0xfdac0000)
💡 18.863s    [test::tests:320] Found power domain node: power-controller
💡 18.897s    [test::tests:320] Found power domain node: power-controller
💡 18.930s    [test::tests:320] Found power domain node: power-controller
💡 18.931s   ts:278] NPU Version: 0x46495245
💡 18.932s    [sdmmc::emmc:74] EMMC Controller created: EMMC Controller { base_addr: 0xffff9000fe2e0000, card: None, caps: 0x226dc881, clock_base: 200000000 }
💡 18.933s    [sdmmc::emmc:91] Init EMMC Controller
🐛 18.934s    [sdmmc::emmc:100] Card inserted: true
💡 18.934s    [sdmmc::emmc:105] EMMC Version: 0x5
💡 18.935s    [sdmmc::emmc:108] EMMC Capabilities 1: 0b100010011011011100100010000001
💡 18.936s    [sdmmc::emmc:114] EMMC Capabilities 2: 0b1000000000000000000000000111
💡 18.937s    [sdmmc::emmc:162] voltage range: 0x60000, 0x12
💡 18.937s    [sdmmc::emmc::rockchip:145] EMMC Power Control: 0xd
🐛 18.948s    [sdmmc::emmc:974] Bus width set to 1
🐛 18.949s    [sdmmc::emmc::rockchip:318] card_clock: 0, bus_width: 1, timing: 0
💡 18.950s    [sdmmc::emmc::rockchip:163] EMMC Clock Control: 0x0
🐛 18.950s    [sdmmc::emmc::rockchip:106] EMMC Clock Control: 0x7
💡 18.951s    [sdmmc::emmc::rockchip:275] Clock 0x7
🐛 18.952s    [sdmmc::emmc::rockchip:353] EMMC Host Control 1: 0x0
🐛 18.953s    [sdmmc::emmc::rockchip:307] EMMC Host Control 2: 0x0
🐛 18.953s    [sdmmc::emmc::rockchip:318] card_clock: 400000, bus_width: 1, timing: 0
🐛 18.954s    [rk3588_clk:111] Setting clk_id 314 to rate 400000
🐛 18.955s    [rk3588_clk:152] CCLK_EMMC: src_clk 2, div 60, new_value 0xbb00, final_value 0xff00bb00
🐛 18.956s    [rk3588_clk:73] Getting clk_id 314
💡 18.957s    [sdmmc::emmc::rockchip:32] input_clk: 400000
💡 18.957s    [sdmmc::emmc::rockchip:42] EMMC Clock Mul: 0
💡 18.958s    [sdmmc::emmc::rockchip:78] EMMC Clock Divisor: 0x0
🐛 18.959s    [sdmmc::emmc::rockchip:106] EMMC Clock Control: 0x7
💡 18.959s    [sdmmc::emmc::rockchip:163] EMMC Clock Control: 0x2
🐛 18.960s    [sdmmc::emmc::rockchip:106] EMMC Clock Control: 0x7
💡 18.961s    [sdmmc::emmc::rockchip:275] Clock 0x7
🐛 18.961s    [sdmmc::emmc::rockchip:353] EMMC Host Control 1: 0x0
🐛 18.962s    [sdmmc::emmc::rockchip:307] EMMC Host Control 2: 0x0
🐛 18.963s    [sdmmc::emmc::rockchip:318] card_clock: 400000, bus_width: 1, timing: 0
🐛 18.964s    [rk3588_clk:111] Setting clk_id 314 to rate 400000
🐛 18.965s    [rk3588_clk:152] CCLK_EMMC: src_clk 2, div 60, new_value 0xbb00, final_value 0xff00bb00
🐛 18.966s    [rk3588_clk:73] Getting clk_id 314
💡 18.966s    [sdmmc::emmc::rockchip:32] input_clk: 400000
💡 18.967s    [sdmmc::emmc::rockchip:42] EMMC Clock Mul: 0
💡 18.968s    [sdmmc::emmc::rockchip:78] EMMC Clock Divisor: 0x0
🐛 18.968s    [sdmmc::emmc::rockchip:106] EMMC Clock Control: 0x7
💡 18.969s    [sdmmc::emmc::rockchip:163] EMMC Clock Control: 0x2
🐛 18.970s    [sdmmc::emmc::rockchip:106] EMMC Clock Control: 0x7
💡 18.971s    [sdmmc::emmc::rockchip:275] Clock 0x7
🐛 18.971s    [sdmmc::emmc::rockchip:353] EMMC Host Control 1: 0x0
🐛 18.972s    [sdmmc::emmc::rockchip:307] EMMC Host Control 2: 0x0
💡 18.973s    [sdmmc::emmc:226] eMMC initialization started
🔍 18.973s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x0, arg=0x0, resp_type=0x0, command=0x0
🔍 18.974s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 18.975s    [sdmmc::emmc::cmd:263] Response Status: 0b1
🔍 18.976s    [sdmmc::emmc::cmd:288] Command completed: status=0b1
💡 18.989s    [sdmmc::emmc::cmd:416] eMMC reset complete
🔍 18.989s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x1, arg=0x0, resp_type=0x1, command=0x102
🔍 18.990s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 18.991s    [sdmmc::emmc::cmd:263] Response Status: 0b1
🔍 18.992s    [sdmmc::emmc::cmd:288] Command completed: status=0b1
💡 19.005s    [sdmmc::emmc::cmd:431] eMMC first CMD1 response (no args): 0xff8080
🔍 19.005s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x1, arg=0x40060000, resp_type=0x1, command=0x102
🔍 19.007s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.007s    [sdmmc::emmc::cmd:263] Response Status: 0b1
🔍 19.008s    [sdmmc::emmc::cmd:288] Command completed: status=0b1
💡 19.011s    [sdmmc::emmc::cmd:453] CMD1 response raw: 0xff8080
💡 19.012s    [sdmmc::emmc::cmd:454] eMMC CMD1 response: 0xff8080
🔍 19.013s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x1, arg=0x40060000, resp_type=0x1, command=0x102
🔍 19.014s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.015s    [sdmmc::emmc::cmd:263] Response Status: 0b1
🔍 19.016s    [sdmmc::emmc::cmd:288] Command completed: status=0b1
💡 19.019s    [sdmmc::emmc::cmd:453] CMD1 response raw: 0xff8080
nse: 0xff8080
🔍 19.021s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x1, arg=0x40060000, resp_:cmd:263] Response Status: 0b0
🔍 19.023s    [sdmmc::emmc::cmd:263] Response Status: 0b1
🔍 19.024s    [sdmmc::emmc::cmd:288] Command completed: status=0b1
💡 19.026s    [sdmmc::emmc::cmd:453] CMD1 response raw: 0xff8080
💡 19.027s    [sdmmc::emmc::cmd:454] eMMC CMD1 response: 0xff8080
🔍 19.029s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x1, arg=0x40060000, resp_type=0x1, command=0x102
🔍 19.030s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.031s    [sdmmc::emmc::cmd:263] Response Status: 0b1
🔍 19.031s    [sdmmc::emmc::cmd:288] Command completed: status=0b1
💡 19.034s    [sdmmc::emmc::cmd:453] CMD1 response raw: 0xff8080
💡 19.035s    [sdmmc::emmc::cmd:454] eMMC CMD1 response: 0xff8080
🔍 19.037s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x1, arg=0x40060000, resp_type=0x1, command=0x102
🔍 19.038s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.039s    [sdmmc::emmc::cmd:263] Response Status: 0b1
🔍 19.039s    [sdmmc::emmc::cmd:288] Command completed: status=0b1
💡 19.042s    [sdmmc::emmc::cmd:453] CMD1 response raw: 0x40ff8080
💡 19.043s    [sdmmc::emmc::cmd:454] eMMC CMD1 response: 0x40ff8080
🔍 19.045s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x1, arg=0x40060000, resp_type=0x1, command=0x102
🔍 19.046s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.046s    [sdmmc::emmc::cmd:263] Response Status: 0b1
🔍 19.047s    [sdmmc::emmc::cmd:288] Command completed: status=0b1
💡 19.050s    [sdmmc::emmc::cmd:453] CMD1 response raw: 0xc0ff8080
💡 19.051s    [sdmmc::emmc::cmd:454] eMMC CMD1 response: 0xc0ff8080
💡 19.051s    [sdmmc::emmc::cmd:478] eMMC initialization status: true
🐛 19.053s    [sdmmc::emmc::cmd:486] Clock control beforand: opcode=0x2, arg=0x0, resp_type=0x7, command=0x209
🔍 19.055s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.056s    [sdmmc::emmc::cmd:263] Response Status: 0b1
🔍 19.057s    [sdmmc::emmc::cmd:288] Command completed: status=0b1
💡 19.059s    [sdmmc::emmc::cmd:69] eMMC response: 0x45010044 0x56343033 0x3201bb29 0x7a017c00
🔍 19.060s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x3, arg=0x10000, resp_type=0x15, command=0x31a
🔍 19.061s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.062s    [sdmmc::emmc::cmd:263] Response Status: 0b1
🔍 19.063s    [sdmmc::emmc::cmd:288] Command completed: status=0b1
🔍 19.066s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x9, arg=0x10000, resp_type=0x7, command=0x909
🔍 19.067s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.068s    [sdmmc::emmc::cmd:263] Response Status: 0b1
🔍 19.068s    [sdmmc::emmc::cmd:288] Command completed: status=0b1
💡 19.071s    [sdmmc::emmc::cmd:69] eMMC response: 0xd00f0032 0x8f5903ff 0xffffffef 0x8a404000
🐛 19.072s    [sdmmc::emmc:256] eMMC CSD version: 4
🔍 19.073s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x7, arg=0x10000, resp_type=0x15, command=0x71a
🔍 19.074s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.075s    [sdmmc::emmc::cmd:263] Response Status: 0b1
🔍 19.075s    [sdmmc::emmc::cmd:288] Command completed: status=0b1
🐛 19.078s    [sdmmc::emmc:327] cmd7: 0x700
🔍 19.079s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x6, arg=0x3b90100, resp_type=0x1d, command=0x61b
🔍 19.080s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.080s    [sdmmc::emmc::cmd:263] Response Status: 0b1
🔍 19.081s    [sdmmc::emmc::cmd:288] Command completed: status=0b1
🐛 19.084s    [sdmmc::emmc:1012] cmd6 0x800
🔍 19.084s    [sdmmc::emmc::cmd:244] Sending command: opcode=0xd, arg=0x10000, resp_type=0x15, command=0xd1a
🔍 19.086s    [sdmmc::emmc::cmd:263] Response Status: 0b0
8m🔍 19.086s    [sdmmc::emmc::cmd:263] Response Status: 0b1
🔍 19.087s    [sdmmc::emmc::cmd:288] Command completed: status=0b1
🔍 19.090s    [sdmmc::emmc::cmd:583] cmd_d 0x900
🐛 19.090s    [sdmmc::emmc::rockchip:318] card_clock: 400000, bus_width: 1, timing: 1
🐛 19.091s    [rk3588_clk:111] Setting clk_id 314 to rate 400000
🐛 19.092s    [rk3588_clk:152] CCLK_EMMC: src_clk 2, div 60, new_value 0xbb00, final_value 0xff00bb00
🐛 19.093s    [rk3588_clk:73] Getting clk_id 314
💡 19.094s    [sdmmc::emmc::rockchip:32] input_clk: 400000
💡 19.094s    [sdmmc::emmc::rockchip:42] EMMC Clock Mul: 0
💡 19.095s    [sdmmc::emmc::rockchip:78] EMMC Clock Divisor: 0x0
🐛 19.096s    [sdmmc::emmc::rockchip:106] EMMC Clock Control: 0x7
💡 19.097s    [sdmmc::emmc::rockchip:163] EMMC Clock Control: 0x2
🐛 19.097s    [sdmmc::emmc::rockchip:106] EMMC Clock Control: 0x7
💡 19.098s    [sdmmc::emmc::rockchip:275] Clock 0x7
🐛 19.099s    [sdmmc::emmc::rockchip:353] EMMC Host Control 1: 0x4
🐛 19.099s    [sdmmc::emmc::rockchip:307] EMMC Host Control 2: 0x2
🐛 19.100s    [sdmmc::emmc::rockchip:318] card_clock: 52000000, bus_width: 1, timing: 1
🐛 19.101s    [rk3588_clk:111] Setting clk_id 314 to rate 52000000
🐛 19.102s    [rk3588_clk:152] CCLK_EMMC: src_clk 1, div 23, new_value 0x5600, final0;188;18m💡 19.103s    [sdmmc::emmc::rockchip:32] input_clk: 65217391
💡 19.104s    [sdmmc::emmc::rockchip:42] EMMC Clock Mul: 0
💡 19.105s    [sdmmc::emmc::rockchip:78] EMMC Clock Divisor: 0x1
🐛 19.106s    [sdmmc::emmc::rockchip:106] EMMC Clock Control: 0x107
💡 19.106s    [sdmmc::emmc::rockchip:163] EMMC Clock Control: 0x2
🐛 19.107s    [sdmmc::emmc::rockchip:106] EMMC Clock Control: 0x7
💡 19.108s    [sdmmc::emmc::rockchip:275] Clock 0x7
🐛 19.108s    [sdmmc::emmc::rockchip:353] EMMC Host Control 1: 0x4
🐛 19.109s    [sdmmc::emmc::rockchip:307] EMMC Host Control 2: 0x2
🔍 19.110s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x8, arg=0x0, resp_type=0x15, command=0x83a
🔍 19.111s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.112s    [sdmmc::emmc::cmd:263] Response Status: 0b1
🔍 19.112s    [sdmmc::emmc::cmd:288] Command completed: status=0b1
🔍 19.113s    [sdmmc::emmc::cmd:339] Data transfer: cmd.data_present=true
🔍 19.114s    [sdmmc::emmc:354] EXT_CSD: [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8, 3, 0, 144, 23, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 128, 0, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 146, 4, 0, 7, 0, 0, 2, 0, 0, 21, 31, 128, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 0, 13, 0, 0, 0, 0, 8, 0, 2, 0, 87, 31, 10, 3, 221, 221, 0, 0, 0, 10, 10, 10, 10, 10, 10, 1, 0, 224, 163, 3, 23, 19, 23, 7, 7, 16, 1, 3, 1, 8, 32, 0, 7, 166, 166, 85, 3, 0, 0, 0, 0, 221, 221, 0, 1, 255, 0, 0, 0, 0, 1, 25, 25, 0, 16, 0, 0, 221, 82, 67, 51, 48, 66, 48, 48, 55, 81, 80, 8, 8, 8, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 31, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 16, 0, 3, 3, 0, 5, 3, 3, 1, 63, 63, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0]
🐛 19.128s    [sdmmc::emmc:412] Boot partition size: 0x400000
🐛 19.128s    [sdmmc::emmc:413] RPMB partition size: 0x1000000
🐛 19.129s    [sdmmc::emmc:434] GP partition sizes: [0, 0, 0, 0]
🔍 19.130s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x8, arg=0x0, resp_type=0x15, command=0x83a
🔍 19.131s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.132s    [sdmmc::emmc::cmd:263] Response Status: 0b100001
🔍 19.132s    [sdmmc::emmc::cmd:288] Command completed: status=0b100001
🔍 19.133s    [sdmmc::emmc::cmd:339] Data transfer: cmd.data_present=true
🔍 19.134s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x8, arg=0x0, resp_type=0x15, command=0x83a
🔍 19.135s    [sdmmc::emmc::cmd:263] Response0001
🔍 19.138s    [sdmmc::emmc::cmd:3type=0x1d, command=0x61b
🔍 19.140s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.140s    [sdmmc::emmc::cmd:263] Response Status: 0b11
🔍 19.141s    [sdmmc::emmc::cmd:288] Command completed: status=0b11
🐛 19.142s    [sdmmc::emmc:1012] cmd6 0x800
🔍 19.142s    [sdmmc::emmc::cmd:244] Sending command: opcode=0xd, arg=0x10000, resp_type=0x15, command=0xd1a
🔍 19.143s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.144s    [sdmmc::emmc::cmd:263] Response Status: 0b1
🔍 19.145s    [sdmmc::emmc::cmd:288] Command completed: status=0b1
🔍 19.146s    [sdmmc::emmc::cmd:583] cmd_d 0x900
🐛 19.146s    [sdmmc::emmc:974] Bus width set to 8
🐛 19.147s    [sdmmc::emmc::rockchip:318] card_clock: 52000000, bus_width: 8, timing: 1
🐛 19.148s    [rk3588_clk:111] Setting clk_id 314 to rate 52000000
🐛 19.149s    [rk3588_clk:152] CCLK_EMMC: src_clk 1, div 23, new_value 0x5600, final_value 0xff005600
🐛 19.150s    [rk3588_clk:73] Getting clk_id 314
💡 19.150s    [sdmmc::emmc::rockchip:32] input_clk: 65217391
💡 19.151s    [sdmmc::emmc::rockchip:42] EMMC Clock Mul: 0
💡 19.152s    [sdmmc::emmc::rockchip:78] EMMC Clock Divisor: 0x1
🐛 19.152s    [sdmmc::emmc::rockchip:106] EMMC Clock Control: 0x107
💡 19.153s    [sdmmc::emmc::rockchip:163] EMMC Clock Control: 0x2
🐛 19.154s    [sdmmc::emmc::rockchip:106] EMMC Clock Control: 0x7
💡 19.155s    [sdmmc::emmc::rockchip:275] Clock 0x7
🐛 19.155s    [sdmmc::emmc::rockchip:353] EMMC Host Control 1: 0x24
🐛 19.156s    [sdmmc::emmc::rockchip:307] EMMC Host Control 2: 0x2
🔍 19.157s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x8, arg=0x0, resp_type=0x15, command=0x83a
🔍 19.158s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.159s    [sdmmc::emmc::cmd:263] Response Status: 0b1
🔍 19.159s    [sdmmc::emmc::cmd:288] Command completed: status=0b1
🔍 19.160s    [sdmmc::emmc::cmd:339] Data transfer: cmd.data_present=true
🔍 19.161s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x6, arg=0x3b90200, resp_type=0x1d, command=0x61b
🔍 19.162s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.163s    [sdmmc::emmc::cmd:263] Response Status: 0b11
🔍 19.164s    [sdmmc::emmc::cmd:288] Command completed: status=0b11
🐛 19.164s    [sdmmc::emmc:1012] cmd6 0x800
🐛 19.166s    [sdmmc::emmc::rockchip:318] card_clock: 52000000, bus_width: 8, timing: 9
🐛 19.167s    [rk3588_clk:111] Setting clk_id 314 to rate 52000000
🐛 19.168s    [rk3588_clk:152] CCLK_EMMC: src_clk 1, div 23, new_value 0x5600, final_value 0xff005600
🐛 19.169s    [rk3588_clk:73] Getting clk_id 314
💡 19.169s    [sdmmc::emmc::rockchip:32] input_clk: 65217391
💡 19.170s    [sdmmc::emmc::rockchip:42] EMMC Clock Mul: 0
💡 19.171s    [sdmmc::emmc::rockchip:78] EMMC Clock Divisor: 0x1
🐛 19.171s    [sdmmc::emmc::rockchip:106] EMMC Clock Control: 0x107
💡 19.172s    [sdmmc::emmc::rockchip:163] EMMC Clock Control: 0x2
🐛 19.173s    [sdmmc::emmc::rockchip:106] EMMC Clock Control: 0x7
💡 19.173s    [sdmmc::emmc::rockchip:275] Clock 0x7
🐛 19.174s    [sdmmc::emmc::rockchip:353] EMMC Host Control 1: 0x24
💡 19.175s    [sdmmc::emmc::rockchip:145] EMMC Power Control: 0xb
🐛 19.186s    [sdmmc::emmc::rockchip:307] EMMC Host Control 2: 0x1b
🐛 19.186s    [sdmmc::emmc::rockchip:318] card_clock: 200000000, bus_width: 8, timing: 9
🐛 19.187s    [rk3588_clk:111] Setting clk_id 314 to rate 200000000
🐛 19.188s    [rk3588_clk:152] CCLK_EMMC: src_clk 1, div 6, new_value 0x4500, final_value 0xff004500
🐛 19.189s    [rk3588_clk:73] Getting clk_id 314
💡 19.190s    [sdmmc::emmc::rockchip:32] input_clk: 250000000
💡 19.190s    [sdmmc::emmc::rockchip:42] EMMC Clock Mul: 0
💡 19.191s    [sdmmc::emmc::rockchip:78] EMMC Clock Divisor: 0x1
🐛 19.192s    [sdmmc::emmc::rockchip:106] EMMC Clock Control: 0x107
💡 19.193s    [sdmmc::emmc::rockchip:163] EMMC Clock Control: 0x2
🐛 19.195s    [sdmmc::emmc::rockchip:106] EMMC Clock Control: 0x7
💡 19.196s    [sdmmc::emmc::rockchip:275] Clock 0x7
🐛 19.197s    [sdmmc::emmc::rockchip:353] EMMC Host Control 1: 0x24
💡 19.197s    [sdmmc::emmc::rockchip:145] EMMC Power Control: 0xb
🐛 19.208s    [sdmmc::emmc::rockchip:307] EMMC Host Control 2: 0x1b
🔍 19.209s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.210s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.211s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.212s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.212s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.213s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.214s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.215s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.216s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.216s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.218s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.218s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.219s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.220s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.221s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.222s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.222s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.223s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.224s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.225s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.226s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.227s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.228s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.229s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.229s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.230s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.231s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.232s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.232s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.234s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.235s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.235s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.235s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.237s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.239s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.239s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.240s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.240s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.241s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.242s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.243s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.244s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.245s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.246s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.246s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.247s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.252s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.252s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.253s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.254s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.255s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.256s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.257s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.257s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.258s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.259s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.260s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.261s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.262s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.263s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.263s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.264s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.265s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.266s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.267s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.268s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.269s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.269s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.270s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.271s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.272s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.273s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.274s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.274s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.276s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.276s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.277s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.278s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.279s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.280s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.280s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.281s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.282s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.283s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.284s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.285s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.286s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.287s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.287s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.288s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.289s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.290s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.291s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.291s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.293s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.293s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.294s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.295s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.296s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.297s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.297s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.298s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.299s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.299s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.300s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.301s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.303s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.304s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.304s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.305s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.306s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.307s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.307s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.302s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.299s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.299s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.300s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.301s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.303s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.304s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.304s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.305s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.306s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.307s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.307s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.308s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.310s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.310s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.311s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.312s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.313s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.314s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.315s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
🔍 19.315s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x15, arg=0x0, resp_type=0x15, command=0x153a
🔍 19.316s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.317s    [sdmmc::emmc::cmd:263] Response Status: 0b100000
🔍 19.318s    [sdmmc::emmc::cmd:288] Command completed: status=0b100000
💡 19.319s    [sdmmc::emmc:189] EMMC initialization completed successfully
SD card initialization successful!
Card type: MmcHc
Manufacturer ID: 0x45
Capacity: 0 MB
Block size: 512 bytes
Attempting to read first block...
🔍 19.321s    [sdmmc::emmc::block:365] pio read_blocks: block_id = 5034498, blocks = 1
🔍 19.322    [sdmmc::emmc::block:383] Reading 1 blocks starting at addres
🔍 19.323s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x11, arg=0x4cd202, resp_type=0x15, command=0x113a
🔍 19.324s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.325s    [sdmmc::emmc::cmd:263] Response Status: 0b100001
🔍 19.325s    [sdmmc::emmc::cmd:288] Command completed: status=0b100001
🔍 19.326s    [sdmmc::emmc::cmd:339] Data transfer: cmd.data_present=true
Successfully read first block!
First 16 bytes of first block: [40, E2, D0, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 8F, D2, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 40, DB, D0, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 80, E0, D0, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, C0, EC, D0, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 40, E9, D0, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 80, EE, D0, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, E4, D0, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, C0, DE, D0, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 40, F0, D0, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, DD, D0, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 80, E7, D0, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 40, A9, D5, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 40, 5B, D7, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 80, 50, D6, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 40, 4E, D6, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 60, 4F, D6, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 80, CE, CD, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 40, 48, DF, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 8E, D2, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 60, D6, CD, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 90, D2, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, A0, 09, DD, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 80, B9, E1, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, EB, D0, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 60, DD, E0, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 20, D1, CD, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, E0, 7E, E2, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 20, A8, D5, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 40, D7, CD, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 91, D2, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, C0, E5, D0, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00]
Testing write and read back...
🔍 19.345s    [sdmmc::emmc::block:417] pio write_blocks: block_id = 3, blocks = 1
🔍 19.346s    [sdmmc::emmc::block:439] Writing 1 blocks starting at address: 0x3
🔍 19.346s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x18, arg=0x3, resp_type=0x15, command=0x183a
🔍 19.348s    [sdmmc::emmc::cmd:263] Response Status: 0b10000
🔍 19.348s    [sdmmc::emmc::cmd:263] Response Status: 0b10001
🔍 19.349s    [sdmmc::emmc::cmd:288] Command completed: status=0b10001
🔍 19.350s    [sdmmc::emmc::cmd:339] Data transfer: cmd.data_present=true
Successfully wrote to block 3!
🔍 19.352s    [sdmmc::emmc::block:365] pio read_blocks: block_id = 3, blocks = 1
🔍 19.353s    [sdmmc::emmc::block:383] Reading 1 blocks starting at address: 0x3
🔍 19.354s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x11, arg=0x3, resp_type=0x15, command=0x113a
🔍 19.355s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.356s    [sdmmc::emmc::cmd:263] Response Status: 0b100001
🔍 19.356s    [sdmmc::emmc::cmd:288] Command completed: status=0b100001
🔍 19.357s    [sdmmc::emmc::cmd:339] Data transfer: cmd.data_present=true
Successfully read back block 3!
First 16 bytes of read block: [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
Data verification successful: written and read data match pe2;128;128;128m🔍 19.373s    [sdmmc::emmc::block:383] Reading 4 at address: 0xc8
🔍 19.374s    [sdmmc::emmc::block:383] Reading 4 blocks starting at address: 0xc8
🔍 19.375s    [sdmmc::emmc::cmd:244] Sending command: opcode=0x12, arg=0xc8, resp_type=0x15, command=0x123a
🔍 19.376s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.377s    [sdmmc::emmc::cmd:263] Response Status: 0b100001
🔍 19.377s    [sdmmc::emmc::cmd:288] Command completed: status=0b100001
🔍 19.378s    [sdmmc::emmc::cmd:339] Data transfer: cmd.data_present=true
🔍 19.379s    [sdmmc::emmc::cmd:244] Sending command: opcode=0xc, arg=0x0, resp_type=0x1d, command=0xc1b
🔍 19.380s    [sdmmc::emmc::cmd:263] Response Status: 0b0
🔍 19.381s    [sdmmc::emmc::cmd:263] Response Status: 0b11
🔍 19.382s    [sdmmc::emmc::cmd:288] Command completed: status=0b11
Successfully read 4 blocks starting at block address 200!
First 16 bytes of first block: [A0, 2F, 00, B9, A1, 8B, 0D, A9, A0, 07, 42, A9, A0, 07, 04, A9]
First 16 bytes of last block: [B5, 01, BD, 01, C6, 01, CE, 01, D6, 01, DE, 01, E7, 01, EF, 01]
SD card test complete
npu version: 0x8010
💡 19.384s    [test::tests:351] test npu cru
🐛 19.385s    [rk3588_clk:439] Enabling gate_id 301
🐛 19.385s    [rk3588_clk:578] Getting status for gate_id 301
💡 19.386s    [rk3588_clk:631] gate_con30 value: 0x0
🐛 19.387s    [rk3588_clk:669] Gate 301 is enabled
npu gate enable: true
🐛 19.387s    [rk3588_clk:439] Enabling gate_id 302
🐛 19.388s    [rk3588_clk:578] Getting status for gate_id 302
💡 19.389s    [rk3588_clk:636] gate_con30 value: 0x0
🐛 19.389s    [rk3588_clk:669] Gate 3 is enabled
npu gate enable: true
🐛 19.390s    [rk3588_clk:439] Enabling gate_id 290
🐛 19.391s    [rk3588_clk:578] Getting status for gate_id 290
💡 19.391s    [rk3588_clk:586] gate_con27 value: 0xaa04
🐛 19.392s    [rk3588_clk:669] Gate 290 is enabled
npu gate enable: true
🐛 19.393s    [rk3588_clk:439] Enabling gate_id 291
🐛 19.394s    [rk3588_clk:578] Getting status for gate_id 291
💡 19.394s    [rk3588_clk:591] gate_con27 value: 0xaa00
🐛 19.395s    [rk3588_clk:669] Gate 291 is enabled
npu gate enable: true
🐛 19.396s    [rk3588_clk:439] Enabling gate_id 292
🐛 19.396s    [rk3588_clk:578] Getting status for gate_id 292
💡 19.397s    [rk3588_clk:596] gate_con28 value: 0xa0
🐛 19.398s    [rk3588_clk:669] Gate 292 is enabled
npu gate enable: true
🐛 19.399s    [rk3588_clk:439] Enabling gate_id 293
🐛 19.399s    [rk3588_clk:578] Getting status for gate_id 293
💡 19.400s    [rk3588_clk:601] gate_con28 value: 0xa0
🐛 19.400s    [rk3588_clk:669] Gate 293 is enabled
npu gate enable: true
🐛 19.401s    [rk3588_clk:439] Enabling gate_id 298
🐛 19.402s    [rk3588_clk:578] Getting status for gate_id 298
💡 19.403s    [rk3588_clk:616] gate_con29 value: 0x263
🐛 19.403s    [rk3588_clk:669] Gate 298 is enabled
npu gate enable: true
npu version: 0x46495245
💡 19.404s    [test::tests:375] test npu cru end
💡 19.405s    [test::tests:88] test uboot
test test_platform passed
All tests passed
```

</details>

## 🤝 Contributing

Contributions are welcome! Please feel free to submit pull requests or open issues to report bugs and feature requests.

## 📄 License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.
