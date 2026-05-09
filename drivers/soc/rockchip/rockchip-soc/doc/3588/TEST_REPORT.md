# RK3588 PLL 配置测试报告

**测试时间**: 2025-12-31
**测试命令**: `cargo test --lib pll`
**测试结果**: ✅ 全部通过 (21/21)

## 测试覆盖

### 1. 基础类型测试 (clock/pll.rs)

- ✅ `test_pll_type_values` - PLL 类型枚举值验证
- ✅ `test_pll_flags` - PLL 标志位定义验证

### 2. RK3588 PLL 配置测试 (variants/rk3588/cru/pll.rs)

#### 数量验证

- ✅ `test_pll_count` - PLL 总数验证 (9 个)
- ✅ `test_pll_rate_table_count` - 频率表项数验证 (17 项)

#### 枚举和名称

- ✅ `test_pll_ids` - PLL ID 值验证 (1-9, 匹配设备树)
- ✅ `test_pll_names` - PLL 名称映射验证
- ✅ `test_pll_default_rates` - 默认频率验证

#### 频率计算

- ✅ `test_pll_rate_calculation` - 整数和小数分频计算验证
  - 整数分频: 1.188 GHz ✅
  - 小数分频: 786.431991 MHz ✅

#### 配置偏移

- ✅ `test_pll_config_offsets` - 关键 PLL 寄存器偏移验证 (使用 get_pll())

#### 个体 PLL 配置验证

- ✅ `test_b0pll_config` - B0PLL (BIGCORE0) 配置验证
  - ID: 1, 偏移: 0x50000, 模式: 0x50280
- ✅ `test_b1pll_config` - B1PLL (BIGCORE1) 配置验证
  - ID: 2, 偏移: 0x52020, 模式: 0x52280
- ✅ `test_lpll_config` - LPLL (DSU) 配置验证
  - ID: 3, 偏移: 0x58040, 模式: 0x58280
- ✅ `test_cpll_config` - CPLL (中心/通用) 配置验证
  - ID: 4, 偏移: 0x1a0, 模式: 0x280
- ✅ `test_gpll_config` - GPLL (通用) 配置验证
  - ID: 5, 偏移: 0x1c0, 模式: 0x280
- ✅ `test_npll_config` - NPLL (网络/视频) 配置验证
  - ID: 6, 偏移: 0x1e0, 模式: 0x280
- ✅ `test_v0pll_config` - V0PLL (视频) 配置验证
  - ID: 7, 偏移: 0x160, 模式: 0x280
- ✅ `test_aupll_config` - AUPLL (音频) 配置验证
  - ID: 8, 偏移: 0x180, 模式: 0x280
- ✅ `test_ppll_config` - PPLL (PMU) 配置验证
  - ID: 9, 偏移: 0x8200, 模式: 0x280

#### 综合验证

- ✅ `test_all_pll_common_attributes` - 所有 PLL 通用属性验证
- ✅ `test_pll_rate_table_entries` - 频率表每个条目参数验证
- ✅ `test_pll_config_complete_validation` - 完整配置验证 (使用 get_pll(), 对比 u-boot)

## 测试用例详情

### 频率计算验证

```rust
#[test]
fn test_pll_rate_calculation() {
    // 整数分频: ((24MHz/2)*198) >> 1 = 1188MHz
    let rate = calc_pll_rate(24_000_000, 2, 198, 1, 0);
    assert_eq!(rate, 1_188_000_000);

    // 小数分频: 目标 786.432MHz, 实际 786.431991MHz (整数精度限制)
    let rate = calc_pll_rate(24_000_000, 2, 262, 2, 9437);
    assert_eq!(rate, 786_431_991);
}
```

### PLL 配置验证示例 (使用 get_pll())

```rust
#[test]
fn test_gpll_config() {
    let pll = get_pll(PllId::GPLL);  // ✅ 使用辅助函数

    // 验证时钟 ID (匹配设备树: PLL_GPLL = 5)
    assert_eq!(pll.id, 5, "GPLL ID should be 5");

    // 验证寄存器偏移
    assert_eq!(pll.con_offset, 0x1c0);
    assert_eq!(pll.mode_offset, 0x280);
    assert_eq!(pll.mode_shift, 2);
    assert_eq!(pll.lock_shift, 15);

    // 验证类型和标志
    assert_eq!(pll.pll_type, RockchipPllType::Rk3588);
    assert_eq!(pll.pll_flags, 0);
}
```

### PllId 验证

```rust
#[test]
fn test_pll_ids() {
    // 验证 PLL ID 值 (匹配设备树绑定 rk3588-cru.h)
    assert_eq!(PllId::B0PLL as usize, 1);  // 匹配 PLL_B0PLL
    assert_eq!(PllId::GPLL as usize, 5);  // 匹配 PLL_GPLL
    assert_eq!(PllId::PPLL as usize, 9);  // 匹配 PLL_PPLL
}

#[test]
fn test_pll_count() {
    // PllId 从 1 开始,所以 _Len = 10 (1-9 + _Len)
    assert_eq!(RK3588_PLL_CLOCKS.len(), 9);
    assert_eq!(PllId::_Len as usize, 10);
}
```

## 关键修正历史

### 1. PllId 枚举优化 (v2.0)

**变更**: PllId 枚举值从 1 开始,直接匹配设备树绑定

