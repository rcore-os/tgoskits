# Axvisor 共享固件 Provider 仲裁设计

本文定义 AArch64 透传型客户机如何继续访问同时被宿主虚拟设备依赖的物理固件 provider。它是 machine planning、driver capability、`DeviceRuntime` 与 stage-2 构建必须遵守的 MMIO 所有权和失败契约。

本文与合入最新 `origin/dev` 后的当前实现同步；实现契约变化时必须在同一变更中更新本文。

## 问题与成功标准

基于 FDT 的 Axvisor machine 会把固件选定的物理 UART 替换为虚拟 UART，同时保持相同的客户机地址和中断身份。物理 UART 及其 clock 继续由宿主所有。透传型客户机仍从可分配物理地址空间的恒等映射开始；除非其他资源在 stage-2 中打洞，否则该范围会包含共享 clock/reset unit。

物理 UART 节点被替换后，Linux 不再看到原 clock 的 consumer。此时 `clk_disable_unused` 可以写共享 provider，并在客户机继续运行时 gate 宿主 UART。RK3568 上，这会在客户机启动期间稳定导致宿主控制台输出消失。加入 `clk_ignore_unused` 后启动能够继续，这证明了所有权违规，但不是可接受的修复。

实现满足以下条件时才算成功：

- 物理 UART 无论显式选择还是默认分配都不可提供给客户机；
- 客户机仍能观察并使用共享 provider 中无关的功能；
- 能够 gate、reparent 或破坏宿主 UART clock 的写入被过滤；
- 完整 provider MMIO 范围不出现在透传 stage-2 mapping 中，而是由一个 `DeviceRuntime` device 提供；
- AxVM runtime access path 中不出现板卡名称、SoC compatible 字符串或寄存器常量；
- 未知或有歧义的 mutable provider 会阻止 VM 构建，而不是退回未仲裁透传；
- RK3568 与 RK3588 均能在没有 `clk_ignore_unused` 的情况下启动。

## 范围与非目标

首个 consumer 是 RK3568 与 RK3588 上由宿主选定 UART 的 clock dependency。契约本身保持 provider-neutral，以便 reset 或 power-domain driver 后续暴露等价的保护 capability。

本设计不：

- 向客户机暴露物理 UART；
- 模拟完整 clock tree，或允许客户机配置宿主 UART；
- 接受来自客户机 TOML 或其他不可信输入的寄存器规则；
- 在 AxVM 中根据板卡名称或通用 clock ID 推断保护规则；
- 支持带多个 MMIO range 的 mutable provider，或支持非 single-cell clock selector，除非 typed provider capability 明确描述这些形状；
- 增加 big-endian 客户机 MMIO 语义。当前 AArch64 Axvisor target 和 Rockchip provider 都是 little-endian。

Fixed clock 及其他没有 mutable MMIO 的 provider 不需要 mediator。

## 替代方案

`clk_ignore_unused` 是客户机内核 workaround，其他 clock 写入仍可能关闭宿主 UART。完全从客户机移除 provider 可以保护宿主，却会破坏无关的透传设备。完整 shadow clock controller 会复制庞大的硬件专用状态机，仍然需要定义 host-owned leaf policy。在 AxVM 中用 RK3568/RK3588 常量过滤写入可以解决已观察到的症状，却会把平台 policy 引入 VM hot path。

最终方案把硬件寄存器知识保留在 Rockchip clock driver 中，通过 typed `rdif-clk` capability 转换，并复用现有事务化 device runtime 管理资源所有权和 dispatch。

## 不可变 Machine Identity

Serial FDT parser 会为每项 `clocks` 引用保留：

- provider phandle；
- 完整 clock specifier cell；
- provider `reg` region。

这些数据在固件仍可访问时完成校验，并成为 `GuestSerialFdtIdentity` 的一部分。客户机可见 UART 节点使用虚拟 fixed clock，但原始引用作为内部 machine-plan 证据保留。它们不是配置字段，TOML 不能覆盖。

Provider 缺少 `reg` 表示没有需要仲裁的 mutable MMIO。当前 mutable provider 必须恰好包含一个 region 和一个 selector cell。缺失 phandle、malformed `#clock-cells`、截断的 specifier、无效 region 或有歧义的形状均返回错误。

## Typed Provider Capability

`rdif-clk` 针对请求的 clock ID 暴露不可变的 assignment protection rule：

- `None` 表示 provider 无法安全仲裁该 assignment；
- 空列表表示没有需要保护的 mutable MMIO state；
- 非空列表定义全部 provider-owned protected write。

初始规则包括：

