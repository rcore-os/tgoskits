# rockchip-soc

Rockchip SoC 的 Rust `no_std` 驱动实现，提供时钟复位单元 (CRU) 和引脚控制 (PINCTRL) 驱动。

## 当前支持

### RK3588

#### 时钟复位单元 (CRU)

- ✅ **Trait 抽象层**: 使用 `enum_dispatch` 实现零成本抽象
- ✅ **PLL 时钟**: 9 个 PLL (B0PLL, B1PLL, LPLL, CPLL, GPLL, NPLL, V0PLL, AUPLL, PPLL)
- ✅ **外设时钟**: I2C, UART, SPI, MMC/EMMC/SDIO, PWM, ADC 等
- ✅ **时钟门控**: 支持动态时钟使能/禁用
- ✅ **复位控制**: 统一的复位 ID 和控制接口
- ✅ **频率配置**: 支持整数和小数分频
- ✅ **初始化验证**: 对比 u-boot 配置验证

#### 引脚控制 (PINCTRL)

- ✅ **GPIO 方向**: 输入/输出设置和获取
- ✅ **引脚配置**: 上拉/下拉/高阻态
- ✅ **GPIO 操作**: 读写单个引脚
- ✅ **类型安全**: 强类型引脚 ID 系统

**详细文档**: [doc/3588/](doc/3588/)

- [CRU 初始化验证](doc/3588/CRU_INIT_VERIFICATION.md)
- [PLL 配置说明](doc/3588/PLL.md)
- [PLL 寄存器读取](doc/3588/PLL_READING.md)
- [测试报告](doc/3588/TEST_REPORT.md)
- [重构记录](doc/3588/REFACTOR_2025-12-31.md)

## 快速开始

### 环境要求

- Rust nightly toolchain (见 `rust-toolchain.toml`)
- 目标架构: `aarch64-unknown-none-softfloat`
- 集成测试需要: `ostool` (用于裸机测试运行)

### 运行测试

```bash
# 库检查和单元测试
cargo check --test test --target aarch64-unknown-none-softfloat
cargo test --lib

# 集成测试 (需要 ostool 和硬件)
cargo install ostool
cargo test --test test --target aarch64-unknown-none-softfloat -- uboot

# 代码格式化
cargo fmt --all
```

## 项目结构

```text
rockchip-soc/
├── src/
│   ├── clock/                 # 时钟通用层
│   │   ├── mod.rs             # CruOp trait, ClkId, 错误类型
│   │   ├── pll.rs             # 通用 PLL 类型
│   │   └── error.rs           # 错误定义
│   ├── pinctrl/               # 引脚控制通用层
│   │   ├── mod.rs             # PinCtrl trait
│   │   ├── gpio/              # GPIO 操作
│   │   └── pinconf.rs         # 引脚配置
│   ├── rst.rs                 # 复位控制 (RstId, ResetRockchip)
│   ├── syscon/                # 系统控制
│   └── variants/              # 变体层
│       ├── mod.rs             # 变体入口，导出时钟 ID 常量
│       └── rk3588/            # RK3588 特定实现
│           ├── cru/           # CRU 实现
│           │   ├── mod.rs     # Cru + CruOp trait 实现
│           │   ├── pll.rs     # PLL 配置和计算
│           │   ├── consts.rs  # 寄存器偏移
│           │   ├── gate.rs    # 时钟门控表
│           │   ├── peripheral.rs   # 外设时钟
│           │   └── clock/mod.rs    # 时钟 ID 常量
│           └── pinctrl/       # PINCTRL 实现
├── doc/
│   └── 3588/                  # RK3588 文档
└── Cargo.toml
```

## 核心架构

### 分层设计

**1. Trait 抽象层**

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

### API 使用示例

```rust
use rockchip_soc::{Cru, CruOp, SocType};

// 创建 CRU 实例 (自动初始化)
let cru = Cru::new(SocType::Rk3588, cru_base_addr, sys_grf_addr);

// 时钟操作
cru.clk_enable(CLK_I2C1)?;
let rate = cru.clk_get_rate(CLK_I2C1)?;
cru.clk_set_rate(CLK_I2C1, 100_000_000)?;

// 复位控制
cru.reset_assert(RstId::new(100));
cru.reset_deassert(RstId::new(100));
```

## 设计原则

- **SOLID**: 单一职责、开闭原则、里氏替换、接口隔离、依赖倒置
- **KISS**: 保持代码和设计简洁直观
- **DRY**: 避免重复，统一相似功能实现
- **YAGNI**: 只实现当前明确需要的功能

## 扩展新芯片

1. 在 `variants/` 下创建新目录 (如 `rk3568/`)
2. 实现芯片特定的 `Cru` 结构体并实现 `CruOp`
3. 定义寄存器常量和偏移 (`consts.rs`)
4. 实现外设时钟支持 (`peripheral.rs`)
5. 在 `src/clock/mod.rs` 的 `Cru` enum 添加变体
6. 在 `variants/mod.rs` 中导出时钟 ID 常量

## 许可证

MIT License

---

**版本**: 0.1.0
**更新时间**: 2025-01-15