```rust
// 之前 (v1.0)
pub enum PllId {
    B0PLL,  // 0 - 数组索引
    ...
}

// 现在 (v2.0)
pub enum PllId {
    B0PLL = 1,  // 直接匹配 #define PLL_B0PLL 1
    ...
}
```

**优点**:
- ✅ 消除了两套 ID 系统的混淆
- ✅ `PllId` 值可直接用于时钟框架
- ✅ 无需 `+1/-1` 转换
- ✅ 语义清晰,不易出错

**影响**: 所有测试改为使用 `get_pll()` 函数,不再直接访问数组

### 2. get_pll() 函数封装

**变更**: 强制使用 `get_pll()` 访问 PLL 配置

```rust
pub const fn get_pll(id: PllId) -> &'static PllClock {
    &RK3588_PLL_CLOCKS[id as usize - 1]  // 内部处理索引转换
}
```

**测试变更**:
```rust
// ❌ 旧方式: 直接访问
let pll = &RK3588_PLL_CLOCKS[PllId::GPLL as usize - 1];

// ✅ 新方式: 使用辅助函数
let pll = get_pll(PllId::GPLL);
```

### 3. 频率计算公式优化

**问题**: 小数分频精度损失

**修复**: 采用 u-boot 公式
```rust
rate = (fin / p) * m
frac_rate = (fin * k) / (p * 65536)
result = (rate + frac_rate) >> s
```

**位置**: pll.rs:216-228

### 4. 测试用例调整

**说明**: 786432000 Hz 是目标频率,实际计算值为 786431991 Hz

**原因**: 整数除法精度限制

**位置**: pll.rs:241-254

## 与 u-boot 对比

| 项目 | u-boot C | Rust 实现 | 优势 |
|------|----------|-----------|------|
| PLL 数量 | 9 个 | 9 个 | ✅ 一致 |
| 频率表项 | 17 项 | 17 项 | ✅ 一致 |
| 寄存器偏移 | 一致 | 一致 | ✅ 一致 |
| 计算公式 | 一致 | 一致 | ✅ 一致 |
| **ID 系统** | **混乱** | **统一** | ✅ **更清晰** |
| 类型安全 | ❌ | ✅ 枚举 | ✅ **更好** |
| 编译时检查 | ❌ | ✅ const fn | ✅ **更好** |
| **数组访问** | **直接** | **封装** | ✅ **更安全** |

## 遵循的设计原则

### SOLID

- ✅ **单一职责**: PllId, 配置数组, 计算函数职责明确
- ✅ **开闭原则**: PllRateTable 可扩展

### KISS (简单至上)

- ✅ const fn 宏简化配置
- ✅ 清晰的注释说明
- ✅ `get_pll()` 简化访问

### DRY (不重复)

- ✅ pll! 宏消除重复
- ✅ 寄存器偏移函数复用

### YAGNI (精益求精)

- ✅ 只实现必需的 9 个 PLL
- ✅ 预设频率表覆盖常用场景

## 测试执行

```bash
$ cd rockchip-soc
$ cargo test --lib pll

   Compiling rockchip-soc v0.1.0
    Finished `test` profile [unoptimized + debuginfo] target(s)

running 21 tests
test clock::pll::tests::test_pll_flags ... ok
test clock::pll::tests::test_pll_type_values ... ok
test variants::rk3588::cru::pll::tests::test_all_pll_common_attributes ... ok
test variants::rk3588::cru::pll::tests::test_aupll_config ... ok
test variants::rk3588::cru::pll::tests::test_b0pll_config ... ok
test variants::rk3588::cru::pll::tests::test_b1pll_config ... ok
test variants::rk3588::cru::pll::tests::test_cpll_config ... ok
test variants::rk3588::cru::pll::tests::test_gpll_config ... ok
test variants::rk3588::cru::pll::tests::test_lpll_config ... ok
test variants::rk3588::cru::pll::tests::test_npll_config ... ok
test variants::rk3588::cru::pll::tests::test_pll_config_complete_validation ... ok
test variants::rk3588::cru::pll::tests::test_pll_config_offsets ... ok
test variants::rk3588::cru::pll::tests::test_pll_count ... ok
test variants::rk3588::cru::pll::tests::test_pll_default_rates ... ok
test variants::rk3588::cru::pll::tests::test_pll_ids ... ok
test variants::rk3588::cru::pll::tests::test_pll_names ... ok
test variants::rk3588::cru::pll::tests::test_pll_rate_calculation ... ok
test variants::rk3588::cru::pll::tests::test_pll_rate_table_count ... ok
test variants::rk3588::cru::pll::tests::test_pll_rate_table_entries ... ok
test variants::rk3588::cru::pll::tests::test_ppll_config ... ok
test variants::rk3588::cru::pll::tests::test_v0pll_config ... ok

test result: ok. 21 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

## 结论

✅ RK3588 PLL 配置与 u-boot C 代码完全一致
✅ 所有 21 个单元测试通过
✅ **PllId 优化消除了双重 ID 系统的混淆**
✅ 类型安全性和编译时检查优于 C 实现
✅ 代码质量符合 Rust 最佳实践

---
**报告版本**: 2.0
**测试通过时间**: 2025-12-31
**主要变更**: PllId 优化为从 1 开始,消除 ID 混淆,强制使用 get_pll() 访问

