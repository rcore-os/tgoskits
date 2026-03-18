# aarch64_sysreg

[![Crates.io](https://img.shields.io/crates/v/aarch64_sysreg.svg)](https://crates.io/crates/aarch64_sysreg)
[![Docs.rs](https://docs.rs/aarch64_sysreg/badge.svg)](https://docs.rs/aarch64_sysreg)
[![License](https://img.shields.io/badge/license-GPL--3.0%2FApache--2.0%2FMulanPSL--2.0-blue.svg)](LICENSE)

AArch64 系统寄存器类型定义库，提供 ARM64 架构中操作类型、寄存器类型和系统寄存器的枚举定义。

## 特性

- `#![no_std]` - 可在裸机环境使用
- `OperationType` - AArch64 指令操作类型枚举
- `RegistersType` - 通用寄存器类型枚举 (W/X/V/B/H/S/D/Q 寄存器等)
- `SystemRegType` - 系统寄存器类型枚举

## 安装

在 `Cargo.toml` 中添加：

```toml
[dependencies]
aarch64_sysreg = "0.1"
```

## 使用示例

```rust
use aarch64_sysreg::{OperationType, RegistersType, SystemRegType};

fn main() {
    // 操作类型
    let op = OperationType::ADD;
    println!("Operation: {}", op);           // ADD
    println!("Value: 0x{:x}", op);           // 0x6

    // 从数值转换
    let op_from = OperationType::from(0x6);
    assert_eq!(op_from, OperationType::ADD);

    // 寄存器类型
    let reg = RegistersType::X0;
    println!("Register: {}", reg);           // X0

    // 系统寄存器
    let sys_reg = SystemRegType::MDSCR_EL1;
    println!("System Register: {}", sys_reg); // MDSCR_EL1
}
```

## 类型说明

### OperationType

定义 AArch64 指令操作类型，包括：
- 算术运算: `ADD`, `SUB`, `MUL`, `DIV` 等
- 逻辑运算: `AND`, `ORR`, `EOR`, `BIC` 等
- 分支指令: `B`, `BL`, `BR`, `RET` 等
- 加载存储: `LDR`, `STR`, `LDP`, `STP` 等
- 系统指令: `MSR`, `MRS`, `SVC`, `HVC` 等

### RegistersType

定义 AArch64 通用和向量寄存器：
- 32位通用寄存器: `W0` - `W30`, `WZR`, `WSP`
- 64位通用寄存器: `X0` - `X30`, `XZR`, `SP`
- 向量寄存器: `V0` - `V31`, `B0` - `B31`, `H0` - `H31`, `S0` - `S31`, `D0` - `D31`, `Q0` - `Q31`
- SVE 寄存器: `Z0` - `Z31`, `P0` - `P15`

### SystemRegType

定义 AArch64 系统寄存器，编号格式为 `<op0><op2><op1><CRn>00000<CRm>0`：
- 调试寄存器: `DBGBCR*_EL1`, `DBGBVR*_EL1` 等
- 跟踪寄存器: `TRC*` 系列
- 性能寄存器: `PMEVCNTR*_EL0`, `PMEVTYPER*_EL0` 等
- 系统控制寄存器: `SCTLR_EL1`, `TTBR*_EL1` 等

## 许可证

本项目采用以下许可证之一：
- GPL-3.0-or-later
- Apache-2.0
- MulanPSL-2.0

详见 [LICENSE](LICENSE) 文件。
