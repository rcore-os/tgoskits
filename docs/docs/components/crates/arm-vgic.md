# `arm_vgic`

> 路径：`virtualization/arm_vgic`

`arm_vgic` 是 `no_std`、GICv3-only、每 VM 独立的中断控制器领域 crate。它按
Distributor、每 vCPU Redistributor/CPU Interface 和可选 ITS/LPI 域建模，不负责
AxVM 的 MMIO 注册、host IRQ 解析、timer、Guest memory 映射或调度。

只实现 Group 1 Non-secure。GICv2、Secure Group 0/1、GICv4 vPE、nested
virtualization 和 guest 直接访问共享 host GITS 均明确不支持。

## 稳定 API

主要出口为：

- `GicV3Controller`：每 VM 控制器与所有权边界；
- `GicV3Config`：一次性校验 GICD/GICR/GITS、vCPU、LR 和 ITS 预算；
- `GicV3Backend`：受检的软件或物理后端；
- `GicV3VcpuBinding`：Redistributor 和 CPU Interface 生命周期；
- `IntId::{Sgi, Ppi, Spi, Lpi}` 及 affinity、priority、ITS ID 强类型。

旧 `Vgic`、`VGicD` downcast、GICv2 路径、全局 callback、全局 ITS/LPI 状态、crate
内 timer 和手动 inject API 已删除。

## 构造顺序

```rust
use std::sync::Arc;

use arm_vgic::{
    GicAffinity, GicV3Config, GicV3Controller, GicV3MmioRegion, GicV3Mode,
    GicV3VcpuWake, GicVcpuId, SoftwareGicV3Backend, VgicResult,
};

struct Wake;

impl GicV3VcpuWake for Wake {
    fn wake(&self) -> VgicResult {
        Ok(())
    }
}

fn build() -> VgicResult<(GicV3Controller, arm_vgic::GicV3VcpuBinding)> {
    let config = GicV3Config::new(
        GicV3Mode::Emulated,
        GicV3MmioRegion::new(0x0800_0000, 0x1_0000)?,
        GicV3MmioRegion::new(0x080a_0000, 0x2_0000)?,
        0x2_0000,
        1,
    )?;
    let controller = GicV3Controller::new(config, Arc::new(SoftwareGicV3Backend))?;
    let binding = controller.attach_vcpu(
        GicVcpuId::new(0),
        GicAffinity::new(0, 0, 0, 0),
        Arc::new(Wake),
    )?;
    Ok((controller, binding))
}
```

VM 集成层先创建 vCPU 和 controller，再为每个 vCPU 建立 binding，最后才连接设备
输入。binding 必须与 vCPU 同生命周期；Drop 会释放 Redistributor ownership，使创建
失败能够回滚。

## Emulated

Emulated 模式按 VM 保存：

- GICD 与每 vCPU GICR 状态；
- SGI/PPI/SPI/LPI 的 enable、pending、active、priority、trigger 与 route；
- ICH_HCR、VMCR、APR 与全部 LR；
- 软件 pending/refill 状态。

LR 满时中断保留在软件 pending 中，由 maintenance/refill 继续投递，不会 panic。
SGI 根据 affinity、IRM 和 target list 进入目标 Redistributor；PPI 只属于一个 vCPU；
SPI 从 controller input 进入 Distributor。

软件 ITS 需要 `new_with_guest_memory` 提供 VM-scoped、受检 `GuestMemory` capability。
命令队列有环形边界与单次预算，支持 MAPD、MAPC、MAPTI/MAPI、MOVI、INT、CLEAR、
DISCARD、INV、INVALL 和 SYNC。MSI 按 `(DeviceId, EventId) -> LPI -> Collection ->
Redistributor` 投递，不假设 GPA=HPA。

## Passthrough

Passthrough 模式只接受显式 ownership：

- `bind_physical_spi` 绑定 Guest SPI、host IRQ 和固定 vCPU/pCPU affinity；
- `bind_physical_msi` 绑定 VM 隔离的 device/event/collection/LPI；
- 无 host IRQ identity 的软件输入返回 `Unsupported`，不会回退到 LR。

SPI bind 只预留 ownership。目标 binding 首次 load 后，backend 才保存 host line 的
group、priority、trigger、route、pending、active 和 enable 快照并应用 Guest 状态；
save 时屏蔽 Guest line，Drop 时恢复 host 快照。

GICD/GICR MMIO 始终 trap。host-owned 位为 RAZ/WI，Guest 只能写本 VM 已分配的位；
混合位图写不会覆盖 host 配置。`IROUTER` 只接受 VM 的固定物理 affinity，Group 固定
为 Group 1 Non-secure。

私有中断同样按 ownership mask 切换。AxVM 从 host IPI、timer capability、maintenance
IRQ 与 Guest FDT timer role 自动建立 mask；timer/IPI/maintenance 不要求 TOML 重复
填写。binding load 恢复 Guest timer PPI，save 恢复 host snapshot，并在完整运行窗口中
保持 host IRQ 屏蔽。

没有隔离物理 ITS capability 时，Guest 不会看到物理 GITS，也不能访问 host ITS MMIO。

## 并发和错误

状态转换在锁内生成 delivery action，锁外再 wake vCPU 或调用 backend，避免持锁回调
与嵌套 controller lock。backend 必须校验地址、宽度、对齐、IRQ identity、affinity 和
资源 ownership。

所有生产 API 使用 `VgicResult<T>`。`VgicError` 可区分非法 INTID/类别、访问宽度与
范围、状态转换、Guest memory、ITS 命令/预算、资源冲突、backend failure 和
unsupported capability。架构规定的 RAZ/WI 寄存器读零或忽略写；Guest 可触发路径不
依赖 panic、unwrap 或 todo。

## AxVM 集成

AxVM 把 `GicV3Controller` 注册为 `InterruptTopology` controller，并在设备创建前绑定
vCPU port。虚拟 PL011 和 timer 只拿到 controller 创建的 `IrqLine`；物理 IRQ adapter
连接 controller input。设备不会接触 vCPU，也不会调用 inject 函数。

CNTP adapter 位于 `axvm/src/arch/aarch64/timer`。每 vCPU 持有自己的 PPI line，并用
generation token 取消过期回调，避免把 timer 投递给回调发生时的“当前 vCPU”。

## 验证

```bash
cargo test -p arm_vgic --all-features
cargo xtask clippy --package arm_vgic
RUSTDOCFLAGS="-D warnings" cargo doc -p arm_vgic --all-features --no-deps
```
