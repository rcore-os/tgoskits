# `riscv_vcpu`

> 路径：`virtualization/riscv_vcpu`
> 类型：库 crate
> 分层：组件层 / RISC-V vCPU core

`riscv_vcpu` 是 RISC-V Hypervisor Extension 的 OS-neutral vCPU core。它只保留 VM entry/exit、CSR 状态保存恢复、SBI 处理、guest page fault 解码、timer pending state 和本 crate 专用的 RISC-V exit/error/value types。AxVM/ArceOS 的 trait 实现、错误转换、设备策略和运行时 IRQ 处理都位于 `virtualization/axvm/src/arch/riscv64`。

## 设计边界

- `RiscvVcpu<H>` 通过 `RiscvHostOps` 获取 host VA 到 PA 的转换能力。
- `RiscvPerCpu` 提供 inherent per-CPU API，不直接实现 AxVM traits。
- `RiscvVmExit` 是 core 输出的 OS-neutral exit，覆盖 hypercall、MMIO、nested page fault、external interrupt、CPU up/down、halt、system down 和 nothing。
- 非 `riscv64` target 下 crate 根使用 `#![cfg(target_arch = "riscv64")]`，等价为空 crate；消费者必须使用 target-specific dependency。

## 主要模块

| 模块 | 作用 |
| --- | --- |
| `types.rs` | 本地 error/result/exit/address/width/flags/nested-paging 类型 |
| `host.rs` | `RiscvHostOps` host capability boundary |
| `vcpu.rs` | vCPU 生命周期、SBI 分发、VM exit 解码 |
| `percpu.rs` | 每核 H 扩展 CSR 初始化和能力查询 |
| `registers.rs` | `hgatp`、guest page fault 地址和 delegation mask 的 typed helpers |
| `regs.rs` | GPR、VS CSR、virtual HS CSR 和 trap CSR 状态 |
| `guest_mem.rs` | HLV/HV guest memory helper 和 guest instruction fetch |
| `trap.rs` + `trap.S` | 进入/退出 guest 的汇编入口和 trap 定义 |

## AxVM 接入

AxVM 的 RISC-V adapter 包装 core 类型：

- `AxvmRiscvHostOps` 实现 `RiscvHostOps`。
- `AxvmRiscvVcpu(RiscvVcpu<AxvmRiscvHostOps>)` 实现 `VmArchVcpuOps<Exit = RiscvVmExit>`。
- `AxvmRiscvPerCpu(RiscvPerCpu)` 实现 `VmArchPerCpuOps`。
- `Riscv64Arch::handle_vcpu_exit_bound` 直接 match `RiscvVmExit`，MMIO、nested page fault、external interrupt、CPU up/down 和 shutdown policy 都在 AxVM adapter 内完成。

`riscv_vplic` 仍是独立设备/中断组件，本次边界不把它并入 `riscv_vcpu`。

## 行为重点

- `set_nested_page_table()` 验证 Sv39x4/Sv48x4 mode 和 16KiB root alignment，然后通过 typed `hgatp_value()` 编码 `hgatp`。
- `bind()`/`unbind()` 保存恢复 VS CSR、virtual HS CSR、`hgatp` 和 guest pending interrupt state。
- SBI HSM、reset、debug console、PMU 和 RFNC 分支在 core 内解码；host-visible 动作以 `RiscvVmExit` 返回。
- guest load/store page fault 尽量解码为 `MmioRead`/`MmioWrite`；无法解码时返回 `NestedPageFault`，由 AxVM adapter 决定是否补页或放弃。
- `inject_interrupt()` 当前支持 VS external interrupt 注入，不支持的 vector 返回 `RiscvVcpuError::Unsupported`。

## 依赖关系

`riscv_vcpu` 不依赖 AxVM/ArceOS crate。核心依赖包括：

- `riscv` / `riscv-h`
- `riscv-decode`
- `rustsbi` / `sbi-rt` / `sbi-spec`
- `bitflags`
- `tock-registers`
- `memoffset`

## 验证

建议从 workspace root 执行：

```bash
cargo test -p riscv_vcpu --tests
cargo check -p riscv_vcpu --target riscv64gc-unknown-none-elf
cargo xtask clippy --package riscv_vcpu
cargo xtask clippy --package axvm
cargo xtask axvisor test qemu --arch riscv64 --test-group normal --test-case smoke
```

若修改 `VmCpuRegisters` 字段顺序或汇编偏移，必须同步检查 `trap.S` 和 `memoffset` 生成的偏移常量。
