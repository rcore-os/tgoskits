# RK3588 PLL 时钟配置

## 概述

RK3588 共有 9 个 PLL (Phase-Locked Loop) 时钟,用于生成系统所需的各种时钟频率。

## PLL 列表

| PLL   | ID  | 数组索引 | 用途             | 默认频率   |
|-------|-----:|---------:|------------------|-----------|
| B0PLL | 1   | 0        | 大核0 PLL        | 816 MHz   |
| B1PLL | 2   | 1        | 大核1 PLL        | 816 MHz   |
| LPLL  | 3   | 2        | 小核 DSU PLL     | 816 MHz   |
| CPLL  | 4   | 3        | 中心/通用 PLL    | 1500 MHz  |
| GPLL  | 5   | 4        | 通用 PLL         | 1188 MHz  |
| NPLL  | 6   | 5        | 网络/视频 PLL    | 850 MHz   |
| V0PLL | 7   | 6        | 视频 PLL         | -         |
| AUPLL | 8   | 7        | 音频 PLL         | -         |
| PPLL  | 9   | 8        | PMU PLL          | 1100 MHz  |

**重要**: `PllId` 枚举值直接匹配设备树绑定 (`rk3588-cru.h`) 的 PLL ID,从 **1** 开始。

## 寄存器映射

| PLL   | CON Offset | Mode Offset | Mode Shift | Lock Shift |
|-------|-----------:|-------------|------------|------------|
| B0PLL | 0x50000   | 0x50280     | 0          | 15         |
| B1PLL | 0x52020   | 0x52280     | 0          | 15         |
| LPLL  | 0x58040   | 0x58280     | 0          | 15         |
| CPLL  | 0x1a0     | 0x280       | 8          | 15         |
| GPLL  | 0x1c0     | 0x280       | 2          | 15         |
| NPLL  | 0x1e0     | 0x280       | 0          | 15         |
| V0PLL | 0x160     | 0x280       | 4          | 15         |
| AUPLL | 0x180     | 0x280       | 6          | 15         |
| PPLL  | 0x8200    | 0x280       | 10         | 15         |

## PLL 参数

RK3588 使用 (p, m, s, k) 参数格式:

- **p**: Pre-divider (预分频)
- **m**: Main divider (主分频/反馈分频)
- **s**: Post-divider power (后分频指数, 2^S)
- **k**: Fractional divider (16-bit 小数分频)

## 频率计算公式

参考 u-boot `clk_pll.c` 的 `rk3588_pll_get_rate()`:

```text
rate = (OSC_HZ / p) * m
if k != 0:
    frac_rate = (OSC_HZ * k) / (p * 65536)
    rate = rate + frac_rate
rate = rate >> s
```

### 整数分频示例

GPLL @ 1.188 GHz:
```rust
fin = 24_000_000, p = 2, m = 198, s = 1, k = 0

rate = (24MHz / 2) * 198
     = 12MHz * 198
     = 2376MHz
result = 2376MHz >> 1 = 1188MHz ✓
```

### 小数分频示例

786.432 MHz (目标频率):
```rust
fin = 24_000_000, p = 2, m = 262, s = 2, k = 9437

rate = (24MHz / 2) * 262 = 3144MHz
frac_rate = (24MHz * 9437) / (2 * 65536)
          = 226488000000 / 131072
          = 1727966 (整数精度限制)
result = (3144MHz + 1.727966MHz) >> 2
       = 786.431991 MHz
```

**注意**: 由于整数除法精度限制,实际输出频率为 786.431991 Hz,与目标 786.432 Hz 有微小差异。

## 预设频率表

支持 17 个预设频率 (100MHz - 1.5GHz):

| 频率 (MHz) | P  | M   | S  | K     |
|-----------:|----|-----|----|-------|
| 1500      | 2  | 250 | 1  | 0     |
| 1200      | 2  | 200 | 1  | 0     |
| 1188      | 2  | 198 | 1  | 0     |
| 1100      | 3  | 550 | 2  | 0     |
| 1008      | 2  | 336 | 2  | 0     |
| 1000      | 3  | 500 | 2  | 0     |
| 900       | 2  | 300 | 2  | 0     |
| 850       | 3  | 425 | 2  | 0     |
| 816       | 2  | 272 | 2  | 0     |
| 786.432   | 2  | 262 | 2  | 9437  |
| 786       | 1  | 131 | 2  | 0     |
| 742.5     | 4  | 495 | 2  | 0     |
| 722.5344  | 8  | 963 | 2  | 24850 |
| 600       | 2  | 200 | 2  | 0     |
| 594       | 2  | 198 | 2  | 0     |
| 200       | 3  | 400 | 4  | 0     |
| 100       | 3  | 400 | 5  | 0     |

## 使用示例

### 获取 PLL 配置

