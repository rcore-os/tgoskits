# SomeHAL 运行时硬件抽象层

`platforms/somehal` 是 TGOSKits 中连接 `someboot` 与 `axplat-dyn` 的多架构运行时硬件抽象层。SomeHAL 源自 Sparreal OS 的同名组件；在 TGOSKits 中，它作为工作区共享软件包维护，版本、功能开关和依赖以 `platforms/somehal/Cargo.toml` 为准。

该组件向操作系统无关的平台适配层提供固件设备发现、运行时中断控制器、定时器、控制台设备选择、CPU-local 绑定和 MMIO 后端。固件入口、早期页表、重定位和次核唤醒仍由 `someboot` 负责，ArceOS 运行时接口则由 `axplat-dyn` 适配。

## 1. 组件边界

`somehal` 不直接实现某个操作系统。它把四类体系结构后端整理成共享运行时接口，再交给直接使用方 `axplat-dyn`；后者实现 `ax-plat` 契约并服务 ArceOS、StarryOS 与 Axvisor。这个边界避免设备发现和中断控制器代码进入单个操作系统。

| 层 | 主要责任 | 代码入口 |
| --- | --- | --- |
| 固件与早期启动 | 固件入口、内存映射、页表、重定位、SMP 唤醒和早期控制台 | `../someboot` |
| 运行时硬件抽象 | FDT/ACPI 设备发现、中断域、定时器、控制台选择和架构后端 | `platforms/somehal/src/lib.rs` |
| 操作系统平台适配 | 实现 `KernelOp`，接入 CPU-local 和内核 MMIO，并导出 `ax-plat` 接口 | `platforms/axplat-dyn/src/boot.rs`、`platforms/axplat-dyn/src/init.rs` |

集成时不要绕过 `axplat-dyn` 复制平台初始化流程。现有 `InitIfImpl` 已规定 `somehal::timer::enable()`、调度时钟初始化以及分页后 `somehal::post_paging()` 的调用位置。

### 1.1 公共接口

`lib.rs` 公开运行时所需的最小表面，并重新导出部分 `someboot` 能力。公开模块和 crate 根导出如下，内部中断路由和架构辅助逻辑保持私有或仅由 `arch` 后端使用。

| 模块或导出 | 用途 |
| --- | --- |
| `setup::KernelOp` | 提供 CPU-local 绑定、当前 CPU 查询和 MMIO 映射回调 |
| `irq` | 中断域登记、路由解析、使能、亲和性、IPI 和 `ActiveIrq` 事务 |
| `arch` | 按目标架构选择 GIC、PLIC、LoongArch 中断控制器或 x86 APIC 后端 |
| `platform` | 从固件信息导出运行时平台名 |
| `cpu` | 优先读取内核 CPU-local 状态，并在早期回退到 `someboot` |
| `console_device_id`、`ConsoleDeviceIdError` | 从 crate 根导出控制台解析结果：解析 `console=`、ACPI SPCR 和 FDT `stdout-path` 后返回控制台 `DeviceId` |
| `cache`、`rtc`、`timer` | 暴露 DMA 缓存维护、固件时钟和体系结构定时能力 |

`irq::begin_irq(raw)` 返回的 `ActiveIrq` 必须在中断分发期间保持存活；其析构负责架构相关的结束操作。调用方不应自行复制控制器 EOI 或重新推导固件中断号。

### 1.2 支持范围

当前后端覆盖 AArch64、LoongArch64、RISC-V 64 和 x86_64。选择后端由目标架构的条件编译完成，调用方继续使用统一的 `somehal::irq`、`timer` 和 `arch` 接口。

| 目标架构 | 运行时控制器与定时能力 |
| --- | --- |
| AArch64 | GICv2/GICv3 运行时识别、ARM Generic Timer、ITS/MSI 路由 |
| LoongArch64 | EIOINTC、PCH-PIC、LIOINTC 和体系结构定时器 |
| RISC-V 64 | PLIC、本地定时器与软件 IPI |
| x86_64 | Local APIC、I/O APIC、MSI 和 Local APIC timer |

后端差异留在 `src/arch/<arch>/`。在组件内新增架构时实现 `common::PlatOp`，再在 `lib.rs` 中增加目标架构的 `arch` 路径；公共调用方不需要按控制器类型分支。

## 2. 集成契约

直接使用 `somehal` 的平台运行时必须提供一个长期存活的 `KernelOp` 实现。`somehal::init()` 会把该实现安装为 MMIO 后端，因此在任何驱动调用 `mmio_api::ioremap()` 之前必须完成初始化。

