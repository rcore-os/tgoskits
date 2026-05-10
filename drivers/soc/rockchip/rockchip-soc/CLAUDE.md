# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 项目概述

Rockchip SoC 的 Rust `no_std` 实现，提供时钟复位单元 (CRU) 和引脚控制 (PINCTRL) 驱动。当前主要支持 RK3588 芯片。

**核心设计原则**: SOLID、KISS、DRY、YAGNI

**架构特点**:
- 使用 `enum_dispatch` 实现零成本抽象的 trait 分发
- 通用层与变体层分离，便于扩展新芯片支持
- 类型安全的时钟 ID 和复位 ID 系统

## 构建和测试

### 环境要求

- Rust nightly toolchain (见 `rust-toolchain.toml`)
- 目标架构: `aarch64-unknown-none-softfloat`
- 集成测试需要: `ostool` (用于裸机测试运行)

### 常用命令

```bash
# 库检查和单元测试
cargo check --test test --target aarch64-unknown-none-softfloat
cargo test --lib

# 运行特定模块测试
cargo test --lib pll
cargo test --lib i2c
cargo test --lib uart

# 代码格式化
cargo fmt --all

# 集成测试 (需要 ostool 和硬件)
cargo install ostool
cargo test --test test --target aarch64-unknown-none-softfloat -- uboot
```

## 参考资料

- **RK3588 TRM**: `/home/zhourui/opensource/proj_usb/CrabUSB2/.spec-workflow/Rockchip_RK3588_TRM_V1.0-Part1.md`
- **设备树**: `/home/zhourui/opensource/proj_usb/u-boot-orangepi/orangepi5plus.dts`
- **u-boot 源码**: `/home/zhourui/opensource/proj_usb/u-boot-orangepi` (参考: `drivers/clk/rockchip/clk_rk3588.c`)
- **Linux 内核**: `/home/zhourui/orangepi-build/kernel/orange-pi-6.1-rk35xx` (参考: `drivers/clk/rockchip/clk-rk3588.c`)

## 开发规范

**提交前检查**:
1. `cargo check --test test --target aarch64-unknown-none-softfloat` 必须通过
2. `cargo fmt --all` 保持代码风格一致
3. 使用最新依赖库，通过 context7 查询 API
4. 使用 `tock-registers` 进行寄存器操作

## 核心架构

### 分层设计

**1. Trait 抽象层** (`src/clock/mod.rs`)
```rust
#[enum_dispatch::enum_dispatch]
pub trait CruOp {
    fn clk_enable(&mut self, id: ClkId) -> ClockResult<()>;
    fn clk_disable(&mut self, id: ClkId) -> ClockResult<()>;
    fn clk_get_rate(&self, id: ClkId) -> ClockResult<u64>;
    fn clk_set_rate(&mut self, id: ClkId, rate_hz: u64) -> ClockResult<u64>;
    fn reset_assert(&mut self, id: RstId);
    fn reset_deassert(&mut self, id: RstId);
}

#[enum_dispatch::enum_dispatch(CruOp)]
pub enum Cru {
    Rk3588(crate::variants::rk3588::cru::Cru),
}
```

**2. 通用层** (`src/`)
- `clock/`: 时钟 ID、错误类型、trait 定义
- `rst/`: 复位控制 (RstId, ResetRockchip)
- `pinctrl/`: 引脚控制 (PinCtrl, GPIO)

**3. 变体层** (`src/variants/`)
- `rk3588/cru/`: CRU 具体实现
- `rk3588/pinctrl/`: PINCTRL 具体实现

### 模块组织

```
src/
├── clock/
│   ├── mod.rs          # CruOp trait, ClkId, 错误类型
│   └── pll.rs          # 通用 PLL 类型
├── rst.rs              # RstId, ResetRockchip
├── pinctrl/            # PinCtrl (独立模块)
└── variants/
    ├── mod.rs          # 变体入口，导出时钟 ID 常量
    └── rk3588/
        ├── cru/
        │   ├── mod.rs          # Cru 实现 + CruOp trait
        │   ├── pll.rs          # PLL 配置和计算
        │   ├── consts.rs       # 寄存器偏移
        │   ├── gate.rs         # 时钟门控表
        │   ├── peripheral.rs   # 外设时钟 (I2C/UART/SPI/MMC)
        │   └── clock/mod.rs    # 时钟 ID 常量
        └── pinctrl/            # 引脚控制实现
```