**推荐方式** - 使用 `get_pll()` 函数:

```rust
use rockchip_soc::rk3588::cru::pll::{PllId, get_pll};

// 通过 PllId 获取 GPLL 配置
let gpll = get_pll(PllId::GPLL);

println!("GPLL 时钟 ID: {}", gpll.id);           // 5
println!("控制寄存器偏移: 0x{:x}", gpll.con_offset);  // 0x1c0
```

**⚠️ 不推荐** - 直接访问数组 (仅内部使用):

```rust
// 需要手动计算索引: PllId - 1
let gpll = &RK3588_PLL_CLOCKS[PllId::GPLL as usize - 1];
```

### 计算 PLL 输出频率

```rust
use rockchip_soc::rk3588::cru::pll::calc_pll_rate;

// 计算整数分频输出
let rate = calc_pll_rate(24_000_000, 2, 198, 1, 0);
assert_eq!(rate, 1_188_000_000);  // 1188 MHz

// 计算小数分频输出
let rate = calc_pll_rate(24_000_000, 2, 262, 2, 9437);
assert_eq!(rate, 786_431_991);  // 786.431991 MHz
```

### 获取默认频率

```rust
use rockchip_soc::rk3588::cru::pll::PllId;

// 获取 GPLL 默认频率
if let Some(rate) = PllId::GPLL.default_rate() {
    println!("GPLL 默认频率: {} Hz", rate);  // 1188000000 Hz
}
```

### 遍历所有 PLL

```rust
use rockchip_soc::rk3588::cru::pll::{PllId, get_pll};

// 遍历所有有效的 PLL (1-9)
for id in 1..=9 {
    if let Ok(pll_id) = id.try_into() {
        let pll = get_pll(pll_id);
        println!("{}: ID={}, 偏移=0x{:x}",
                 pll_id.name(), pll.id, pll.con_offset);
    }
}
```

## 设计亮点

### PllId 枚举优化

`PllId` 枚举值直接匹配设备树绑定:

```rust
pub enum PllId {
    B0PLL = 1,  // 匹配 #define PLL_B0PLL 1
    B1PLL = 2,  // 匹配 #define PLL_B1PLL 2
    LPLL = 3,   // 匹配 #define PLL_LPLL  3
    ...
}
```

**优点**:
- ✅ 消除了两套 ID 系统的混淆
- ✅ `PllId` 值可直接用于时钟框架
- ✅ 无需 `+1/-1` 转换
- ✅ 语义清晰,不易出错

### 安全的访问方式

通过 `get_pll()` 函数封装数组访问:

```rust
pub const fn get_pll(id: PllId) -> &'static PllClock {
    &RK3588_PLL_CLOCKS[id as usize - 1]  // 内部处理索引转换
}
```

**优点**:
- ✅ 隐藏索引转换细节
- ✅ 防止直接数组访问错误
- ✅ API 简洁直观

## 与 u-boot 的对比

| 项目 | u-boot C | Rust 实现 | 优势 |
|------|----------|-----------|------|
| PLL 数量 | 9 个 | 9 个 | ✅ 一致 |
| 频率表项 | 17 项 | 17 项 | ✅ 一致 |
| 寄存器偏移 | 一致 | 一致 | ✅ 一致 |
| 计算公式 | 一致 | 一致 | ✅ 一致 |
| ID 系统 | 混乱 | 统一 | ✅ **更清晰** |
| 类型安全 | ❌ | ✅ 枚举 | ✅ **更好** |
| 编译时检查 | ❌ | ✅ const fn | ✅ **更好** |
| 数组访问 | 直接 | 封装 | ✅ **更安全** |

## 测试

所有 PLL 配置均已通过单元测试验证:

```bash
cargo test --lib pll
```

测试覆盖:
- ✅ PLL 数量和 ID 验证 (1-9)
- ✅ 寄存器偏移验证
- ✅ 频率计算验证 (整数和小数分频)
- ✅ 9 个独立 PLL 配置验证
- ✅ 与 u-boot C 代码一致性验证

测试报告: [TEST_REPORT.md](TEST_REPORT.md)

## 参考资料

1. u-boot 源码: `drivers/clk/rockchip/clk_rk3588.c`
2. 头文件: `arch/arm/include/asm/arch-rockchip/cru_rk3588.h`
3. 设备树绑定: `include/dt-bindings/clock/rk3588-cru.h`
4. RK3588 TRM (技术参考手册)

## 设计原则

- **SOLID**: 单一职责,开闭原则
- **KISS**: 简洁的宏定义和清晰的注释
- **DRY**: 复用寄存器偏移函数
- **YAGNI**: 只实现必需的功能

---
**版本**: 2.0
**更新时间**: 2025-12-31
**主要变更**: PllId 优化为从 1 开始,消除 ID 混淆
