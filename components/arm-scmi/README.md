# ARM SCMI Rust 实现 🦀

<div align="center">

[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-2024+-orange.svg)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/platform-ARM64-green.svg)](#)

*ARM System Control and Management Interface (SCMI) 协议的纯 Rust 实现*

</div>

## 📖 项目简介

ARM SCMI (System Control and Management Interface) 是一个用 Rust 编写的 ARM SCMI 协议实现库。该库专门为裸机和嵌入式环境设计，支持在 U-Boot 环境下运行，提供系统控制和管理功能的标准化接口。

本项目实现了 ARM SCMI 协议的核心功能，包括时钟管理、系统配置等，通过 SMC (Secure Monitor Call) 传输层与平台安全监控器进行通信。

### 核心优势

- 🔒 **安全优先**: 通过 SMC 调用与平台安全监控器安全通信
- ⚡ **高性能**: 高效的共享内存通信，支持大数据传输
- 🧠 **智能设计**: 基于 Future 的异步操作，响应迅速
- 📦 **零依赖**: 完全 `no_std` 兼容，适用于嵌入式环境
- 🛡️ **线程安全**: 内置并发支持，使用 Arc<Mutex<>>

## ✨ 功能特性

| 功能 | 描述 |
|------|------|
| 📘 **完整的 SCMI 支持** | ARM SCMI 规范的完整实现 |
| ⏱️ **时钟管理** | 时钟启用/禁用、频率设置/获取 |
| 🔐 **SMC 传输层** | Secure Monitor Call 通信 |
| 💾 **共享内存** | 高性能数据传输机制 |
| 🔄 **异步操作** | 基于 Future 的非阻塞操作 |
| 🚫 **no_std 兼容** | 可在裸机环境中运行 |
| 🏗️ **ARM64 优化** | 专为 64 位 ARM 架构量身定制 |

## 🚀 快速开始

### 环境要求

- Rust 2024 Edition
- ARM64 开发环境
- 支持 U-Boot 的硬件平台
- [ostool](https://crates.io/crates/ostool) 工具

### 安装步骤

1. 安装 `ostool` 依赖工具：
   ```bash
   cargo install ostool
   ```

2. 将项目添加到 `Cargo.toml`：
   ```toml
   [dependencies]
   arm-scmi = { git = "https://github.com/drivercraft/arm-scmi.git" }
   ```

### 基本使用

```rust
use arm_scmi::{Scmi, Smc, Shmem};

// 创建 SMC 传输层
let smc = Smc::new(0x84000000, None); // func_id, irq

// 初始化共享内存
let shmem = Shmem::new();

// 创建 SCMI 实例
let scmi = Scmi::new(smc, shmem);

// 获取时钟协议接口
let mut clock = scmi.protocol_clk();

// 启用时钟
clock.clk_enable(0)?;

// 设置时钟频率
clock.rate_set(0, 1000000)?;
```

## 📁 项目结构

```
src/
├── lib.rs              # 主入口和 Scmi 结构体
├── protocol/           # SCMI 协议实现
│   ├── mod.rs          # 通用协议框架和消息传输
│   └── clock.rs        # 时钟协议实现
├── transport/          # 传输层实现
│   ├── mod.rs          # 传输层 trait 定义
│   └── smc.rs          # SMC 传输实现
├── shmem.rs            # 共享内存管理
└── err.rs              # 错误处理
```

## 📚 API 文档

### 核心结构体

- **[`Scmi<T: Transport>`](src/lib.rs)**: 主要的 SCMI 接口结构体
- **[`Smc`](src/transport/smc.rs)**: SMC 传输层实现
- **[`Clock<T: Transport>`](src/protocol/clock.rs)**: 时钟协议接口
- **[`Shmem`](src/shmem.rs)**: 共享内存管理器

### 主要接口

| 方法 | 描述 |
|------|------|
| [`Scmi::new()`](src/lib.rs) | 创建新的 SCMI 实例 |
| [`Scmi::protocol_clk()`](src/lib.rs) | 获取时钟协议接口 |
| [`Clock::clk_enable()`](src/protocol/clock.rs) | 启用指定时钟 |
| [`Clock::clk_disable()`](src/protocol/clock.rs) | 禁用指定时钟 |
| [`Clock::rate_get()`](src/protocol/clock.rs) | 获取时钟频率 |
| [`Clock::rate_set()`](src/protocol/clock.rs) | 设置时钟频率 |

## 💡 使用示例

### 时钟管理示例

```rust
use arm_scmi::{Scmi, Smc, Shmem};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化 SCMI
    let smc = Smc::new(0x84000000, None);
    let shmem = Shmem::new();
    let scmi = Scmi::new(smc, shmem);

    // 获取时钟控制接口
    let mut clock = scmi.protocol_clk();

    // 启用时钟 0
    clock.clk_enable(0)?;
    println!("Clock 0 enabled");

    // 设置时钟频率为 1MHz
    clock.rate_set(0, 1_000_000)?;
    println!("Clock 0 frequency set to 1MHz");

    // 读取时钟频率
    let freq = clock.rate_get(0)?;
    println!("Clock 0 frequency: {} Hz", freq);

    Ok(())
}
```

## 🧪 测试结果

### 运行测试

#### 带U-Boot环境的硬件测试

```bash
# 带uboot的开发板测试
cargo test --test test -- tests --show-output --uboot
```

### 测试输出示例

<details>
<summary>点击查看测试结果</summary>

```
     _____                                         __
    / ___/ ____   ____ _ _____ _____ ___   ____ _ / /
    \__ \ / __ \ / __ `// ___// ___// _ \ / __ `// / 
   ___/ // / /   /  __// /_/ // /  
  /____// .___/ \__,_//_/   /_/    \___/ \__,_//_/   
       /_/                                           

Version                       : 0.12.2
Platfrom                      : RK3588 OPi 5 Plus
Start CPU                     : 0x0
FDT                           : 0xffff900000f1a000
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
🐛 17.339s    [sparreal_kernel::irq:136] Enable irq 0x1e on chip 405
🐛 17.340s    [sparreal_kernel::hal_al::run:33] Driver initialized
🐛 17.959s    [rdrive:132] probe pci devices
begin test
Run test: it_works
💡 17.978s    [test::tests:31] found scmi node: "scmi"
💡 18.003s    [test::tests:43] found shmem node: "sram@0"
🐛 18.004s    [somehal::arch::mem::mmu:181] Map `iomap       `: RW- | [0xffff90000010f000, 0xffff900000110000) -> [0x10f000, 0x110000)
💡 18.005s    [test::tests:58] shmem reg: <0x10f000(0x0), 0x100>
💡 18.006s    [test::tests:59] func_id: 0x82000010
🔍 18.006s    [arm_scmi::shmem:41] Reset SHMEM at 0xffff90000010f000
🔍 18.007s    [arm_scmi::protocol:75] Polling completion: xfer status=Init
🔍 18.008s    [arm_scmi::shmem:63] Preparing TX: hdr=MsgHeader { id: 0, protocol_id: 20, type_: Command, seq: 0, status: 0, poll_completion: false }, tx_len=0, all_len=4
🔍 18.009s    [arm_scmi::transport::smc:32] Sending SMC message MsgHeader { id: 0, protocol_id: 20, type_: Command, seq: 0, status: 0, poll_completion: false }
🔍 18.011s    [arm_scmi::protocol:75] Polling completion: xfer status=SendOk
🔍 18.012s    [arm_scmi::transport::smc:49] Fetched SMC response rx_len = 4, header: MsgHeader { id: 0, protocol_id: 20, type_: Command, seq: 0, status: 0, poll_completion: false }
🔍 18.014s    [arm_scmi::transport::smc:58] Fetched response: hdr=MsgHeader { id: 0, protocol_id: 20, type_: Command, seq: 0, status: 0, poll_completion: false }, rx_len=4, buff=[0, 0, 2, 0]
🔍 18.015s    [arm_scmi::protocol:75] Polling completion: xfer status=RespOk
🔍 18.016s    [arm_scmi::shmem:41] Reset SHMEM at 0xffff90000010f000
🐛 18.017s    [arm_scmi::protocol::clock:33] Clock Protocol version: 2.0
🔍 18.018s    [arm_scmi::protocol:75] Polling completion: xfer status=Init
🔍 18.019s    [arm_scmi::shmem:63] Preparing TX: hdr=MsgHeader { id: 1, protocol_id: 20, type_: Command, seq: 1, status: 0, poll_completion: false }, tx_len=0, all_len=4
🔍 18.020s    [arm_scmi::transport::smc:32] Sending SMC message MsgHeader { id: 1, protocol_id: 20, type_: Command, seq: 1, status: 0, poll_completion: false }
🔍 18.022s    [arm_scmi::protocol:75] Polling completion: xfer status=SendOk
🔍 18.023s    [arm_scmi::transport::smc:49] Fetched SMC response rx_len = 4, header: MsgHeader { id: 1, protocol_id: 20, type_: Command, seq: 1, status: 0, poll_completion: false }
🔍 18.024s    [arm_scmi::transport::smc:58] Fetched response: hdr=MsgHeader { id: 1, protocol_id: 20, poll_completion: false }, rx_len=4, buff=[40, 0, 1, 0]
🔍 18.026s    [arm_scmi::protocol:75] Polling completion: xfer status=RespOk
🔍 18.027s    [arm_scmi::shmem:41] Reset SHMEM at 0xffff90000010f000
🐛 18.028s    [arm_scmi::protocol::clock:50] Clock Protocol Attributes: num_clocks=40, max_async_req=1
🔍 18.029s    [arm_scmi::protocol:75] Polling completion: xfer status=Init
🔍 18.030s    [arm_scmi::shmem:63] Preparing TX: hdr=MsgHeader { id: 7, protocol_id: 20, type_: Command, seq: 2, status: 0, poll_completion: false }, tx_len=8, all_len=12
🔍 18.031s    [arm_scmi::transport::smc:32] Sending SMC message MsgHeader { id: 7, protocol_id: 20, type_: Command, seq: 2, status: 0, poll_completion: false }
🔍 18.033s    [arm_scmi::protocol:75] Polling completion: xfer status=SendOk
🔍 18.034s    [arm_scmi::transport::smc:49] Fetched SMC response rx_len = 0, header: MsgHeader { id: 7, protocol_id: 20, type_: Command, seq: 2, status: 0, poll_completion: false }
🔍 18.035s    [arm_scmi::transport::smc:58] Fetched response: hdr=MsgHeader { id: 7, protocol_id: 20, type_: Command, seq: 2, status: 0, poll_completion: false }, rx_len=0, buff=[]
🔍 18.037s    [arm_scmi::protocol:75] Polling completion: xfer status=RespOk
🔍 18.038s    [arm_scmi::shmem:41] Reset SHMEM at 0xffff90000010f000
🔍 18.039s    [arm_scmi::protocol:75] Polling completion: xfer status=Init
🔍 18.039s    [arm_scmi::shmem:63] Preparing TX: hdr=MsgHeader { id: 6, protocol_id: 20, type_: Command, seq: 3, status: 0, poll_completion: false }, tx_len=4, all_len=8
🔍 18.041s    [arm_scmi::transport::smc:32] Sending SMC message MsgHeader { id: 6, protocol_id: 20, type_: Command, seq: 3, status: 0, poll_completion: false }
🔍 18.043s    [arm_scmi::protocol:75] Polling completion: xfer status=SendOk
🔍 18.043s    [arm_scmi::transport::smc:49] Fetched SMC response rx_len = 8, header: MsgHeader { id: 6, protocol_id: 20, type_: Command, seq: 3, status: 0, poll_completion: false }
🔍 18.045s    [arm_scmi::transport::smc:58] Fetched response: hdr=MsgHeader { id: 6, protocol_id: 20, type_: Command, seq: 3, status: 0, poll_completion: false }, rx_len=8, buff=[0, 44, 163, 48, 0, 0, 0, 0]
🔍 18.047s    [arm_scmi::protocol:75] Polling completion: xfer status=RespOk
🔍 18.048s    [arm_scmi::shmem:41] Reset SHMEM at 0xffff90000010f000
Clock clk0 (id=0): rate=816000000 Hz
🔍 18.049s    [arm_scmi::protocol:75] Polling completion: xfer status=Init
🔍 18.050s    [arm_scmi::shmem:63] Preparing TX: hdr=MsgHeader { id: 5, protocol_id: 20, type_: Command, seq: 4, status: 0, poll_completion: false }, tx_len=16, all_len=20
🔍 18.051s    [arm_scmi::transport::smc:32] Sending SMC message MsgHeader { id: 5, protocol_id: 20, type_: Command, seq: 4, status: 0, poll_completion: false }
🔍 18.053s    [arm_scmi::protocol:75] Polling completion: xfer status=SendOk
🔍 18.054s    [arm_scmi::transport::smc:49] Fetched SMC response rx_len = 0, header: MsgHeader { id: 5, protocol_id: 20, type_: Command, seq: 4, status: 0, poll_completion: false }
🔍 18.056s    [arm_scmi::transport::smc:58] Fetched response: hdr=MsgHeader { id: 5, protocol_id: 20, type_: Command, seq: 4, status: 0, poll_completion: false }, rx_len=0, buff=[]
🔍 18.057s    [arm_scmi::protocol:75] Polling completion: xfer status=RespOk
🔍 18.058s    [arm_scmi::shmem:41] Reset SHMEM at 0xffff90000010f000
🔍 18.059s    [arm_scmi::protocol:75] Polling completion: xfer status=Init
🔍 18.060s    [arm_scmi::shmem:63] Preparing TX: hdr=MsgHeader { id: 6, protocol_id: 20, type_: Command, seq: 5, status: 0, poll_completion: false }, tx_len=4, all_len=8
🔍 18.061s    [arm_scmi::transport::smc:32] Sending SMC message MsgHeader { id: 6, protocol_id: 20, type_: Command, seq: 5, status: 0, poll_completion: false }
🔍 18.063s    [arm_scmi::protocol:75] Polling completion: xfer status=SendOk
🔍 18.064s    [arm_scmi::transport::smc:49] Fetched SMC response rx_len = 8, header: MsgHeader { id: 6, protocol_id: 20, type_: Command, seq: 5, status: 0, poll_completion: false }
🔍 18.065s    [arm_scmi::transport::smc:58] Fetched response: hdr=MsgHeader { id: 6, protocol_id: 20, type_: Command, seq: 5, status: 0, poll_completion: false }, rx_len=8, buff=[0, 44, 163, 48, 0, 0, 0, 0]
🔍 18.067s    [arm_scmi::protocol:75] Polling completion: xfer status=RespOk
🔍 18.068s    [arm_scmi::shmem:41] Reset SHMEM at 0xffff90000010f000
Clock clk0 (id=0): new rate=816000000 Hz
🔍 18.069s    [arm_scmi::protocol:75] Polling completion: xfer status=Init
🔍 18.070s    [arm_scmi::shmem:63] Preparing TX: hdr=MsgHeader { id: 7, protocol_id: 20, type_: Command, seq: 6, status: 0, poll_completion: false }, tx_len=8, all_len=12
🔍 18.072s    [arm_scmi::transport::smc:32] Sending SMC message MsgHeader { id: 7, protocol_id: 20, type_: Command, seq: 6, status: 0, poll_completion: false }
🔍 18.073s    [arm_scmi::protocol:75] Polling completion: xfer status=SendOk
🔍 18.074s    [arm_scmi::transport::smc:49] Fetched SMC response rx_len = 0, header: MsgHeader { id: 7, protocol_id: 20, type_: Command, seq: 6, status: 0, poll_completion: false }
🔍 18.076s    [arm_scmi::transport::smc:58] Fetched response: hdr=MsgHeader { id: 7, protocol_id: 20, type_: Command, seq: 6, status: 0, poll_completion: false }, rx_len=0, buff=[]
🔍 18.078s    [arm_scmi::protocol:75] Polling completion: xfer status=RespOk
🔍 18.078s    [arm_scmi::shmem:41] Reset SHMEM at 0xffff90000010f000
🔍 18.079s    [arm_scmi::protocol:75] Polling completion: xfer status=Init
🔍 18.080s    [arm_scmi::shmem:63] Preparing TX: hdr=MsgHeader { id: 6, protocol_id: 20, type_: Command, seq: 7, status: 0, poll_completion: false }, tx_len=4, all_len=8
🔍 18.082s    [arm_scmi::transport::smc:32] Sending SMC message MsgHeader { id: 6, protocol_id: 20, type_: Command, seq: 7, status: 0, poll_completion: false }
🔍 18.083s    [arm_scmi::protocol:75] Polling completion: xfer status=SendOk
🔍 18.084s    [arm_scmi::transport::smc:49] Fetched SMC response rx_len = 8, header: MsgHeader { id: 6, protocol_id: 20, type_: Command, seq: 7, status: 0, poll_completion: false }
🔍 18.086s    [arm_scmi::transport::smc:58] Fetched response: hdr=MsgHeader { id: 6, protocol_id: 20, type_: Command, seq: 7, status: 0, poll_completion: false }, rx_len=8, buff=[0, 44, 163, 48, 0, 0, 0, 0]
🔍 18.088s    [arm_scmi::protocol:75] Polling completion: xfer status=RespOk
🔍 18.088s    [arm_scmi::shmem:41] Reset SHMEM at 0xffff90000010f000
Clock clk1 (id=2): rate=816000000 Hz
🔍 18.090s    [arm_scmi::protocol:75] Polling completion: xfer status=Init
🔍 18.090s    [arm_scmi::shmem:63] Preparing TX: hdr=MsgHeader { id: 5, protocol_id: 20, type_: Command, seq: 8, status: 0, poll_completion: false }, tx_len=16, all_len=20
🔍 18.092s    [arm_scmi::transport::smc:32] Sending SMC message MsgHeader { id: 5, protocol_id: 20, type_: Command, seq: 8, status: 0, poll_completion: false }
🔍 18.094s    [arm_scmi::protocol:75] Polling completion: xfer status=SendOk
🔍 18.094s    [arm_scmi::transport::smc:49] Fetched SMC response rx_len = 0, header: MsgHeader { id: 5, protocol_id: 20, type_: Command, seq: 8, status: 0, poll_completion: false }
🔍 18.096s    [arm_scmi::transport::smc:58] Fetched response: hdr=MsgHeader { id: 5, protocol_id: 20, type_: Command, seq: 8, status: 0, poll_completion: false }, rx_len=0, buff=[]
🔍 18.098s    [arm_scmi::protocol:75] Polling completion: xfer status=RespOk
🔍 18.099s    [arm_scmi::shmem:41] Reset SHMEM at 0xffff90000010f000
🔍 18.100s    [arm_scmi::protocol:75] Polling completion: xfer status=Init
🔍 18.100s    [arm_scmi::shmem:63] Preparing TX: hdr=MsgHeader { id: 6, protocol_id: 20, type_: Command, seq: 9, status: 0, poll_completion: false }, tx_len=4, all_len=8
🔍 18.102s    [arm_scmi::transport::smc:32] Sending SMC message MsgHeader { id: 6, protocol_id: 20, type_: Command, seq: 9, status: 0, poll_completion: false }
🔍 18.103s    [arm_scmi::protocol:75] Polling completion: xfer status=SendOk
🔍 18.104s    [arm_scmi::transport::smc:49] Fetched SMC response rx_len = 8, header: MsgHeader { id: 6, protocol_id: 20, type_: Command, seq: 9, status: 0, poll_completion: false }
🔍 18.106s    [arm_scmi::transport::smc:58] Fetched response: hdr=MsgHeader { id: 6, protocol_id: 20, type_: Command, seq: 9, status: 0, poll_completion: false }, rx_len=8, buff=[0, 44, 163, 48, 0, 0, 0, 0]
🔍 18.108s    [arm_scmi::protocol:75] Polling completion: xfer status=RespOk
🔍 18.109s    [arm_scmi::shmem:41] Reset SHMEM at 0xffff90000010f000
Clock clk1 (id=2): new rate=816000000 Hz
🔍 18.110s    [arm_scmi::protocol:75] Polling completion: xfer status=Init
🔍 18.111s    [arm_scmi::shmem:63] Preparing TX: hdr=MsgHeader { id: 7, protocol_id: 20, type_: Command, seq: 10, status: 0, poll_completion: false }, tx_len=8, all_len=12
🔍 18.112s    [arm_scmi::transport::smc:32] Sending SMC message MsgHeader { id: 7, protocol_id: 20, type_: Command, seq: 10, status: 0, poll_completion: false }
🔍 18.114s    [arm_scmi::protocol:75] Polling completion: xfer status=SendOk
🔍 18.115s    [arm_scmi::transport::smc:49] Fetched SMC response rx_len = 0, header: MsgHeader { id: 7, protocol_id: 20, type_: Command, seq: 10, status: 0, poll_completion: false }
🔍 18.117s    [arm_scmi::transport::smc:58] Fetched response: hdr=MsgHeader { id: 7, protocol_id: 20, type_: Command, seq: 10, status: 0, poll_completion: false }, rx_len=0, buff=[]
🔍 18.118s    [arm_scmi::protocol:75] Polling completion: xfer status=RespOk
🔍 18.119s    [arm_scmi::shmem:41] Reset SHMEM at 0xffff90000010f000
🔍 18.120s    [arm_scmi::protocol:75] Polling completion: xfer status=Init
🔍 18.121s    [arm_scmi::shmem:63] Preparing TX: hdr=MsgHeader { id: 6, protocol_id: 20, type_: Command, seq: 11, status: 0, poll_completion: false }, tx_len=4, all_len=8
🔍 18.122s    [arm_scmi::transport::smc:32] Sending SMC message MsgHeader { id: 6, protocol_id: 20, type_: Command, seq: 11, status: 0, poll_completion: false }
🔍 18.124s    [arm_scmi::protocol:75] Polling completion: xfer status=SendOk
🔍 18.125s    [arm_scmi::transport::smc:49] Fetched SMC response rx_len = 8, header: MsgHeader { id: 6, protocol_id: 20, type_: Command, seq: 11, status: 0, poll_completion: false }
🔍 18.126s    [arm_scmi::transport::smc:58] Fetched response: hdr=MsgHeader { id: 6, protocol_id: 20, type_: Command, seq: 11, status: 0, poll_completion: false }, rx_len=8, buff=[0, 44, 163, 48, 0, 0, 0, 0]
🔍 18.128s    [arm_scmi::protocol:75] Polling completion: xfer status=RespOk
🔍 18.129s    [arm_scmi::shmem:41] Reset SHMEM at 0xffff90000010f000
Clock clk2 (id=3): rate=816000000 Hz
🔍 18.130s    [arm_scmi::protocol:75] Polling completion: xfer status=Init
🔍 18.131s    [arm_scmi::shmem:63] Preparing TX: hdr=MsgHeader { id: 5, protocol_id: 20, type_: Command, seq: 12, status: 0, poll_completion: false }, tx_len=16, all_len=20
🔍 18.133s    [arm_scmi::transport::smc:32] Sending SMC message MsgHeader { id: 5, protocol_id: 20, type_: Command, seq: 12, status: 0, poll_completion: false }
🔍 18.134s    [arm_scmi::protocol:75] Polling completion: xfer status=SendOk
🔍 18.135s    [arm_scmi::transport::smc:49] Fetched SMC response rx_len = 0, header: MsgHeader { id: 5, protocol_id: 20, type_: Command, seq: 12, status: 0, poll_completion: false }
🔍 18.137s    [arm_scmi::transport::smc:58] Fetched response: hdr=MsgHeader { id: 5, protocol_id: 20, type_: Command, seq: 12, status: 0, poll_completion: false }, rx_len=0, buff=[]
🔍 18.139s    [arm_scmi::protocol:75] Polling completion: xfer status=RespOk
🔍 18.139s    [arm_scmi::shmem:41] Reset SHMEM at 0xffff90000010f000
🔍 18.140s    [arm_scmi::protocol:75] Polling completion: xfer status=Init
🔍 18.141s    [arm_scmi::shmem:63] Preparing TX: hdr=MsgHeader { id: 6, protocol_id: 20, type_: Command, seq: 13, status: 0, poll_completion: false }, tx_len=4, all_len=8
🔍 18.143s    [arm_scmi::transport::smc:32] Sending SMC message MsgHeader { id: 6, protocol_id: 20, type_: Command, seq: 13, status: 0, poll_completion: false }
🔍 18.144s    [arm_scmi::protocol:75] Polling completion: xfer status=SendOk
🔍 18.145s    [arm_scmi::transport::smc:49] Fetched SMC response rx_len = 8, header: MsgHeader { id: 6, protocol_id: 20, type_: Command, seq: 13, status: 0, poll_completion: false }
🔍 18.147s    [arm_scmi::transport::smc:58] Fetched response: hdr=MsgHeader { id: 6, protocol_id: 20, type_: Command, seq: 13, status: 0, poll_completion: false }, rx_len=8, buff=[0, 44, 163, 48, 0, 0, 0, 0]
🔍 18.149s    [arm_scmi::protocol:75] Polling completion: xfer status=RespOk
🔍 18.150s    [arm_scmi::shmem:41] Reset SHMEM at 0xffff90000010f000
Clock clk2 (id=3): new rate=816000000 Hz
test passed!
test it_works passed
All tests passed
```

</details>

#### 测试功能说明

测试程序会执行以下操作：

1. **设备树解析**: 从设备树中查找 SCMI SMC 节点
2. **共享内存初始化**: 映射共享内存区域用于数据传输
3. **SMC 传输层配置**: 设置 SMC 函数 ID 和中断配置
4. **时钟协议测试**:
   - 启用多个时钟 (clk0, clk1, clk2)
   - 读取当前时钟频率
   - 设置新的时钟频率 (0x30a32c00 Hz)
   - 验证频率设置结果

**注意**: 完整测试需要支持 SCMI 的 ARM 硬件平台和 U-Boot 环境

## 🤝 贡献

欢迎贡献！请随时提交拉取请求或开启问题来报告错误和功能请求。

## 📄 许可证

该项目基于 MIT 许可证 - 详情请见 [LICENSE](LICENSE) 文件。