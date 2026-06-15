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

### Raw 单对象 polling 使用

raw concrete 驱动直接持有寄存器和状态；读、写、poll、IRQ sync 都通过同一个对象完成。
someboot 等 allocator 初始化前路径直接保存这个对象，不需要 `Box`、rdif trait object 或内部锁。

```rust
use core::ptr::NonNull;
use some_serial::{
    ns16550::Ns16550, Config, DataBits, InterfaceRaw as _, Parity, SerialDirection, StopBits,
};

let base_addr = NonNull::new(0x9000000 as *mut u8).unwrap();
let mut uart = Ns16550::new_mmio(base_addr, 1_843_200, 1);

let config = Config::new()
    .baudrate(115200)
    .data_bits(DataBits::Eight)
    .stop_bits(StopBits::One)
    .parity(Parity::None);

uart.set_config(&config).expect("Failed to configure UART");
uart.open();
uart.enable_loopback();

let test_data = b"Hello, Serial!";
let mut sent = 0;
while sent < test_data.len() {
    let n = uart.try_write(&test_data[sent..]);
    if n == 0 {
        core::hint::spin_loop();
    }
    sent += n;
}
println!("Sent {} bytes", sent);

let mut buffer = [0u8; 64];
let received = if uart.pending(SerialDirection::Input) {
    uart.try_read(&mut buffer).expect("Failed to receive")
} else {
    0
};
println!("Received {} bytes: {:?}", received, &buffer[..received]);
```

### 驱动选择示例

根据硬件平台和访问方式选择合适的驱动：

```rust
use core::ptr::NonNull;

#[cfg(target_arch = "x86_64")]
let mut uart = some_serial::ns16550::Ns16550::new_port(0x3f8, 1_843_200);

#[cfg(target_arch = "aarch64")]
let mut uart = some_serial::pl011::Pl011::new(
    NonNull::new(0x9000000 as *mut u8).unwrap(),
    24_000_000,
);

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
let mut uart = some_serial::ns16550::Ns16550::new_mmio(
    NonNull::new(0x40000000 as *mut u8).unwrap(),
    16_000_000,
    1,
);
```

### 高级功能

#### 中断驱动通信

```rust
use some_serial::{InterfaceRaw as _, InterruptMask};
use some_serial::pl011::Pl011;

// 创建并配置 UART
let mut uart = Pl011::new(base_addr, clock_freq);
uart.set_config(&config).unwrap();
uart.open();

// 启用中断
uart.set_irq_mask(InterruptMask::RX_AVAILABLE | InterruptMask::TX_EMPTY);

// 在中断控制器回调中同步硬件 IRQ 状态
let event = uart.handle_irq();
if event.rx_ready() {
    // 运行时决定唤醒任务或继续轮询
}

// 数据搬运仍由任务态通过 try_read/try_write 推进
```

#### 平台检测与适配

需要运行时动态分发的 rdrive/Starry 路径可以把 concrete 设备包装成 `rdif_serial::BSerial`。
这个对象只负责控制和 split/restore；拆出的 TX/RX/IRQ runtime parts 各自持有可复制寄存器入口，
并通过共享原子状态同步 IRQ event 和 read-clear 错误位，不在 rdif adapter 内使用 Mutex。

```rust
use core::ptr::NonNull;
use rdif_serial::{BSerial, Interface as _, TTxQueue as _};

fn create_serial_for_runtime(base_addr: NonNull<u8>, clock_freq: u32) -> BSerial {
    some_serial::ns16550::Ns16550::new_mmio_boxed(base_addr, clock_freq, 1)
}

let mut serial = create_serial_for_runtime(
    NonNull::new(0x40000000 as *mut u8).unwrap(),
    16_000_000,
);

let mut tx = serial.take_tx().expect("missing TX queue");
let mut sent = 0;
let bytes = b"runtime serial\n";
while sent < bytes.len() {
    let n = tx.try_write(&bytes[sent..]);
    if n == 0 {
        core::hint::spin_loop();
    }
    sent += n;
}
```

#### 平台特定配置获取

```rust
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
// 查询当前控制配置
let current_baudrate = uart.baudrate();
let data_bits = uart.data_bits();
let stop_bits = uart.stop_bits();
let parity = uart.parity();

// 查询 I/O 就绪事件
let event = uart.poll();
let can_write = uart.pending(some_serial::SerialDirection::Output);
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
2. **实现 raw 接口**：驱动对象实现 `InterfaceRaw` 的配置、IRQ mask、`pending`、`poll`、`try_write`、`try_read`、`handle_irq`
3. **添加测试**：为新驱动编写完整的测试套件
4. **更新文档**：在 README 中添加驱动说明和使用示例
5. **提交 PR**：详细描述新驱动的功能和使用方法

### 参考实现

可以参考现有的 `src/pl011.rs` 作为新驱动的实现模板：

```rust
// 新驱动的基本结构示例
pub struct NewDriver {
    // 驱动寄存器句柄、时钟、saved status、IRQ mask shadow 等状态
}

impl InterfaceRaw for NewDriver {
    // 实现配置、开关、IRQ mask
    // 实现 pending/poll/try_write/try_read/handle_irq
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