### 关键设计模式

**1. 时钟 ID 系统**
- `ClkId(u64)`: 新类型包装，类型安全
- 时钟 ID 映射到设备树绑定 (`rk3588-cru.h`)
- 常量定义在 `src/variants/rk3588/cru/clock/mod.rs`
- 支持范围检查 (`RangeBounds` trait)

**2. PLL 管理**
- 频率通过查找表 (`rate_table`) 配置
- 支持整数和小数分频 (K 参数)
- 计算公式: `rate = ((fin / p) * m + (fin * k) / (p * 65536)) >> s`
- 9 个 PLL: B0PLL, B1PLL, LPLL, CPLL, GPLL, NPLL, V0PLL, AUPLL, PPLL

**3. Rockchip 寄存器写掩码**
- 高 16 位: 要清除的位掩码
- 低 16 位: 要设置的值
- 方法: `clrsetreg()`, `clrreg()`, `setreg()`

**4. 错误处理**
- 统一定义在 `src/clock/error.rs`
- `ClockError`: 不支持时钟、无效频率、读写失败等
- `ClockResult<T>`: 类型别名

**5. 时钟门控机制**
- 门控表定义在 `gate.rs`
- 包含: 寄存器偏移、位位置、时钟类型
- 查找流程: `find_clk_gate()` → `get_gate_reg_offset()` → 寄存器操作

### API 使用示例

```rust
use rockchip_soc::{Cru, CruOp, SocType};

// 创建 CRU 实例 (自动初始化)
let cru = Cru::new(SocType::Rk3588, base_addr, sys_grf_addr);

// 时钟操作
cru.clk_enable(CLK_I2C1)?;
let rate = cru.clk_get_rate(CLK_I2C1)?;
cru.clk_set_rate(CLK_I2C1, 100_000_000)?;

// 复位控制
cru.reset_assert(RstId::new(100));
cru.reset_deassert(RstId::new(100));
```

### RK3588 CRU 实现

**Cru 结构体** (`src/variants/rk3588/cru/mod.rs`):
- 实现 `CruOp` trait
- `new()`: 创建实例并自动调用 `init()`
- `init()`: 验证 u-boot 配置的 PLL 和时钟分频 (不修改寄存器)

**外设时钟支持** (`peripheral.rs`):
- I2C: 100/200MHz
- UART: 可配置频率
- SPI: 可配置频率
- PWM: 可配置频率
- ADC: SARADC, TSADC
- MMC/EMMC/SDIO/SFC: 支持频率设置

**时钟类型判断** (`clock/mod.rs`):
- `is_pll_clk()`, `is_i2c_clk()`, `is_uart_clk()` 等
- `get_i2c_num()`, `get_uart_num()`, `get_spi_num()` 等提取编号

## 测试策略

**单元测试** (`cargo test --lib`):
- 寄存器位掩码验证
- PLL 频率计算一致性
- u-boot 配置值验证
- 时钟 ID 范围和编号提取

**集成测试** (`tests/test.rs`):
- 需要裸机环境和硬件
- 使用 `bare-test` 框架
- 通过 `ostool` 运行
- 测试 EMMC 时钟集成

## 文档

详细文档位于 `doc/3588/`:
- `CRU_INIT_VERIFICATION.md`: CRU 初始化验证
- `REFACTOR_2025-12-31.md`: 重构记录
- `TEST_REPORT.md`: 测试报告

## 扩展新芯片

**步骤**:
1. 在 `variants/` 下创建新目录 (如 `rk3568/`)
2. 实现芯片特定的 `Cru` 结构体并实现 `CruOp`
3. 定义寄存器常量和偏移 (`consts.rs`)
4. 实现外设时钟支持 (`peripheral.rs`)
5. 在 `src/clock/mod.rs` 的 `Cru` enum 添加变体
6. 在 `variants/mod.rs` 中导出时钟 ID 常量

**注意事项**:
- 保持与 u-boot 实现一致
- 所有配置参考 u-boot 源码
- 不要主动执行 git 操作 (除非用户明确要求)
