# AxVM RISC-V SBI IPI 分层设计

## 背景与目标

RISC-V guest SMP 启动和核间调度依赖 SBI IPI。SBI IPI 的调用约定、hart mask
以及 VSSIP 状态是 RISC-V vCPU 协议的一部分；guest hart 拓扑属于 VM；中断队列、
目标 vCPU 唤醒和宿主 IPI 则属于 AxVM 的通用运行时。如果把这些职责放进公共
`ArchOps`，公共层就会出现 RISC-V `cfg`、协议字段和路由分支，后续架构能力也会继续
向同一接口堆叠。

本设计只实现 RISC-V guest SBI IPI，不改变宿主 IPI、vPLIC、其他 guest 架构、
StarryOS syscall 或启动流程。成功标准是三核 Linux guest 能发现 SBI IPI extension、
启动至少三个 CPU，并观察到非零 IPI 计数。

规范依据：

- [SBI IPI Extension](https://github.com/riscv-non-isa/riscv-sbi-doc/blob/master/src/ext-ipi.adoc)
- [SBI Legacy Extensions](https://github.com/riscv-non-isa/riscv-sbi-doc/blob/master/src/ext-legacy.adoc)

## 方案选择

| 方案 | 结论 | 原因 |
| --- | --- | --- |
| 在 `ArchOps` 增加 IPI 方法，并在公共实现中加入 RISC-V `cfg` | 不采用 | 把 guest SBI 协议泄漏到公共架构接口，每增加一个可选架构能力都会扩大公共 trait |
| 当前 vCPU 直接写 HVIP，远端 vCPU 走发送队列 | 不采用 | 同一事件有两套顺序、唤醒和 drain 语义，难以证明当前核与远端核行为一致 |
| vCPU 协议层产出类型化请求，RISC-V AxVM 层解析拓扑，通用 sender 发布 | 采用 | 各层只拥有自己的事实源，并复用现有中断发布与唤醒路径 |

不另建 SBI IPI crate。当前协议规模不足以支撑新的发布单元，而 `riscv_vcpu` 已经拥有
SBI ECALL 与 vCPU 中断状态。

## 所有权边界

### `riscv_vcpu`

`riscv_vcpu` 拥有以下 RISC-V 协议事实：

- `RiscvIpiRequest` 中的 `hart_mask`、`hart_mask_base` 和
  `RiscvIpiAbi::{Legacy, SbiV02}`；
- SBI v0.2 IPI extension、legacy `SEND_IPI`/`CLEAR_IPI` 的解码；
- legacy/SBI v0.2 返回寄存器的差异，以及 ECALL PC 只推进一次的约束；
- 保存的 HVIP 和当前绑定硬件 CSR 中 VSSIP、VSTIP、VSEIP 的一致性。

请求字段私有，只提供只读访问器。AxVM 不能自行拼装协议请求，也不需要知道返回寄存器。
`CLEAR_IPI` 只清除当前 vCPU 的 VSSIP，不改变 timer、external 或其他 vCPU 状态。

### AxVM RISC-V 层

`virtualization/axvm/src/arch/riscv64/ipi.rs` 拥有 SBI hart 到 AxVM vCPU 的解释：

- `hart_mask_base == usize::MAX` 表示所有已配置 guest hart；
- 零 mask 是成功的空操作；
- 普通 mask 的每个置位通过 `base + bit` 得到 guest hart ID；
- guest hart ID 通过 crate-private `VmArchCpuIdResolver::vcpu_id_for_arch_cpu_id` 查询映射为
  vCPU ID。

该 capability trait 在正常的 `architecture::cpu_up` 模块中为 `AxVM` 实现，CPU-up 与 IPI
共用同一查询，配置三元组不再由调用方分别拆解。RISC-V 路由实现保持在私有架构模块中，
不向公共 `arch/mod.rs` 或 `architecture/ops.rs` 暴露 hart-mask helper。

### AxVM 通用中断运行时

`VmInterruptSender`、`PendingVcpuInterrupt` 和目标运行时拥有队列、唤醒与宿主 IPI。
VSSIP 以 level-triggered `VirtualInterruptId(1)` 发布。当前 vCPU 和远端 vCPU 都先进入
同一队列，再由目标侧 drain 调用 vCPU 中断注入；RISC-V 层不直接写当前 vCPU 的 HVIP。

## 数据流

1. guest 执行 legacy `SEND_IPI` 或 SBI v0.2 `send_ipi` ECALL。
2. `riscv_vcpu` 解码参数，推进一次 PC，返回 `RiscvVmExit::SendIpi(request)`；此时不预写成功。
3. AxVM RISC-V 层先解析并验证完整目标集合。
4. 验证成功后，为每个目标通过 `VmInterruptSender` 发布 level-triggered VSSIP。
5. 目标运行时入队、唤醒并 drain，`riscv_vcpu` 更新保存的 HVIP；若该 vCPU 当前绑定，
   同时更新硬件 HVIP CSR。
6. AxVM 把整体投递结果交给原 vCPU 的 `complete_ipi`，由协议层写回对应 ABI 的返回值。

## 错误与原子性语义

- 普通 mask 的 `base + bit` 溢出，或任一 guest hart 未配置，返回
  `SBI_ERR_INVALID_PARAM`。
- 目标解析在发布前完成，因此参数错误不会产生部分投递。
- 目标解析成功后的入队或唤醒失败返回 `SBI_ERR_FAILED`。此前已发布的软件中断不可可靠
  回滚，调用方不能把运行时失败理解为没有任何目标观察到中断。
- legacy mask 在本次 RV64、最多 64 个 guest hart 的边界内只读取一个 XLEN word；
  guest 指针短读或不可读返回失败。
- 零 mask 不投递中断并返回成功。

## 锁与并发边界

目标集合解析发生在 vCPU 退出处理的 task context，可以构造临时 `Vec`。解析期间不持有
目标 vCPU 的运行时锁，也不发布事件。发布阶段沿用 `VmInterruptSender`：运行时注册表查找、
目标队列更新和唤醒按现有窄临界区顺序完成，不在广域锁内调用目标 vCPU 后端。

HVIP 的保存副本由 `riscv_vcpu` 独占；只有当前绑定到硬件的 vCPU 才同步写 CSR。这样迁移、
解绑和重新绑定仍以保存状态为事实源，不要求 AxVM 路由层持有 vCPU 内部锁。

## 验证与回滚

回归只保留 `linux-smp3-ipi.toml`，删除不再提供额外覆盖的 RISC-V QEMU 单核配置。三核
配置为新内核提供 128 MiB guest RAM，避免 64 MiB 配置在初始化驱动前只剩约 15 MiB
可用内存而失去确定性。QEMU 用例同时检查 `nproc >= 3`、SBI IPI extension 日志和
`/proc/interrupts` 非零 IPI 计数，最终输出唯一标记 `guest smp ipi pass!`。

最低层行为回归直接编译 RISC-V 生产模块，并通过 RISC-V musl test binary 在
qemu-user 中执行。跨架构 crate test 的 musl target、静态链接、linker 和 qemu-user
runner 统一由 axbuild 的 `cross-test` 命令解析，不把工具链策略展开到 CI workflow。
`riscv_vcpu` 用例验证 legacy 与 SBI v0.2 completion 对 A0/A1 的不同写回；AxVM
RISC-V router 用例验证广播、零 mask、目标顺序、hart ID 溢出、未映射 hart、重复
vCPU 映射，以及运行时投递失败。参数错误用例同时断言完整目标集合校验完成前没有发布
任何中断；运行时失败用例断言已经发布的前缀不会被伪装成可回滚。对应命令为：

```bash
cargo xtask cross-test --arch riscv64 \
  --package riscv_vcpu --package axvm \
  --features axvm/host-test --no-default-features --lib ipi
```

该目标相关行为测试保持在 RISC-V 私有模块和 vCPU 协议模块内部，不通过 `#[path]`
复制生产文件，也不要求公共 `arch/mod.rs` 为 host test 编译 RISC-V 实现。

源码边界契约测试禁止在 `architecture/ops.rs` 加入架构 `cfg`，也禁止公共 `arch/mod.rs`
出现 hart-mask、IPI 路由 helper 或测试专用 RISC-V 编译分支。该测试应在旧 PR #1681 实现
上失败，在本设计上通过。

若需要回滚，应整体移除 RISC-V IPI 请求处理、独立 QEMU 用例和本设计文档；不得保留公共层
架构分支或恢复当前 vCPU 直接注入的特殊路径。
