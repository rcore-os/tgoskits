<h1 align="center">axdevice</h1>

<p align="center">面向虚拟机的操作系统无关设备抽象层</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/axdevice.svg)](https://crates.io/crates/axdevice)
[![Docs.rs](https://docs.rs/axdevice/badge.svg)](https://docs.rs/axdevice)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/arceos-hypervisor/axdevice/blob/main/LICENSE)

</div>

[English](README.md) | 中文

# Introduction

`axdevice` 是一个可复用、与操作系统无关的虚拟机设备抽象层。它在 `#![no_std]` 环境中为模拟设备提供统一管理能力，并将访存请求分发到 MMIO、系统寄存器和端口类设备。

该 crate 当前导出两个核心类型：

- **`AxVmDeviceConfig`** - 封装用于初始化虚拟机设备的 `EmulatedDeviceConfig` 列表
- **`AxVmDevices`** - 管理设备集合、分发设备访问请求，并提供 IVC 通道分配能力

该 crate 适用于面向 AArch64 或 RISC-V 的 hypervisor 及底层操作系统组件。

## Quick Start

### Requirements

- Rust nightly 工具链
- Rust 组件：rust-src、clippy、rustfmt

```bash
# 安装 rustup（如果尚未安装）
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 安装 nightly 工具链与所需组件
rustup install nightly
rustup component add rust-src clippy rustfmt --toolchain nightly
```

### Run Check and Test

```bash
# 1. 进入仓库目录
cd axdevice

# 2. 代码检查（格式化 + clippy + 构建）
./scripts/check.sh

# 3. 运行测试
./scripts/test.sh
```

## Integration

### Installation

将以下依赖加入 `Cargo.toml`：

```toml
[dependencies]
axdevice = "0.2.2"
```

### Example

```rust
use std::sync::{Arc, Mutex};

use axaddrspace::device::AccessWidth;
use axaddrspace::{GuestPhysAddr, GuestPhysAddrRange};
use axdevice::{AxVmDeviceConfig, AxVmDevices};
use axdevice_base::BaseDeviceOps;
use axerrno::AxResult;
use axvmconfig::EmulatedDeviceType;

struct MockMmioDevice {
    range: GuestPhysAddrRange,
    last_write: Mutex<Option<usize>>,
}

impl MockMmioDevice {
    fn new(base: usize, size: usize) -> Self {
        Self {
            range: GuestPhysAddrRange::new(
                GuestPhysAddr::from(base),
                GuestPhysAddr::from(base + size),
            ),
            last_write: Mutex::new(None),
        }
    }
}

impl BaseDeviceOps<GuestPhysAddrRange> for MockMmioDevice {
    fn address_range(&self) -> GuestPhysAddrRange {
        self.range
    }

    fn emu_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::IVCChannel
    }

    fn handle_read(&self, _addr: GuestPhysAddr, _width: AccessWidth) -> AxResult<usize> {
        Ok(0xDEAD_BEEF)
    }

    fn handle_write(&self, addr: GuestPhysAddr, _width: AccessWidth, val: usize) -> AxResult {
        let offset = addr.as_usize() - self.range.start.as_usize();
        assert_eq!(offset, 0x40);
        *self.last_write.lock().unwrap() = Some(val);
        Ok(())
    }
}

fn main() {
    let config = AxVmDeviceConfig::new(vec![]);
    let mut devices = AxVmDevices::new(config);

    let mock = Arc::new(MockMmioDevice::new(0x1000_0000, 0x1000));
    devices.add_mmio_dev(mock.clone());

    let width = AccessWidth::try_from(4).unwrap();
    let addr = GuestPhysAddr::from(0x1000_0040);

    devices.handle_mmio_write(addr, width, 0x1234_5678).unwrap();
    let value = devices.handle_mmio_read(addr, width).unwrap();

    assert_eq!(value, 0xDEAD_BEEF);
    assert_eq!(*mock.last_write.lock().unwrap(), Some(0x1234_5678));
}
```

### Documentation

生成并查看 API 文档：

```bash
cargo doc --no-deps --open
```

在线文档： [docs.rs/axdevice](https://docs.rs/axdevice)

# Contributing

1. Fork 仓库并创建分支
2. 本地运行检查：`./scripts/check.sh`
3. 本地运行测试：`./scripts/test.sh`
4. 提交 PR 并通过 CI 检查

# License

本项目基于 Apache License 2.0 许可证发布。详见 [LICENSE](LICENSE)。
