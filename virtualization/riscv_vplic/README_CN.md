<h1 align="center">riscv_vplic</h1>

<p align="center">每 VM 独立的软件 RISC-V PLIC</p>

[English](README.md) | 中文

`riscv_vplic` 是面向 hypervisor 的 `no_std + alloc` PLIC 1.0.0 设备模型。每个 `VPlicGlobal` 独立保存一个 VM 的 priority、enable、pending、active、source level、threshold 和 claim/complete 状态。

该 crate 不解引用 host PLIC MMIO，也不假设 host 与 guest PLIC 地址相同。平台 IRQ adapter 负责物理 IRQ ownership 和路由，并通过 AxVM 中断拓扑触发已经分配的 vPLIC 输入。

## 功能

- PLIC priority、pending、enable、threshold 与 claim/complete 寄存器
- 每 VM、每 context 独立状态
- edge/level 输入，以及 level 在 complete 后重新 pending
- 显式的每 VM source ownership；未分配位为 RAZ/WI
- 构造、MMIO、context 和 source 的结构化错误

## 使用

```rust
use axvm_types::GuestPhysAddr;
use riscv_vplic::VPlicGlobal;

let plic = VPlicGlobal::new(
    GuestPhysAddr::from(0x0c00_0000),
    Some(0x40_0000),
    2,
)?;
plic.restrict_to_assigned_sources();
plic.assign_source(10)?;
plic.set_source_level(10, true)?;
# Ok::<(), riscv_vplic::VplicError>(())
```

AxVM 总是根据 `VmMachinePlan` 安装显式 source assignment。物理 IRQ ownership 和 host claim/complete 属于 host adapter，不属于该 crate。

## 验证

```bash
cargo fmt --all
cargo xtask clippy --package riscv_vplic
cargo test -p riscv_vplic --all-features
RUSTDOCFLAGS="-Dwarnings" cargo doc -p riscv_vplic --no-deps
```

许可证：Apache-2.0。