- `Deny { offset, length }`：抑制所有重叠写入；
- `MaskedWrite32 { offset, value_mask, write_enable_mask }`：只接受对齐的 32-bit write，在转发剩余部分前同时移除受保护 value bit 及对应 write-enable bit。

规则由硬件 driver 生成，并在 VM 可运行前相对 provider region 完成校验。规则必须使用 provider-relative offset、非空 range、对齐的 masked register、互不重叠且非零的 mask，以及有界算术。

Rockchip high-half write-mask register 允许转发无关字段，而不需要软件 read-modify-write lock。Fractional divider register 作为整体拒绝访问，因为 numerator 与 denominator 构成不可分割的 clock configuration。

## dyn 模型、资源与访问流程

Provider resolution 在架构资源构建期间的 task context 中执行：

1. 对 `Virtualized` 客户机，AArch64 planner 忽略这些引用；
2. 对 `Passthrough` 客户机，通过 `rdrive` 解析 provider phandle；
3. 通过 typed clock capability 请求 protection rule；
4. 合并并去重同一 provider 的引用；
5. 为每个 provider 创建一个持有已校验 plan 的 `Arc<dyn DeviceModel>`；
6. 把该模型作为 `HostReplacement` 节点加入 AArch64 设备图；
7. 同一个模型在 `build()` 中映射物理 provider 并返回 `SharedMmioDevice`；
8. device claim 完整 provider MMIO range；
9. 地址 planner 把该资源视为 emulated-device hole，因此 stage-2 passthrough mapping 不与其重叠。

模型直接持有拥有所有权的 provider region 和 protection rule，不保存原始 TOML，也不依赖设备类型枚举或中心 factory lookup。`requirements()` 只声明完整 provider 固定 MMIO slot；`build()` 必须消费并核对该 slot 后才映射硬件。后续构建失败时，`DeviceBundle` 与资源 lease 会一起原子回滚。

读取按原 width 转发。写入先检查 range 与 alignment，再由不可变规则过滤，最后转发或抑制。Runtime path 不执行 driver lookup、不分配、不查找 VM、不调用 callback，也不获取 provider lock。

## 并发与生命周期

模型构建完成后，provider rule 与 mapped region 均不可变。多个 vCPU 可以并发调用 MMIO device。Rockchip masked write 仍是原子的硬件操作，被拒绝的 register 永不到达硬件；没有 shadow clock state，也没有第二条 pending queue。

Host clock driver 仍可通过自身 typed operation 访问同一个物理 provider。因此，安全共享依赖不需要软件 read-modify-write transaction 的硬件操作。未来 provider 如果需要串行化，必须把该串行化能力作为 runtime capability 暴露，而不是在 AxVM 中增加全局锁。

停止或销毁 VM 会 drop device runtime 及其 mapping。物理 provider 继续由 host driver 所有；客户机生命周期操作绝不 reset 或 disable 它。

## 失败策略

Planner 会拒绝：

- mutable provider 没有注册 typed capability；
- provider region/specifier layout 不受支持；
- protection rule 无效或越界；
- 同一 phandle 对应不一致的 region；
- resolved fixed slot 与已校验 provider region 不一致；
- 物理 provider 映射失败。

上述情况不存在 raw-passthrough fallback。安全的 fixed provider 通过缺少 mutable provider region 表达，而不是忽略错误。

## 验证

确定性回归为：

```text
cargo test -p axvm --no-default-features --features host-test \
  shared_mmio::tests::strips_rk3568_uart2_gate_disable_write -- --exact
```

实现 filter 前，真实 RK3568 `0x0009_0009` gate-disable write 会被转发；同一个测试现在会抑制它。其他单元测试覆盖无关 bit 转发、partial/denied write、对未保护 register 的零写、资源身份，以及 MMIO read/write 转发。Rockchip 测试固定 RK3568 与 RK3588 UART2 的完整 gate、selector、mux 和 fractional-divider rule。FDT 测试覆盖 multi-clock 解析与 malformed specifier。设备图的 replacement range 验证证明每个共享 provider 完整范围都会从 passthrough mapping 中扣除。

集成验证必须包括：

- AArch64 Axvisor target build；
- QEMU GICv2 与 GICv3 timer stress，防止破坏相邻的 timer 工作；
- RK3568 连续三次启动到达配置的客户机 marker，且没有 host-time jump 或控制台丢失；
- RK3588/OrangePi-5-Plus 在没有 `clk_ignore_unused` 时启动；
- `rdif-clk`、`rockchip-soc`、`ax-driver`、`axvm-types` 与 `axvm` 的 formatter 和定向 Clippy 检查。
