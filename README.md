# Rockchip Power Management Driver (rockchip-pm)

一个用于 Rockchip SoC 的 Rust 电源管理驱动库，提供基础的电源域控制功能。

## 特性

- 🔋 **基础电源域控制**: 支持 RK3588 电源域的开关操作
- 🛡️ **内存安全**: 利用 Rust 类型系统确保内存和线程安全
- 📋 **无标准库**: `#![no_std]` 设计，适用于嵌入式环境
- 🎯 **硬件准确**: 直接寄存器访问，无抽象开销
- 🔌 **名称查找**: 支持通过名称查找电源域
- 📦 **驱动框架**: 基于 rdif-base 驱动框架

## 快速开始

```rust
use rockchip_pm::{RockchipPM, RkBoard, PowerDomain};
use core::ptr::NonNull;

// 初始化 RK3588 PMU（基础地址通常来自设备树）
let pmu_base = unsafe { NonNull::new_unchecked(0xfd8d8000 as *mut u8) };
let mut pm = RockchipPM::new(pmu_base, RkBoard::Rk3588);

// 使用 ID 控制电源域
let npu_domain = PowerDomain::new(8);  // NPU 主域
pm.power_domain_on(npu_domain)?;

// 通过名称查找电源域
if let Some(npu) = pm.get_power_dowain_by_name("npu") {
    pm.power_domain_on(npu)?;
}

// 关闭电源域
pm.power_domain_off(npu)?;
```

## API 文档

### 核心结构体

```rust
pub struct RockchipPM {
    // 私有字段：板型信息、寄存器接口、电源域配置
}
```

### 板型支持

```rust
pub enum RkBoard {
    Rk3588,  // 已实现
    Rk3568,  // 未实现（占位符）
}
```

### 电源域类型

```rust
pub struct PowerDomain {
    // 电源域 ID
}
impl PowerDomain {
    pub fn new(id: u32) -> Self
    pub fn id(&self) -> u32
}
```

### 错误处理

```rust
pub enum NpuError {
    DomainNotFound,  // 电源域不存在
    Timeout,         // 操作超时
    HardwareError,   // 硬件错误
}

pub type NpuResult<T> = Result<T, NpuError>;
```

### 主要方法

```rust
impl RockchipPM {
    /// 创建新的电源管理器实例
    pub fn new(base: NonNull<u8>, board: RkBoard) -> Self

    /// 通过名称查找电源域
    pub fn get_power_dowain_by_name(&self, name: &str) -> Option<PowerDomain>

    /// 开启指定电源域
    pub fn power_domain_on(&mut self, domain: PowerDomain) -> NpuResult<()>

    /// 关闭指定电源域
    pub fn power_domain_off(&mut self, domain: PowerDomain) -> NpuResult<()>
}
```

## 支持的电源域 (RK3588)

### 计算域
- **NPU** (ID: 8) - 神经处理单元主域
- **NPUTOP** (ID: 9) - NPU 顶层域
- **NPU1** (ID: 10) - NPU 核心 1
- **NPU2** (ID: 11) - NPU 核心 2

### 图形域
- **GPU** (ID: 0) - 图形处理单元
- **VOP** (ID: 26) - 视频输出处理器
- **VO0** (ID: 27) - 视频输出 0
- **VO1** (ID: 28) - 视频输出 1

### 视频域
- **VCODEC** (ID: 4) - 视频编解码器主域
- **VENC0** (ID: 5) - 视频编码器 0
- **VENC1** (ID: 6) - 视频编码器 1
- **RKVDEC0** (ID: 7) - Rockchip 视频解码器 0
- **RKVDEC1** (ID: 12) - Rockchip 视频解码器 1
- **AV1** (ID: 18) - AV1 解码器
- **VDPU** (ID: 2) - 视频处理单元

### 图像域
- **VI** (ID: 29) - 视频输入
- **ISP1** (ID: 30) - 图像信号处理器
- **RGA30** (ID: 15) - 光栅图形加速器 30
- **RGA31** (ID: 16) - 光栅图形加速器 31

### 总线域
- **PHP** (ID: 17) - PHP 控制器
- **GMAC** (ID: 19) - 千兆以太网 MAC
- **PCIE** (ID: 20) - PCIe 控制器
- **SDIO** (ID: 21) - SDIO 控制器
- **USB** (ID: 22) - USB 控制器
- **SDMMC** (ID: 23) - SD/MMC 控制器

