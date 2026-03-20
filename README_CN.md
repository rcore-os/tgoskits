<h1 align="center">riscv-h</h1>

<p align="center">RISC-V 虚拟化扩展寄存器支持库</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/riscv-h.svg)](https://crates.io/crates/riscv-h)
[![Docs.rs](https://docs.rs/riscv-h/badge.svg)](https://docs.rs/riscv-h)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/arceos-hypervisor/riscv-h/blob/main/LICENSE)

</div>

[English](README.md) | 中文

# 简介

RISC-V 虚拟化扩展寄存器支持库，提供 RISC-V Hypervisor Extension 中定义的控制和状态寄存器（CSR）的低级访问接口。支持 `#![no_std]`，可用于裸机和操作系统内核开发。

本库导出以下核心模块：

- **`register::hstatus`** — 虚拟化管理员状态寄存器
- **`register::hgatp`** — 虚拟化客户地址翻译和保护寄存器
- **`register::hvip`** — 虚拟化虚拟中断挂起寄存器
- **`register::vsstatus`** — 虚拟管理员状态寄存器
- **`register::vsatp`** — 虚拟管理员地址翻译和保护寄存器

所有寄存器类型均实现了 `Copy`、`Clone`、`Debug` trait，并提供类型安全的位字段访问方法。

## 快速上手

### 环境要求

- Rust nightly 工具链
- Rust 组件: rust-src, clippy, rustfmt, llvm-tools
- 目标平台: riscv64gc-unknown-none-elf

```bash
# 安装 rustup（如未安装）
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 安装 nightly 工具链及组件
rustup install nightly
rustup component add rust-src clippy rustfmt llvm-tools --toolchain nightly

# 添加 RISC-V 目标
rustup target add riscv64gc-unknown-none-elf --toolchain nightly
```

### 运行检查和测试

```bash
# 1. 克隆仓库
git clone https://github.com/arceos-hypervisor/riscv-h.git
cd riscv-h

# 2. 代码检查（格式检查 + clippy + 构建 + 文档生成）
./scripts/check.sh

# 3. 运行测试
# 运行全部测试（单元测试 + 集成测试）
./scripts/test.sh

# 仅运行单元测试
./scripts/test.sh unit

# 仅运行集成测试
./scripts/test.sh integration

# 列出所有可用的测试套件
./scripts/test.sh list

# 指定单元测试目标
./scripts/test.sh unit --unit-targets x86_64-unknown-linux-gnu
```

## 集成使用

### 安装

在 `Cargo.toml` 中添加：

```toml
[dependencies]
riscv-h = "0.2.0"
```

### 使用示例

```rust
#![no_std]

use riscv_h::register::{hstatus, hgatp, hvip};

fn main() {
    // 读取虚拟化管理员状态寄存器
    let hstatus = hstatus::read();
    
    // 检查是否处于虚拟化模式
    if hstatus.spv() {
        // 访问各个字段
        let vsxl = hstatus.vsxl();      // 虚拟管理员 XLEN
        let vtw = hstatus.vtw();        // 陷阱 WFI
        let vtsr = hstatus.vtsr();      // 陷阱 SRET
        let vgein = hstatus.vgein();    // 虚拟客户外部中断号
        
        setup_guest_translation();
    }
    
    // 配置虚拟中断挂起
    let mut hvip_val = hvip::Hvip::from_bits(0);
    hvip_val.set_vssip(true);  // 设置虚拟管理员软件中断挂起
    hvip_val.set_vstip(true);  // 设置虚拟管理员定时器中断挂起
    hvip_val.set_vseip(true);  // 设置虚拟管理员外部中断挂起
    
    unsafe {
        hvip_val.write();
    }
}

fn setup_guest_translation() {
    unsafe {
        // 配置客户地址翻译
        let mut hgatp = hgatp::Hgatp::from_bits(0);
        hgatp.set_mode(hgatp::HgatpValues::Sv48x4);  // 使用 Sv48x4 模式
        hgatp.set_vmid(1);                            // 设置 VMID
        hgatp.set_ppn(0x1000);                        // 设置根页表 PPN
        hgatp.write();
    }
}
```

### 异常和中断委托

```rust
use riscv_h::register::{hedeleg, hideleg};

unsafe {
    // 将常见异常委托给 VS-mode
    hedeleg::set_ex2(true);   // 非法指令
    hedeleg::set_ex8(true);   // U-mode 环境调用
    hedeleg::set_ex12(true);  // 指令页面错误
    hedeleg::set_ex13(true);  // 加载页面错误
    hedeleg::set_ex15(true);  // 存储页面错误
    
    // 将定时器和软件中断委托给 VS-mode
    hideleg::set_vstie(true);  // VS-mode 定时器中断
    hideleg::set_vssie(true);  // VS-mode 软件中断
}
```

## 支持的寄存器

### 虚拟化控制寄存器

| 寄存器 | 描述 | CSR 地址 |
|--------|------|----------|
| `hstatus` | 虚拟化管理员状态寄存器 | 0x600 |
| `hedeleg` | 虚拟化异常委托寄存器 | 0x602 |
| `hideleg` | 虚拟化中断委托寄存器 | 0x603 |
| `hie` | 虚拟化中断使能寄存器 | 0x604 |
| `hcounteren` | 虚拟化计数器使能寄存器 | 0x606 |
| `hgatp` | 虚拟化客户地址翻译和保护寄存器 | 0x680 |

### 虚拟管理员寄存器

| 寄存器 | 描述 | CSR 地址 |
|--------|------|----------|
| `vsstatus` | 虚拟管理员状态寄存器 | 0x200 |
| `vsie` | 虚拟管理员中断使能寄存器 | 0x204 |
| `vstvec` | 虚拟管理员陷阱向量寄存器 | 0x205 |
| `vsscratch` | 虚拟管理员暂存寄存器 | 0x240 |
| `vsepc` | 虚拟管理员异常程序计数器 | 0x241 |
| `vscause` | 虚拟管理员原因寄存器 | 0x242 |
| `vstval` | 虚拟管理员陷阱值寄存器 | 0x243 |
| `vsatp` | 虚拟管理员地址翻译和保护寄存器 | 0x280 |

### 其他寄存器

- **中断管理**: `hip`, `hvip`, `hgeie`, `hgeip`
- **时间管理**: `htimedelta`, `htimedeltah` 
- **陷阱信息**: `htval`, `htinst`
- **虚拟管理员中断**: `vsip`

### 文档

生成并查看 API 文档：

```bash
cargo doc --no-deps --open
```

在线文档：[docs.rs/riscv-h](https://docs.rs/riscv-h)

# 贡献

1. Fork 仓库并创建分支
2. 运行本地检查：`./scripts/check.sh`
3. 运行本地测试：`./scripts/test.sh`
4. 提交 PR 并通过 CI 检查

# 协议

本项目采用 Apache License, Version 2.0 许可证。详见 [LICENSE](LICENSE) 文件。
