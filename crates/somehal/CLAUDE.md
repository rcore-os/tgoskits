[根目录](../../CLAUDE.md) > [crates](../) > **somehal**

# SomeHAL - 硬件抽象层

## 模块职责

SomeHAL (Some Hardware Abstraction Layer) 提供跨平台的硬件抽象接口，支持 AArch64 和 LoongArch64 架构，为上层内核提供统一的硬件访问接口。

## 入口与启动

### 主要入口点
- **入口宏**: `#[somehal::entry]` - 定义平台主函数
- **启动流程**: 在 `platform/sparreal-rt` 中被调用来启动内核

### 启动示例
```rust
#[somehal::entry]
fn main() -> ! {
    somehal::println!("Starting Sparreal OS kernel...");
    sparreal_kernel::hal::setup::start_kernel()
}
```

## 对外接口

### 平台接口 (Platform Trait)
通过 `#[api_impl]` 宏实现的平台接口，包括：
- `wait_for_interrupt()`: 等待中断
- `shutdown()`: 系统关闭
- 其他平台特定的操作

### 架构支持
- **AArch64**: ARM 64位架构支持
- **LoongArch64**: 龙芯 64位架构支持

### 特性开关
- `efi`: EFI 启动支持
- `hv`: 虚拟化支持
- `mmu`: 内存管理单元支持
- `uspace`: 用户空间支持（依赖 mmu）

## 关键依赖与配置

### 核心依赖
```toml
[dependencies]
acpi = "6.0.1"              # ACPI 表解析
aml = "0.16"                # AML 字节码解析
fdt-parser = "0.5"          # 设备树解析
loongArch64 = "0.2"         # LoongArch64 支持 (目标特定)
aarch64-cpu = "11"          # AArch64 CPU 支持 (目标特定)
some-serial = "0.3"         # 串口通信
```

### 架构特定依赖
- **LoongArch64**: uefi, uefi-raw, loongArch64
- **AArch64**: aarch64-cpu, aarch64-cpu-ext, kasm-aarch64

## 数据模型

### 设备树支持
- **FDT 解析**: 使用 `fdt-parser` 解析扁平设备树
- **ACPI 支持**: 解析 ACPI 表获取硬件信息
- **AML 解释**: 执行 AML 字节码进行设备配置

### 内存管理
- **页表**: 与 `page-table-generic` 集成
- **内存映射**: 支持物理地址到虚拟地址的映射
- **DMA**: 支持 DMA 操作的内存管理

### 中断处理
- **中断控制器**: 支持 GIC (AArch64) 等中断控制器
- **中断向量**: 统一的中断向量表管理
- **中断分发**: 中断到内核的分发机制

## 测试与质量

### 当前测试状态
- ⚠️ **单元测试**: 缺少详细的硬件抽象层测试
- ⚠️ **集成测试**: 通过 QEMU 进行基本的功能测试
- ⚠️ **硬件测试**: 需要在实际硬件上进行验证

### 建议的测试策略
1. **模拟器测试**: 在 QEMU 中测试各种硬件初始化场景
2. **架构测试**: 分别测试 AArch64 和 LoongArch64 的功能
3. **边界测试**: 测试硬件故障、设备缺失等异常情况
4. **性能测试**: 测试硬件访问的性能开销

### 质量工具
- **架构特定编译**: 确保每个架构的代码正确编译
- **静态分析**: 使用 Clippy 检查代码质量
- **文档测试**: 验证文档中的代码示例

## 常见问题 (FAQ)

### Q: 如何添加新的架构支持？
A: 在 `src/arch/` 下创建新的架构目录，实现必要的 HAL 接口，并在 `build.rs` 中添加架构特定的构建逻辑。

### Q: 如何添加新的设备驱动？
A: 通过设备树或 ACPI 检测设备，实现相应的驱动程序，并通过 HAL 接口暴露给上层。

### Q: EFI 启动如何工作？
A: 启用 `efi` 特性后，可以使用 UEFI 环境进行启动，需要实现 EFI 应用程序入口。

### Q: 虚拟化支持包括什么？
A: `hv` 特性提供基本的虚拟化支持，包括 Stage-2 页表和虚拟机管理。

## 相关文件清单

### 核心文件
- `src/lib.rs` - 库入口
- `build.rs` - 构建脚本，处理架构特定逻辑

### 架构支持
- `src/arch/loongarch64/head.rs` - LoongArch64 启动代码
- `src/arch/aarch64/` - AArch64 架构支持（推测）
- `src/efi_stub/pe.rs` - EFI PE 文件支持

### 硬件抽象
- `src/platform/` - 平台特定实现
- `src/cpu/` - CPU 抽象层
- `src/memory/` - 内存管理抽象
- `src/interrupt/` - 中断处理抽象

### 设备支持
- `src/serial/` - 串口设备支持
- `src/timer/` - 定时器设备支持
- `src/pci/` - PCI 设备支持（推测）

### 构建相关
- `build.rs` - 构建脚本
- `Cargo.toml` - 项目配置

---

## 变更记录 (Changelog)

### 2025-12-03 09:30:10
- 初始化 somehal 模块文档
- 完成架构支持和特性分析
- 识别设备驱动和硬件抽象接口
- 建立测试策略和常见问题解答