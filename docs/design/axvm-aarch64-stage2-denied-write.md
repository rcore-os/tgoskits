# AArch64 客户机只读映射异常

AxVM 在 AArch64 上把 stage-2 写权限错误转化为客户机可观察的同步外部数据中止，由客户机内核的既有异常路径处理拒写；其他异常仍失败封闭并停止 VM。

## 1. 问题与边界

AxVM 的 ivshmem BAR2 将状态表和其他 peer 的输出节映射为 stage-2 只读页。`IvshmemDirectPlan::derive()` 决定权限，`AxStage2Remap::update()` 安装映射。Linux 用户进程尝试写这些页时，EL2 收到 stage-2 permission fault；目前 `handle_data_abort()` 返回 `ArmVcpuError::Unsupported`，`ArmVcpu::run()` 因而终止整个 VM。合并套例 `apps/linux/ivshmem/suite/main.c` 的 `expect_denied_write()` 需要客户机收到异常、保持原有数据不变，再继续轮询和 MSI-X 阶段。现有运行在两个 VM 上分别于 IPA `0x0c000000`、`0x0c000008` 触发写入权限错误，均在套例的 `profile` 检查后停机。

### 1.1 必须保留的保证

写入不能绕过 stage-2 权限，也不能只把客户机 PC 跳过出错指令。拒绝写入后共享字节不变，客户机内核按其既有异常和信号路径处理失败；宿主不因这一预期访问终止整个 VM。异常注入不得用于 MMIO 翻译故障、无法解析的 IPA、stage-1 page-table walk 故障或无法验证执行状态的退出。错误分类必须留在 AArch64 客户机边界，不能由 ivshmem 端点自行伪造 Linux 信号。

### 1.2 现有调用链

`ax_cpu::virtualization::enter_guest()` 保存 `Exit`，包含客户机虚拟地址 `fault_address`、ESR 和经 stage-1 解析的 `guest_address`。`ArmVcpu::vmexit_handler()` 将它交给 `handle_exception_sync()`；当前 `handle_data_abort()` 仅将 stage-2 翻译错误转换为 MMIO 退出。客户机 EL1 的 `VBAR_EL1`、`ESR_EL1`、`FAR_EL1`、`ELR_EL1` 和 `SPSR_EL1` 由 `GuestSystemRegisters` 保存和恢复，返回模式及返回 PC 位于 `GuestContext`。注入只应操作该 vCPU 的已保存上下文，不新增全局待处理异常队列。

## 2. 方案与审查点

参照 Linux v7.0 [`arch/arm64/kvm/inject_fault.c`](https://github.com/torvalds/linux/blob/v7.0/arch/arm64/kvm/inject_fault.c) 的 `inject_abt64()`：向客户机报告同步外部数据中止，而不是把 EL2 stage-2 fault syndrome 原样当作客户机 stage-1 权限错误。EL0 写入需使用 lower-EL AArch64 同步异常向量，`FAR_EL1` 填原始客户机虚拟地址，`ELR_EL1` 保留出错指令地址，`SPSR_EL1` 保留原客户机 PSTATE，`ESR_EL1` 使用正确的 Data Abort/External Abort 编码；恢复执行时使用屏蔽异常的 EL1h PSTATE。只有确认 `Exit` 为 stage-2 permission fault、是写入、不是 S1PTW，且来源为 AArch64 EL0t 时，才进入此路径；其他异常仍失败封闭。`GuestSystemRegisters::inject_el0_external_data_abort()` 校验 EL1 向量基址并在修改状态前决定是否支持注入。KVM 对 S1PTW、AArch32、SCTLR2 异步异常和嵌套虚拟化有额外分支，本次不推断这些场景已受支持。

### 2.1 替代方案与所有权

删除 `expect_denied_write()` 无法证明拒写后状态保持，不可采用。把 BAR2 改为可写违反 peer 隔离。让 VM 停止只证明宿主拒绝访问，不能让同一个 VM 完成后续协议。最小扩展是在现有 AArch64 vCPU 异常路径中实现客户机可观察的中止，机器寄存器由 `ax_cpu` 的客户机上下文独占，AxVM 负责决定何时注入；不增加设备私有的异常机制或 Linux 专属行为。若失败或目标模式不受支持，仍停止 VM，不能回退到普通 MMIO 写入。

### 2.2 验收、回滚与风险

合并套例在原实现上确定失败：两个 Linux peer 写入 BAR2 只读 section 后，AxVM 以 `Unsupported` 停止 VM。注入草稿在同一 QEMU 套例中通过 `deny-write-state`、`deny-write-output`，并由客户机检查写入前后共享字节一致；单纯输出成功文字不足以证明拒写。用上游 v0.0.15 的 Linux 镜像、同版本 `axvisor.ko` 和 ArceOS peer 运行后，三 peer 轮询交换、UIO 绑定、MSI-X 双向门铃以及最终 `IVSHMEM_PCI_SUITE_PASSED` 均通过。`cargo xtask clippy --package ax-cpu`、`cargo xtask clippy --package axvm` 和全量 `cargo xtask test` 通过。当前没有单独可运行的 AArch64 CPU 状态转换单元测试；已知的架构遗留点是 `Exit` 的 PC 与注入后的 `GuestContext` PC 为同一机器来源的两个已保存拷贝，后续应把访问集中到同一个 AxCPU 表示。

改动不迁移持久状态；回滚注入会恢复 VM 遇拒写即停止的旧行为，套例将重新标红。错误向量、地址或跨 vCPU 状态污染可能导致客户机内核崩溃或权限处理错误，回归测试需同时覆盖拒绝未初始化 `VBAR_EL1`、非 EL0t 及 S1PTW 的失败路径。