### 其他域
- **AUDIO** (ID: 1) - 音频子系统
- **FEC** (ID: 24) - 前向纠错编码
- **NVM** (ID: 25) - 非易失性存储器
- **NVM0** (ID: 3) - NVM 域 0

## 项目结构

```
rockchip-pm/
├── src/
│   ├── lib.rs              # 主 API 和 RockchipPM 结构
│   ├── registers/mod.rs    # 寄存器定义和访问抽象
│   └── variants/           # 芯片特定实现
│       ├── mod.rs          # PowerDomain 类型和通用结构
│       ├── _macros.rs      # 电源域定义宏
│       └── rk3588.rs       # RK3588 电源域定义
├── tests/
│   └── test.rs             # NPU 电源控制集成测试
├── Cargo.toml              # 项目配置和依赖
├── build.rs                # 构建脚本
├── rust-toolchain.toml     # Rust 工具链配置
└── README.md               # 项目文档
```

## 构建和测试

### 环境要求

- Rust 1.75+ (nightly)
- aarch64-unknown-none-softfloat 目标支持

### 构建步骤

```bash
# 添加目标架构支持
rustup target add aarch64-unknown-none-softfloat

# 构建库
cargo build

# 构建发布版本
cargo build --release

# 检查代码
cargo check
```

### 运行测试

项目包含 1 个集成测试，验证 NPU 电源域控制功能：

```bash
# 在开发板上运行测试（需要 U-Boot 环境）
cargo uboot
```

**测试内容：**
- ✅ RK3588 NPU 相关电源域开关
- ✅ 设备树电源域解析
- ✅ 寄存器访问验证

## 依赖项

### 核心依赖

- **rdif-base** (v0.7): 设备驱动框架
- **tock-registers** (v0.10): 类型安全的寄存器访问和位域操作
- **mbarrier** (v0.1): 内存屏障原语，用于寄存器访问排序
- **dma-api** (v0.5): DMA API 支持
- **log** (v0.4): 日志记录

### 开发依赖

- **bare-test** (v0.7): 裸机测试框架

### 构建依赖

- **bare-test-macros** (v0.2): 测试宏定义

## 硬件兼容性

### 支持的芯片

- **RK3588**: ✅ 已完整实现
- **RK3568**: ❌ 未实现（代码中为 `unimplemented!()` 占位符）

### 开发板

- **RK3588 板型**:
  - Orange Pi 5/5 Plus/5B
  - Rock 5A/5B/5C
  - NanoPC-T6
  - 其他基于 RK3588/RK3588S 的开发板

### 内存映射要求

使用本库需要确保：

1. **正确的 PMU 基础地址**:
   - RK3588: 通常为 `0xfd8d8000`（请从设备树验证）
2. **内存映射权限**: PMU 寄存器区域的读写权限
3. **时钟配置**: 确保 PMU 时钟正确配置

## 工作原理

### 电源开启流程

1. 写入电源控制寄存器，开启电源域
2. 轮询状态寄存器，等待电源域稳定（最多 10000 次循环）
3. 验证电源状态是否成功开启

### 电源关闭流程

1. 写入电源控制寄存器，关闭电源域
2. 轮询状态寄存器，等待电源域稳定（最多 10000 次循环）
3. 验证电源状态是否成功关闭

## 安全注意事项

⚠️ **重要**: 本库直接操作硬件寄存器。使用前请确保：

- 系统 PMU 硬件已正确初始化
- 没有其他驱动同时控制相同电源域
- 在真实硬件上使用前进行充分验证

## License

本项目采用 [MIT 许可证](LICENSE)。

## 贡献

欢迎贡献！请提交 Issue 和 Pull Request。

### 开发环境设置

```bash
# 克隆项目
git clone https://github.com/drivercraft/rockchip-pm.git
cd rockchip-pm

# 安装开发工具
rustup component add rustfmt clippy

# 格式化代码
cargo fmt

# 运行代码检查
cargo clippy
```

## 参考资料

- Linux 内核 `drivers/soc/rockchip/pm_domains.c`
- RK3588 技术参考手册
- Rockchip 电源域设备树绑定文档

---

**注意**: 本驱动是底层系统软件。确保硬件寄存器操作符合芯片规格。在生产环境中使用前请进行充分测试。