### 2.1 KernelOp 回调

`KernelOp` 继承 `MmioOp`，并只增加 CPU-local 绑定和当前 CPU 查询。两个方法都有默认“不支持”行为；启用 SMP 的运行时必须覆盖 `bind_current_cpu()`，需要在内核初始化后查询当前 CPU 时还应覆盖 `current_cpu_idx()`。

```rust
pub trait KernelOp: MmioOp {
    fn bind_current_cpu(&self, cpu_index: CpuIndex) -> Result<(), CpuBindError> {
        Err(CpuBindError::Unsupported)
    }

    fn current_cpu_idx(&self) -> Option<usize> {
        None
    }
}
```

`MmioOp` 负责 `ioremap()` 与 `iounmap()`。TGOSKits 的 `axplat-dyn` 实现将绑定委托给 `ax_percpu` 和 `cpu_local`，并把 MMIO 操作转发给 `axklib::mmio::op()`；新增平台适配应复用这些既有所有权边界，而不是让驱动直接访问物理地址。

### 2.2 主核与次核入口

主核入口使用 `#[somehal::entry(Kernel)]`。宏会在函数体前插入 `somehal::init(&Kernel)`，因此集成代码只定义类型并声明入口，不应再次手工安装 `KernelOp`。完整示例见 `platforms/axplat-dyn/src/boot.rs`。

启用 SMP 时，内核侧通过 `#[somehal::secondary_entry]` 声明次核继续执行点。`somehal` 自身的 `secondary_entry()` 负责绑定已发布的 CPU-local 区域、切换内核页表并初始化次核中断，然后调用该继续执行点；平台适配层无需复制这些步骤。

### 2.3 分页后初始化

页表可用后调用 `somehal::post_paging()`。该入口执行 `someboot::post_allocator()`，再根据已有 FDT 或 ACPI RSDP 初始化 `rdrive`；没有固件描述时只记录警告并跳过设备探测。

`axplat-dyn` 已在 `InitIfImpl::init_later()` 中调用 `somehal::post_paging()`。设备驱动和运行时适配应依赖这一统一时机，避免在平台入口或单核早期路径提前探测运行期控制器。

## 3. 功能开关

功能开关主要转发给 `someboot`，使最终镜像保持同一套页表、TLS 和虚拟化契约。消费方应只启用目标运行时实际支持的组合。

| 功能 | 行为 |
| --- | --- |
| `efi` | 启用 `someboot/efi` 的 UEFI 启动支持 |
| `mmu` | 启用 `someboot/mmu` 的分页能力 |
| `uspace` | 启用用户空间支持，并隐含 `mmu` |
| `hv` | 启用虚拟化支持，并隐含 `mmu` |
| `tls` | 请求内核 TLS；与 `uspace` 同时启用时由 `uspace` 取得寄存器所有权 |
| `thead-mae` | 启用 `someboot/thead-mae` 的 T-Head MAE 支持 |

`somehal` 的 AArch64 运行时 Clippy 配置使用 `aarch64-unknown-none-softfloat` 目标与 `uspace` 功能。修改功能组合后，应同时检查直接消费方 `axplat-dyn` 的目标配置。

## 4. 验证与排错

从仓库根使用项目任务入口检查受影响软件包，不直接运行原生 Cargo 静态检查：

```bash
cargo xtask clippy --package somehal
cargo xtask clippy --package axplat-dyn
```

运行时行为应通过使用 `axplat-dyn` 的现有 ArceOS、StarryOS 或 Axvisor 定向 QEMU/板卡用例验证。若启动在中断、定时器或设备探测处失败，先确认 `KernelOp` 已在 `mmio_api` 使用前安装、`post_paging()` 在分页后调用，再按目标架构检查对应 `src/arch` 后端。

## 5. 来源、文档与许可

SomeHAL 最初服务于 Sparreal OS，现以 TGOSKits 工作区组件的形式维护。TGOSKits 的版本不再提供 Sparreal 独立仓库路径或 `sparreal_kernel` 示例，集成应使用本仓库的 `axplat-dyn` 和 `cargo xtask` 入口。

进一步资料包括[运行时 HAL 架构说明](../../docs/docs/architecture/platform/somehal.md)、[someboot 组件](../someboot)和[axplat-dyn 适配层](../axplat-dyn)。

本组件通过 `license.workspace = true` 采用 Apache-2.0；完整许可条款见仓库根目录的 [`LICENSE`](../../LICENSE)。
