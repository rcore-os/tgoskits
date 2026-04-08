<h1 align="center">axaddrspace</h1>

<p align="center">ArceOS-Hypervisor 客户机虚拟机地址空间管理模块</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/axaddrspace.svg)](https://crates.io/crates/axaddrspace)
[![Docs.rs](https://docs.rs/axaddrspace/badge.svg)](https://docs.rs/axaddrspace)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/arceos-hypervisor/axaddrspace/blob/main/LICENSE)

</div>

[English](README.md) | 中文

# 简介

`axaddrspace` 是 [ArceOS-Hypervisor](https://github.com/arceos-hypervisor/)
项目中的客户机地址空间管理 crate，提供嵌套页表管理、客户机物理地址转换、内存映射后端以及嵌套页错误处理能力，面向 Hypervisor 场景使用。

该 crate 支持多种体系结构：

- **x86_64** - VMX Extended Page Tables（EPT）
- **AArch64** - Stage-2 页表
- **RISC-V** - 基于 Hypervisor 扩展的嵌套页表

核心能力包括：

- **`AddrSpace`** - 地址空间创建、映射、解除映射与地址转换
- **`AxMmHal`** - 用于页帧分配与地址转换的硬件抽象 trait
- **线性映射后端** - 映射已知的连续宿主物理内存区域
- **分配映射后端** - 支持预分配或缺页时惰性分配页帧
- **客户机内存辅助接口** - 将客户机地址转换为宿主可访问缓冲区

该库支持 `#![no_std]`，适用于裸机 Hypervisor 和内核环境。

## 快速开始

### 环境要求

- Rust nightly 工具链
- Rust 组件：`rust-src`、`clippy`、`rustfmt`

```bash
# 安装 rustup（如未安装）
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 安装 nightly 工具链和组件
rustup install nightly
rustup component add rust-src clippy rustfmt --toolchain nightly
```

### 运行检查和测试

```bash
# 1. 克隆仓库
git clone https://github.com/arceos-hypervisor/axaddrspace.git
cd axaddrspace

# 2. 代码检查
./scripts/check.sh

# 3. 运行测试
./scripts/test.sh

# 4. 直接运行指定集成测试目标
cargo test --test address_space
```

辅助脚本会在首次运行时自动下载共享的 `axci` 检查/测试框架。

## 集成方式

### 安装

在 `Cargo.toml` 中添加：

```toml
[dependencies]
axaddrspace = "0.3.0"
```

### 示例

```rust
use axaddrspace::{AddrSpace, AxMmHal, GuestPhysAddr, HostPhysAddr, HostVirtAddr, MappingFlags};
use memory_addr::{PhysAddr, VirtAddr};
use ax_page_table_multiarch::PagingHandler;

struct MyHal;

impl AxMmHal for MyHal {
    fn alloc_frame() -> Option<HostPhysAddr> {
        unimplemented!()
    }

    fn dealloc_frame(_paddr: HostPhysAddr) {
        unimplemented!()
    }

    fn phys_to_virt(_paddr: HostPhysAddr) -> HostVirtAddr {
        unimplemented!()
    }

    fn virt_to_phys(_vaddr: HostVirtAddr) -> HostPhysAddr {
        unimplemented!()
    }
}

impl PagingHandler for MyHal {
    fn alloc_frame() -> Option<PhysAddr> {
        <Self as AxMmHal>::alloc_frame()
    }

    fn dealloc_frame(paddr: PhysAddr) {
        <Self as AxMmHal>::dealloc_frame(paddr)
    }

    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
        <Self as AxMmHal>::phys_to_virt(paddr)
    }
}

fn example() -> ax_errno::AxResult<()> {
    let base = GuestPhysAddr::from_usize(0x1000_0000);
    let mut addr_space = AddrSpace::<MyHal>::new_empty(4, base, 0x20_0000)?;

    addr_space.map_linear(
        base,
        PhysAddr::from_usize(0x8000_0000),
        0x10_0000,
        MappingFlags::READ | MappingFlags::WRITE,
    )?;

    addr_space.map_alloc(
        base + 0x10_0000,
        0x2000,
        MappingFlags::READ | MappingFlags::WRITE,
        false,
    )?;

    let fault_handled = addr_space.handle_page_fault(
        base + 0x10_0000,
        MappingFlags::READ,
    );
    assert!(fault_handled);

    let host_paddr = addr_space.translate(base).unwrap();
    assert_eq!(host_paddr, PhysAddr::from_usize(0x8000_0000));

    Ok(())
}
```

### 特性

- `arm-el2`：启用 AArch64 EL2 支持
- `default`：默认包含 `arm-el2`

### 文档

生成并查看 API 文档：

```bash
cargo doc --no-deps --open
```

在线文档：[docs.rs/axaddrspace](https://docs.rs/axaddrspace)

# 贡献

1. Fork 仓库并创建分支
2. 本地运行检查：`./scripts/check.sh`
3. 本地运行测试：`./scripts/test.sh`
4. 提交 PR 并通过 CI 检查

# 许可证

本项目采用 Apache License 2.0。详见 [LICENSE](LICENSE)。
