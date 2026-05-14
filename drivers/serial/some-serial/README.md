# Some Serial - 嵌入式串口驱动集合

[![Crates.io](https://img.shields.io/crates/v/some-serial.svg)](https://crates.io/crates/some-serial)
[![Documentation](https://docs.rs/some-serial/badge.svg)](https://docs.rs/some-serial)
[![Test CI](https://github.com/drivercraft/some-serial/actions/workflows/test.yml/badge.svg)](https://github.com/drivercraft/some-serial/actions/workflows/test.yml)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE)

一个为嵌入式和裸机环境设计的 **统一串口驱动集合**，提供多种常见串口硬件的高性能、可靠驱动实现。

## 🎯 项目定位

`Some Serial` 旨在为嵌入式开发者提供统一的串口通信解决方案，支持多种硬件平台：

- 🔌 **统一接口** - 所有驱动使用相同的 API 接口
- 🚀 **高性能** - 针对裸机环境优化的零拷贝设计
- 🛡️ **内存安全** - 基于 Rust 类型系统的内存安全保证
- 🔧 **易于扩展** - 模块化设计，轻松添加新的驱动支持

## 🚀 核心特性

### 通用架构特性

- 🏗️ **统一抽象接口** - 基于 `rdif-serial` 的统一串口抽象
- 🛡️ **无标准库设计** (`no_std`) - 适用于裸机和嵌入式系统
- 📦 **模块化架构** - 每个驱动独立模块，按需选择
- 🔒 **类型安全** - 使用 Rust 类型系统确保内存安全
- 🧪 **全面测试** - 包含完整的测试套件，覆盖各种使用场景

### 驱动功能特性

- ⚡ **中断驱动** - 支持 TX/RX 中断，提供高效异步通信
- 📊 **FIFO 支持** - 硬件 FIFO 缓冲，可配置触发级别
- 🎛️ **灵活配置** - 支持波特率、数据位、停止位、奇偶校验配置
- 🔄 **回环测试** - 内置回环模式支持，便于测试和调试
- 📈 **性能优化** - 零拷贝数据传输，直接硬件访问

## 🔌 支持的驱动类型

### 当前支持

- ✅ **ARM PL011 UART** - ARM PrimeCell UART (PL011)
  - 广泛用于 ARM Cortex-A、Cortex-M、Cortex-R 系列
  - 支持 FIFO、中断、回环等完整功能
  - 适用于树莓派、STM32 等 ARM 平台

- ✅ **NS16550/16450 UART** - 经典串口控制器系列
  - **NS16550Mmio** - 内存映射 I/O 版本（通用嵌入式平台）
  - **NS16550Pio** - 端口 I/O 版本（x86_64 架构）
  - 支持 16 字节 FIFO 缓冲和中断驱动
  - 广泛兼容 PC 兼容串口设备和嵌入式系统

### 计划支持

- 🚧 **更多 ARM UART 驱动** - 扩展 ARM 平台支持
- 🚧 **RISC-V 平台适配** - 支持 RISC-V 嵌入式系统

## 🚀 快速开始

### 添加依赖

在你的 `Cargo.toml` 中添加：

```toml
[dependencies]
some-serial = "0.1.0"
```

### 通用接口使用

所有驱动都实现了统一的 `Serial` trait，提供一致的使用体验：

```rust
use core::ptr::NonNull;
use some_serial::{Serial, Config};

// 根据平台选择合适的驱动
#[cfg(target_arch = "aarch64")]
use some_serial::pl011::Pl011;

#[cfg(not(target_arch = "aarch64"))]
use some_serial::ns16550::Ns16550Mmio;

// 创建串口实例
let base_addr = 0x9000000 as *mut u8; // 你的 UART 基地址
let clock_freq = match target_arch {
    "aarch64" => 24_000_000, // ARM PL011: 24MHz
    _ => 1_843_200,          // NS16550: 1.8432MHz
};

let mut uart = match target_arch {
    "aarch64" => Pl011::new(
        NonNull::new(base_addr).unwrap(),
        clock_freq
    ),
    _ => Ns16550Mmio::new(
        NonNull::new(base_addr).unwrap(),
        clock_freq
    ),
};

// 统一配置接口
let config = Config::new()
    .baudrate(115200)
    .data_bits(some_serial::DataBits::Eight)
    .stop_bits(some_serial::StopBits::One)
    .parity(some_serial::Parity::None);

uart.set_config(&config).expect("Failed to configure UART");
uart.open().expect("Failed to open UART");

// 启用回环模式进行测试（如果支持）
uart.enable_loopback();

// 获取 TX/RX 接口进行数据传输
let mut tx = uart.take_tx().unwrap();
let mut rx = uart.take_rx().unwrap();

// 发送和接收数据
let test_data = b"Hello, Serial!";
let sent = tx.send(test_data);
println!("Sent {} bytes", sent);

let mut buffer = [0u8; 64];
let received = rx.receive(&mut buffer).expect("Failed to receive");
println!("Received {} bytes: {:?}", received, &buffer[..received]);
```

### 驱动选择示例

根据硬件平台和访问方式选择合适的驱动：

```rust
// ARM 平台 - 使用 PL011
#[cfg(target_arch = "aarch64")]
use some_serial::pl011::Pl011;

// x86_64 平台 - 使用端口 I/O
#[cfg(target_arch = "x86_64")]
use some_serial::ns16550::Ns16550Pio;

// 其他嵌入式平台 - 使用内存映射 I/O
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
use some_serial::ns16550::Ns16550Mmio;

// 平台特定的创建函数
fn create_uart_for_platform(base_addr: *mut u8, clock_freq: u32) -> Box<dyn Serial> {
    #[cfg(target_arch = "aarch64")]
    {
        Box::new(Pl011::new(
            NonNull::new(base_addr).unwrap(),
            clock_freq
        ))
    }

    #[cfg(target_arch = "x86_64")]
    {
        Box::new(Ns16550Pio::new(
            NonNull::new(base_addr).unwrap(),
            clock_freq
        ))
    }

    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    {
        Box::new(Ns16550Mmio::new(
            NonNull::new(base_addr).unwrap(),
            clock_freq
        ))
    }
}

// 统一的创建和使用方式
let mut uart = create_uart_for_platform(base_addr, clock_freq);
// ... 后续使用方式完全相同
```

### 高级功能

#### 中断驱动通信

```rust
use some_serial::{Serial, InterruptMask};
use some_serial::pl011::Pl011;

// 创建并配置 UART
let mut uart = Pl011::new(base_addr, clock_freq);
uart.set_config(&config).unwrap();
uart.open().unwrap();

// 启用中断
uart.enable_interrupts(InterruptMask::RX_AVAILABLE | InterruptMask::TX_EMPTY);

// 注册中断处理程序
let irq_handler = uart.irq_handler().unwrap();
// 在你的中断控制器中注册 irq_handler...

// 现在可以在中断处理中高效处理数据传输
```

#### 平台检测与适配

```rust
// 运行时平台检测示例
fn create_serial_for_platform(base_addr: *mut u8, clock_freq: u32) -> Box<dyn Serial> {
    #[cfg(target_arch = "aarch64")]
    {
        // ARM64 平台，使用 PL011
        Box::new(Pl011::new(
            NonNull::new(base_addr).unwrap(),
            clock_freq
        ))
    }

    #[cfg(target_arch = "x86_64")]
    {
        // x86_64 平台，使用 NS16550 端口 I/O
        Box::new(Ns16550Pio::new(
            NonNull::new(base_addr).unwrap(),
            clock_freq
        ))
    }

    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    {
        // 其他嵌入式平台，使用 NS16550 内存映射 I/O
        Box::new(Ns16550Mmio::new(
            NonNull::new(base_addr).unwrap(),
            clock_freq
        ))
    }
}

// 系统集成示例
fn init_system_uart() -> Result<Box<dyn Serial>, &'static str> {
    let (base_addr, clock_freq) = get_platform_uart_config()?;

    let mut uart = create_serial_for_platform(base_addr, clock_freq);

    // 标准配置
    let config = Config::new()
        .baudrate(115200)
        .data_bits(DataBits::Eight)
        .stop_bits(StopBits::One)
        .parity(Parity::None);

    uart.set_config(&config).map_err(|_| "Failed to configure UART")?;
    uart.open().map_err(|_| "Failed to open UART")?;

    Ok(uart)
}

// 平台特定配置获取
fn get_platform_uart_config() -> Result<(*mut u8, u32), &'static str> {
    #[cfg(target_arch = "aarch64")]
    {
        // ARM 平台常见配置
        Ok((0x9000000 as *mut u8, 24_000_000))
    }

    #[cfg(target_arch = "x86_64")]
    {
        // x86 平台常见配置
        Ok((0x3F8 as *mut u8, 1_843_200))
    }

    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    {
        // 默认嵌入式配置
        Ok((0x40000000 as *mut u8, 16_000_000))
    }
}
```

## API 文档

### 配置选项

```rust
use some_serial::{Config, DataBits, StopBits, Parity};

let config = Config::new()
    .baudrate(115200)           // 波特率
    .data_bits(DataBits::Eight) // 数据位：5/6/7/8
    .stop_bits(StopBits::One)   // 停止位：1/2
    .parity(Parity::None);      // 校验位：None/Odd/Even/Mark/Space
```

### 状态查询

```rust
// 查询线路状态
let status = uart.line_status();
if status.contains(some_serial::LineStatus::DATA_READY) {
    // 有数据可读
}

if status.contains(some_serial::LineStatus::TX_HOLDING_EMPTY) {
    // 可以发送数据
}

// 查询当前配置
let current_baudrate = uart.baudrate();
let data_bits = uart.data_bits();
let stop_bits = uart.stop_bits();
let parity = uart.parity();
```

## 测试

这个库包含了一个全面的测试套件，使用 `bare-test` 框架在裸机环境中运行。

### 运行测试

```bash
# 安装 ostool 用于裸机测试
cargo install ostool

# 运行测试
cargo test --test test --  --show-output
# 真机测试
cargo test --test test --  --show-output --uboot
```

### 测试覆盖

- **基础回环测试** - 验证基本的发送/接收功能
- **资源管理测试** - 验证 RAII 和资源生命周期
- **配置测试** - 验证各种配置选项
- **中断测试** - 验证中断功能和掩码控制
- **压力测试** - 高频数据传输测试
- **多模式测试** - 不同数据模式的测试

## 性能特性

- **低延迟** - 直接硬件寄存器访问
- **高吞吐量** - FIFO 支持提高传输效率
- **内存效率** - 零拷贝数据传输
- **中断优化** - 最小化中断处理开销

## 许可证

本项目采用以下许可证：

- [MIT License](LICENSE-MIT)
- [Apache License 2.0](LICENSE-APACHE)

你可以选择其中任何一个许可证使用本项目。

## 🤝 贡献指南

我们欢迎社区贡献！以下是贡献方式：

### 添加新驱动支持

1. **创建驱动模块**：在 `src/` 目录下创建新的驱动文件
2. **实现 Serial trait**：确保实现统一的 `rdif-serial` 接口
3. **添加测试**：为新驱动编写完整的测试套件
4. **更新文档**：在 README 中添加驱动说明和使用示例
5. **提交 PR**：详细描述新驱动的功能和使用方法

### 参考实现

可以参考现有的 `src/pl011.rs` 作为新驱动的实现模板：

```rust
// 新驱动的基本结构示例
pub struct NewDriver {
    // 驱动特定的状态
}

impl Serial for NewDriver {
    // 实现 Serial trait 的所有方法
}

impl NewDriver {
    // 驱动特定的初始化和配置方法
}
```

## 📚 相关资源

### 技术文档

- [ARM PL011 Technical Reference Manual](https://developer.arm.com/documentation/ddi0183/g/) - PL011 硬件规格
- [rdif-serial](https://github.com/rdif-rs/rdif-serial) - 统一串口接口抽象
- [bare-test](https://github.com/bare-test/bare-test) - 裸机测试框架

### 硬件参考

- [16550/16450 UART 数据手册](https://www.lammertbies.nl/comm/info/serial-uart.html) - 经典串口控制器

## 致谢

感谢所有为嵌入式串口通信生态系统做出贡献的开发者和项目！

## 更新日志

### v0.1.0 (2024-01-XX)

- ✨ 初始发布 - 嵌入式串口驱动集合
- ✅ 完整的 ARM PL011 UART 支持
- ✅ **新增 NS16550/16450 UART 驱动支持**
  - ✅ NS16550Mmio - 内存映射 I/O 版本
  - ✅ NS16550Pio - 端口 I/O 版本（x86_64）
  - ✅ 支持 FIFO、中断、回环等完整功能
- ✅ 基于 rdif-serial 的统一接口抽象
- ✅ 中断驱动通信和 FIFO 功能
- ✅ 全面测试套件和文档
- ✅ **性能优化和类型安全改进**
- 🏗️ 模块化架构，支持多平台驱动选择

### 未来计划

- 🎯 扩展更多 ARM UART 驱动支持
- 🎯 RISC-V 平台适配
- 🎯 更多性能优化和功能特性

---
