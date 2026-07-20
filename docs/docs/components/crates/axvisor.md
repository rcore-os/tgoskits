# `axvisor`

> 路径：`os/axvisor`

Axvisor 是 AxVM 的宿主编排程序。它负责读取 VM TOML 与镜像、获取 host platform
snapshot、调用 machine planner、启动创建事务、管理 VM runtime 和提供 shell；资源
分配、device/controller 语义和架构 vCPU core 位于 `virtualization/`。

## 当前模块

| 文件 | 职责 |
| --- | --- |
| `src/main.rs` | 初始化 runtime、platform IRQ、manager 与 shell |
| `src/config.rs` | 配置转换、镜像加载、machine plan、firmware 和 VM 创建 |
| `src/manager.rs` | 批量创建、注册和启动 VM |
| `src/platform_irq.rs` | 注册架构平台 IRQ adapter |
| `src/shell/` | VM 查询与生命周期命令 |
| `src/banner.rs` | 启动输出 |

架构相关 controller、timer、固件和 address-space glue 不放在 Axvisor 本体，而位于
`virtualization/axvm/src/arch/<arch>`。

## 创建流程

每份配置按以下顺序处理：

1. `axvmconfig` 严格解析 `[machine]`、memory 与 devices；
2. Virtual 机型使用空 host snapshot，Passthrough 从 FDT/ACPI 获取 snapshot；
3. `VmMachinePlanner` 生成不可变 plan；
4. 根据 plan 生成 FDT 或 ACPI；
5. 读取 kernel、ramdisk 与外部 boot firmware；
6. 一次性 claim plan 中全部 passthrough device；
7. AxVM 依次构建 RAM、vCPU、controller/binding、device/topology、mapping、firmware 与
   boot state；
8. commit 后注册到 VM manager。

claim、snapshot generation revalidation 或后续构建失败时，RAII lease 会恢复已取得的
ownership。Axvisor 不把半完成 VM 放进 manager。

## 配置

```toml
[machine]
mode = "passthrough"
firmware = "auto"
interrupts_passthrough = false

[base]
id = 1
name = "guest"
cpu_num = 1
phys_cpu_ids = [1]

[[memory.regions]]
guest_base = 0x8000_0000
size = 0x2000_0000
permissions = "rwx"
backing = { kind = "allocate" }

[devices]
disable_defaults = []
deny = []
```

`interrupts_passthrough = false` 使已分配 host IRQ 经过 software-backed input
mediated；`true` 要求它们先取得 host IRQ ownership，再通过 HW-backed virtual
interrupt 转发。该字段只影响物理 source，两种情况都使用同一 VM-local
controller，并都允许虚拟设备的软件 IRQ。Virtual 机型拒绝该字段。

默认 console 由架构 profile 创建：Virtual AArch64 使用 PL011；host 派生的 AArch64
Passthrough VM 按固件选中的 UART 使用 PL011、packed NS16550 或 DW-APB 虚拟替换；
RISC-V/LoongArch 使用 NS16550，x86 使用 COM1。可以用
`disable_defaults = ["console"]` 关闭。其他虚拟设备通过 `[[devices.virtual]]` 显式声明。

## 固件

- AArch64/RISC-V 使用 plan 生成 FDT；
- x86_64 使用 plan 生成 ACPI tables/AML；
- LoongArch64 生成 ACPI 并通过 fw_cfg 提供；
- Passthrough 描述只包含授权或结构性资源，不复制 host AML，也不把完整 host FDT 直接
  暴露给 Guest。

## 构建与验证

Axvisor build/test 使用 xtask：

```bash
cargo xtask axvisor build -c os/axvisor/configs/board/qemu-aarch64.toml --debug
cargo xtask axvisor build -c os/axvisor/configs/board/qemu-riscv64.toml --debug
cargo xtask axvisor build -c os/axvisor/configs/board/qemu-x86_64.toml --debug
cargo xtask axvisor build -c os/axvisor/configs/board/qemu-loongarch64.toml --debug
```

有 Guest 镜像时再用对应 `cargo xtask axvisor test qemu ...` workflow 做 smoke。物理板
ownership、timer/IPI 和设备交接问题需要 board workflow 验证。
