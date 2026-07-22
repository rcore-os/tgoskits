# arm_vgic

`arm_vgic` 是一个 `no_std`、每 VM 独立的 Arm GICv3 控制器领域 crate。它按
GICv3 的物理结构建模 Distributor、每 vCPU Redistributor/虚拟 CPU Interface，
以及可选的 ITS/LPI 域。MMIO 映射、host IRQ 发现、guest memory、定时器和 vCPU
调度均留在 VMM 适配层，并通过受检 capability 接入。

当前只支持 GICv3 Group 1 Non-secure；不支持 GICv2、Secure Group 0/1、GICv4
vPE 和 nested virtualization。

## 构造流程

先用 `GicV3Config` 一次性校验 GICD、GICR、可选 GITS、vCPU 数量、LR 数量和
命令预算，再创建 `GicV3Controller`。设备连接中断源之前，必须用
`attach_vcpu` 为每个 vCPU 建立 `GicV3VcpuBinding` 和固定 affinity。

Guest ITS 必须通过 `new_with_guest_memory` 提供 VM 级 `GuestMemory`
capability。ITS 只按受检 GPA 读取有预算上限的环形命令队列，不假设 GPA=HPA。

## SPI 所有权与 backing

`GicV3SpiOwnership` 只描述 guest 可见的 Distributor 资源：

- `AllGuestOwned` 用于全虚拟机型，全部已实现 SPI 均属于 guest；
- `Explicit` 在软件 endpoint 或物理 binding 认领 SPI 前保持 RAZ/WI，混合位图写入
  不会修改未认领的 host SPI。

每个 endpoint 独立选择 backing。`configure_spi_input` 认领软件 SPI；
`bind_physical_spi` 绑定已取得 ownership 的 host IRQ 和固定 vCPU affinity。两者可在
同一个控制器与 CPU Interface 中共存：软件事件使用普通虚拟 LR，物理事件使用
HW-backed LR。硬件转发仍会退出到 EL2，并非静态 CPU/device bypass。物理 binding
由真实电气线路驱动，不能通过软件 `IrqLine` 拉高。

物理 SPI 绑定阶段只预留所有权，不修改 host 硬件；平台 backend 可在 guest enable
时保存并交接已释放线路，销毁时必须恢复快照。物理 MSI 同样按
`(DeviceId, EventId)` 单独选择；连接为软件 endpoint 的事件继续走软件 ITS。

SGI/PPI 始终属于 VM 本地状态。`GicV3VcpuBinding::{load, save, synchronize}` 保存完整
虚拟 CPU Interface，并支持软件与 HW-backed LR 混合。LR refill 会重建有界工作集，
优先选择 pending，而不是让 active 中断永久占据 LR；溢出项会保留完整的
pending/active 状态和 backing，并通过 `ICH_HCR_EL2` 的 NPIE、LRENPIE、UIE 请求再次
同步。

EOImode 0 的溢出通过 `ICH_HCR_EL2.EOIcount` 回收；EOImode 1 必须 trap
`ICC_DIR_EL1` 并路由到 `GicV3VcpuBinding::deactivate`。该操作会先收割实时 ICH 状态，
再在同一个生命周期事务中执行 DIR 和 refill，不依赖外层 run loop 预先复制硬件
Pending-to-Active 转换。溢出的 HW-backed 项仍通过受检物理 backend 完成
deactivation，不会退化成软件中断。AArch64 适配层在 `ICH_VTR_EL2.TDS` 表示支持时
使用专用 `TDIR` trap；不支持时走架构规定的 common CPU Interface trap
（`ICH_HCR_EL2.TC`），并模拟 `ICC_DIR_EL1`、`ICC_CTLR_EL1`、`ICC_PMR_EL1` 和
`ICC_RPR_EL1`，因此不同 CPU 上保持相同的溢出和 EOImode 语义。

backend 必须校验平台 IRQ identity、affinity、地址、访问宽度和所有权。控制器在
锁内只生成投递动作，释放锁后才唤醒 vCPU 或调用 backend。

## 错误语义

所有可失败 API 返回 `VgicResult<T>`。`VgicError` 可区分非法 INTID、错误 INTID
类别、非法寄存器访问、状态转换、guest-memory 访问、ITS 命令或预算、资源缺失/
冲突、不支持能力和 backend 失败。架构规定的未知 RAZ/WI 寄存器读零/忽略写；
非法宽度、对齐、范围和所有权均显式报错。

## 破坏性 API 变化

新的 GICv3 API 直接替换旧 `Vgic`/GICv2、全局 host callback、全局 ITS/LPI
状态、crate 内定时器和手动硬件注入函数。集成层现在必须注册
`GicV3Controller`、绑定 vCPU，并让设备持有控制器创建的有线或 MSI endpoint。
虚拟 CNTP 定时器属于 VMM，每 vCPU 应持有自己的 PPI 中断线。

## 验证

```bash
cargo fmt --all --check
cargo clippy -p arm_vgic --all-targets --all-features -- -D warnings
cargo test -p arm_vgic --all-features
RUSTDOCFLAGS="-D warnings" cargo doc -p arm_vgic --all-features --no-deps
```

本项目使用 Apache-2.0 许可证。
