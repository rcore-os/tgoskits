# 地址空间切换与 TLB 回收事务化

## 1. 基线、问题与目标

本文对应 PR #1775 之上的通用性重构。实现和验证基线固定为：

- PR #1775 head：`982a13d33020fe9f5895140a37bb3ad31b96ec21`；
- parent branch：`codex/refactor-ax-task-from-1596`；
- 本地 Linux 对照树：`/home/zhourui/linux-src`，head
  `8cd9520d35a6c38db6567e97dd93b1f11f185dc6`（Linux v7.1）。

原问题不是某一条漏掉的 `flush_tlb`，而是一个跨层状态转换被拆成了多个可独立调用的动作：
调度器先选择任务，runtime 再切地址空间和寄存器上下文，OS 的页表后端又独立删除 PTE、
释放页框。开发者只要漏掉一步、交换两步，或者把物理 root 当成逻辑 mm 身份，就可能产生：

- 新任务使用旧任务页表，或 execution context 与地址空间不配对；
- 同一 mm 的线程切换错误撤销 active-mm lease；
- 不同 mm 恰好复用同一个 root 数值时错误跳过硬件安装；
- CPU 仍缓存旧 PTE 时释放、复用 frame 或 VA；
- 普通 Rust 调用者绕过 runtime 检查直接进入用户态。

本次目标是把这些顺序变成类型和对象所有权的一部分，而不是依赖调用点记忆。非目标包括：

- 不把 PID/TID 可见编号用作硬件切换身份；
- 不在每次 user/kernel trap 上切到独立内核页表；
- 不把 boot page table、Axvisor stage-2/EPT/NPT 纳入本次 stage-1 统一；
- 不把 page-table-generic 扩成跨 CPU shootdown 或 OS 资源回收层；
- v1 不采用 per-core replicated page table。

## 2. 架构裁决：行为对象加单向 prepared token

采用两类互补对象：

1. 行为对象拥有长期资源和身份：`TaskAddressSpace`、`AddressSpaceCpuState`、
   Starry `AddrSpace`、ax-mm `AddrSpace`；
2. move-only prepared token 表达一次性状态转换：`RuntimeSwitchPlan`、
   `PreparedAddressSpaceSwitch`、架构 `PreparedContextSwitch`、`PreparedUserEntry` 和
   `TlbGather`。

不采用“拿到 guard 后由上层任意执行，Drop 时再完成正常切换”的设计。`Drop` 只适合回滚尚未
commit 的本地准备；正常页表安装、远端 shootdown、可睡眠资源回收都具有外部可观察副作用，
不能靠隐式析构决定发生时机。所有正常转换必须由消费式 `commit/enter` 明确完成。

这一设计把可失败阶段和不可逆阶段分开：

```text
prepare（可失败、无发布）
  ├─ 校验 previous/next execution context
  ├─ 校验 CPU binding、FP 状态和 switch-tail
  ├─ 校验 previous/next 逻辑 mm 与 active-mm lease
  └─ 校验 membarrier identity

commit（不可失败、不得插入可失败逻辑）
  ├─ 发布 next 的 CPU footprint
  ├─ 安装 next root（按逻辑 mm 判断）
  ├─ 发布 CPU-local active handle
  ├─ 撤销 previous footprint / lease
  └─ 立即进入裸 context switch 汇编
```

任务退出遵循同一边界。`CurrentExitPermit` 在 OS 可见完成态发布前完成 switch-tail、owner work、
placement、PI 和 callback 校验，并关闭该线程新的 scheduler activity；发布完成态后的
`commit_prepared_current_exit()` 只消费 permit，不再重复进入可失败的 switch-tail 或 inbox drain。
架构 switch-tail 自身也在 `finish_context_switch_tail()` 前完成 migration/deadline 校验；该发布点
之后的状态不一致只能作为 fatal invariant，禁止返回一个看似可重试、实际已部分提交的错误。

调度器只构造并消费一个 `RuntimeSwitchPlan`，不再先调用独立的
`TaskRuntime::activate_address_space()`。这样 execution context 和 address-space handle 不会在
两个 runtime 调用之间失配。

## 3. 逻辑 mm、root 与 active-mm lease

`AddressSpaceCpuState` 是逻辑 mm 的硬件身份和 CPU footprint 账本。判断 same-mm 只允许比较
共享 tracker 的对象身份，不能比较线程 token、raw handle、PID/TID 或 root 数值。

| previous → next | 行为 |
| --- | --- |
| 同一 `AddressSpaceCpuState` | 保留既有 active token/lease，不重复换账本 |
| 不同 `AddressSpaceCpuState`、root 不同 | 发布 next footprint，安装 root，撤 previous footprint |
| 不同 `AddressSpaceCpuState`、root 数值相同 | 仍按 `DifferentAddressSpace` 安装，禁止把 root 相等当 same-mm |
| user → kernel thread | 保留 Linux 式 lazy active-mm lease，不因 trap 切页表 |

CPU footprint 的发布顺序是安全性的组成部分：进入 mm 时先发布 CPU bit，再安装 root；离开 mm
时先安装 replacement/safe root，再清 CPU bit。前者保证并发 PTE 修改不会漏掉正要进入的 CPU，
后者保证 shootdown 不会忽略仍可能使用旧 root 的 CPU。

exec、任务退出、normal schedule 和 CPU offline 使用同一 active-mm 状态机。CPU offline 先安装
安全 kernel root，再撤 active handle 和 CPU bit；只有之后才允许发布 CPU offline。

## 4. 用户态入口边界

四架构 `UserContext` 只保留带完整 `# Safety` 合同的
`unsafe run_unchecked()`。普通调用者使用 runtime 的 `UserExecutionContext`：

- `bind()` 把寄存器镜像绑定到当前 execution-context identity 和逻辑 address-space identity；
- `enter()` 先耗尽 scheduler work，再验证 current context、switch tail、preempt/IRQ 状态、逻辑
  mm、active handle、硬件 root 和架构返回镜像；AArch64 还要求固定的 EL0t/DAIF profile，
  ptrace 先在临时镜像上验证再提交；
- 私有 `PreparedUserEntry` 借用寄存器镜像，既不可复制也不可 `Send/Sync`，并在最终验证后立即
  消费进入汇编；
- 上层拿不到一个可在 prepare 和 enter 之间插入任意安全代码的 guard。

入口失败恢复调用前的 IRQ 状态并返回 typed error；一旦形成 `PreparedUserEntry`，后续不再运行
可失败逻辑。

## 5. 页表修改、失效与回收

### 5.1 共同事务

所有 published stage-1 destructive mutation 固定为：

```text
修改 PTE
  → 记录所有受影响 VA range
  → 对可能使用该 root 的 CPU 同步失效
  → 全部确认后释放 frame / VA / backend / page-cache owner
```

`TlbGather` 是 `#[must_use]` 的 move-only 所有权包。backend unmap 把 frame 所有权交给 gather，
不能在 `unmap_page()` 后直接 `dealloc_frame()`。部分 populate 失败时，尚未发布的 frame 可以立即
回滚；已经安装过 PTE 的 frame 必须进入 gather。

事务区分“逻辑提交”和“硬件确认”。PTE、VMA 与 RSS 等逻辑账本在地址空间锁内一起提交；RSS
不是可在 shootdown 后才补写的 deferred resource。shootdown 失败时，已发布 mutation 的结果不能
被重新包装成普通 `Err`，否则 `mremap`、`brk`、clone 或清理路径会错误执行“操作未发生”的补偿。
实现保留原 mutation 结果，把确认失败的 range 和 owner 放入 quarantine；下一次 mutation 在任何
新 PTE 修改或 VA 复用之前必须先成功重试，否则以“本次操作尚未开始”返回错误。

跨 CPU 失效即使遇到一个远端错误，也先完成当前 CPU，并继续尝试所有其余目标 CPU，只保留第一
个错误用于诊断。这样发起 syscall/page fault 的 CPU 不会因为远端超时而携带自己的旧 translation
返回；未确认的远端仍由 quarantine 阻止 frame/VA 回收和复用。

### 5.2 ax-mm kernel stage-1

ax-mm 的 gather 面向所有可能使用全局 kernel root 的在线 CPU，跨 CPU 失效由 ax-hal/runtime
完成。kmod、kprobe、eBPF、perf/JIT、kernel text patch、DMA alias 和 kernel page fault 不再取得
`kernel_aspace` 的裸 mutable 入口，而是通过 `ax_runtime::kernel_mapping` 的
`map/protect/unmap/query` 门面。

失败的 gather 连同 deferred frame 进入 ax-mm address-space quarantine。重试点包括下一次安全
mutation、kernel mm teardown 和 CPU offline 的 active-mm 撤销之前；只有 quarantine 重试成功后，
offline commit 才安装安全 root、撤 active handle/CPU bit 并停止本地 clock event。overwrite mapping
在 unmap 与同 VA 安装之间还会显式重试一次，确认前 VA 和 frame
都不能复用。

### 5.3 Starry user stage-1

Starry 保留自己的 active-mask gather，因为它还拥有 COW backend、file backend、
`DeferredFrameRelease` 和 `PageCache` 等 OS 资源，不能复制成 ax-mm 的 frame-only 实现。
published `AddrSpace::page_table_mut()` 不再公开；loader 只能在页表尚未发布时调用专用的
`initialize_kernel_root_entries_from()`。

Starry quarantine 保存 range、backend、frame 和被逐出的 page-cache owner。失败时下一次安全
mutation 或 mm teardown 会重试；teardown 仍失败时输出 root identity、active CPU mask、range
数量、pending 数、失败次数和最后错误。最终 slot 释放前把预分配的 intrusive retention node
转移到全局 owner list；node 持有完整 `Arc<AddrSpace>`，因此 page-table root、VMA、backend 和
pending gather 一起存活。teardown 是单向状态，开始后 process/scheduler slot 都不能重新 attach；
确认成功后才允许最后一个强引用进入 `Drop`，禁止用 `mem::forget` 静默掩盖。

同一 cached file 可被多个地址空间映射。容量淘汰按 address-space owner 给 listener 分组：持有
当前 `AddrSpace` 锁的 populate 只排除自己的 owner，并把其全部 VMA 失效并入当前 gather；其余
owner 必须各自完成 unmap 与 shootdown。任一非当前 owner 返回 false，cache 就把旧页回插并返回
`ResourceBusy`，不能让当前 gather 只确认一个 mm 后释放仍被另一个 mm 映射的 frame。替换页的
分配和 backing read 在旧页脱离 cache 之前完成，gather 的 retained-page 容量也在驱逐之前预留。
普通 LRU 拒绝 eviction 时回插的是同一 `(file, page number, frame)`，不是把 frame 交给 allocator；
同一 mm 的下一次 eviction 会先重试其 TLB quarantine。truncate 已经提交新 EOF 后不能把 listener
拒绝重新包装成“truncate 未发生”，也不能丢弃 EOF 外旧 frame；这些 page 进入 cached-file 所有的
单槽 quarantine，后续 task-context truncate 或最后 backend owner teardown 确认安全后才释放。
eviction 先在 listener registry mutex 下克隆 callback `Arc`，随即释放 registry mutex，再调用外部
callback；这样 callback 可以安全登记或移除 listener，不会自锁。snapshot 发生在旧 page 已从
cache 摘下之后，晚到的 listener 只能映射 replacement page，不能漏掉对旧 frame 的失效确认。
容量淘汰为保持 pop/restore 原子性仍持有 cached I/O/page lock，因此其 callback 合同保持非阻塞；
truncate 和全局 reclaim 则在 callback 前释放这些 lock。

gather 的 bookkeeping 也属于 prepare/commit 合同：多 frame/backend owner 的 overflow capacity
必须在对应 backend 修改第一条 PTE 前用 `try_reserve` 准备；多 range 容量在 operation 前准备，若
修改后的离散 range 超出容量，则无分配地折叠成覆盖全部地址的保守区间。retained file-page owner
处理逐个克隆现有 `Arc`，不在 PTE 修改后 `collect` 临时 `Vec`。

`move_pages` 先把源 range 的全部 backend（包括 file eviction listener owner）转移一份引用到
gather，再完整预检 Cow RSS charge map、移动 PTE，最后以无失败 commit 更新 RSS；即使 shootdown
进入 quarantine，后续 source VMA metadata commit 也不能提前析构最后一个 backend owner。不同 VMA
的 `protect` 先完成全部 backend/PTE prepare，任一失败则按逆序恢复旧 flags，成功后才拆分并发布
VMA metadata。unmap 在第一条 PTE/owner 修改前预检所有 backend；backend/PTE 阶段完全结束前不
删除任何 VMA metadata，因此后续 area 失败时全部 owner 仍存活且同一请求可安全重试；已经脱离
PTE 的 frame、file-page 与 backend owner 进入 gather。因此 operation error 表示逻辑状态已回滚、
尚未开始，或保留完整 owner metadata 的可重试中间状态；post-commit 的
shootdown error 只进入 quarantine，不会触发 syscall 层的错误补偿。

普通 unmap 和最终 `AddrSpace::clear` 还要处理 inode-scoped memfd writable-shared VMA 计数。
该计数不属于 deferred frame，不能在可能失败的 VMA/PTE 清理之前修改：range unmap 先准备
`SharedWritableUnmap` delta plan，在 `MemorySet::unmap` 成功后才 commit；clear 先分配并填充 move-only
`SharedWritableRelease`，`MemorySet::clear` 成功移除全部 VMA 后才无分配地消费 release plan。
中途失败直接丢弃未 commit 的 plan，普通 retry 和 whole-owner teardown 都不会重复修改同一
memfd 计数。

CPU offline 的 runtime hook 处于 IRQ-off、scheduler lock 持有阶段。所有可能失败的 kernel gather
重试必须先完成；之后才以不可失败 commit 安装安全 root、撤 active bit。这里不能获取 Starry
`PiMutex` 或析构可能睡眠的 file/backend owner。因而 Starry per-mm quarantine 在 CPU bit 撤销后
变为可重试，但资源释放仍由
下一次 task-context mutation 或 teardown 执行。这是有意的上下文边界，不把可睡眠回收塞进
offline guard。

## 6. Linux v7.1 对照

本地 Linux v7.1 的关键顺序如下：

- `kernel/sched/core.c:5325-5375`：`context_switch()` 对 kernel thread 借用 previous
  `active_mm`，user task 在切换 mm 前执行 membarrier 相关顺序；
- `kernel/fork.c:672-740`：`cleanup_lazy_tlbs()` / `__mmdrop()` 在释放 mm 前先把 lazy CPU
  切离；
- `kernel/cpu.c:908-920`、`kernel/sched/core.c:8342-8357`：CPU offline 先切到
  `init_mm`，再 drop 旧 active_mm；
- `arch/x86/mm/tlb.c:909-965`：以 `LOADED_MM_SWITCHING` 和 CPU mask 包住 CR3/
  `loaded_mm` 切换；
- `arch/x86/mm/tlb.c:1276-1355`：mm 切换采用保守 flush，`freed_tables` 要求所有 CPU
  参与；
- `arch/x86/mm/tlb.c:1428-1463`：generation 发布和同步确认形成回收屏障；
- `mm/mmu_gather.c:427-555`：页表/TLB flush 完成后才执行批量 free。

TGOSKits 不照搬 Linux 的散布式 C 宏和隐式约定，而是保留其语义顺序，再用 Rust ownership、
对象 identity 和线性 token 收紧可调用状态。

## 7. 论文证据与采用边界

| 材料 | 可复用结论 | 本次采用 | 仅作为后续方向或限制 |
| --- | --- | --- | --- |
| [Theseus](https://www.usenix.org/conference/osdi20/presentation/boos) | 用所有权和 affine type 表达资源边界，`MappedPages` 把映射和生命周期结合 | address-space 行为对象、move-only transition/gather | unsafe context switch 仍需显式小边界，类型系统不能替代硬件顺序 |
| [Asterinas](https://arxiv.org/abs/2506.03876) | 把敏感资源收敛到小 TCB；任务在任一时刻最多运行于一个 CPU | runtime 独占 user-entry 能力、CPU-bound prepared token | safe scheduler 仍可能产生逻辑错误，因此保留运行时 identity 验证 |
| [RadixVM](https://pdos.csail.mit.edu/papers/radixvm:eurosys13.pdf) | per-mapping CPU footprint；unmap 等待相关 CPU 后再 free | `AddressSpaceCpuState::active_mask` 与确认后回收 | per-core replicated page table 会复制 fault/同步复杂度，v1 不采用 |
| [RelaxedVM](https://arxiv.org/abs/2203.00642) | Arm TLBI、DSB、ISB、break-before-make 必须按弱内存模型验证 | 将 shootdown acknowledgment 视作资源回收屏障；四架构 SMP 测试 | 后续增加 litmus/model/hardware 三层验证，不把 QEMU 当弱内存完备证明 |
| [Hyperkernel](https://www.helgikrs.com/papers/hyperkernel.pdf) | 小状态机、表示不变量、失败时状态不变更容易验证 | prepare 可失败、commit 不可失败的小转换 | 论文验证范围偏 UP/IRQ-off，不能直接证明本实现 SMP 正确性 |
| [Singularity](https://www.microsoft.com/en-us/research/wp-content/uploads/2016/02/osr2007_rethinkingsoftwarestack.pdf) | contract state machine 与 ownership transfer 缩小信任边界 | runtime/OS capability 分层和消费式所有权转移 | language isolation 不证明 MMU/TLB 硬件一致性 |

## 8. 分层边界

| 层 | 负责 | 不负责 |
| --- | --- | --- |
| page-table-generic | walker、PTE 结构、单页 map/unmap/protect 原语 | CPU footprint、IPI、deferred owner |
| ax-cpu / ax-hal | stage-1 flags、本地 root/TLB 指令、架构 user entry | OS backend 生命周期 |
| ax-task | 调度决策、execution/address-space handle 的单次 switch plan | 解引用 OS mm、直接操作页表 |
| ax-runtime | prepared switch 组合、active-mm lease、kernel mapping 门面、跨 CPU shootdown | Starry VMA/COW/file policy |
| ax-mm | kernel stage-1 aspace、frame deferred reclaim/quarantine | Starry page cache/backend |
| Starry mm | user VMA、COW/file backend、active-mask gather/quarantine | 调度器 context switch 细节 |

## 9. 回归和验收台账

确定性低层回归包括：

- ax-mm：shootdown 失败时 frame 不 reclaim，重试确认后才 reclaim；partial populate rollback 中
  已发布 frame 同样 deferred；
- runtime：`A(same mm) → B(same mm) → C(other mm)` 保留共享 lease 后再按序撤销；不同
  逻辑 mm 即使 root 相同仍安装；
- user entry：current context 错误、switch tail 未完成、mm/root 不匹配、IRQ/preempt 状态不安全，
  以及可返回特权态或屏蔽中断的寄存器镜像都不能进入；raw entry 保持
  `unsafe run_unchecked()`，rustdoc `compile_fail` 证明安全代码不能直接调用，四架构构建中的
  `unsafe fn` 类型断言防止任一实现把签名退回安全函数；AArch64 DAIF validator 必须使用
  `FieldValue::mask()` 的已移位硬件 mask，禁止把字段宽度 `Field.mask` 当成 SPSR 位掩码；
- Starry：shootdown 失败把 backend/frame/file-page owner 原样返回 quarantine；published
  `page_table_mut` 只在 mm backend 的受控 gather 范围内可见，外部 mutable access 由 rustdoc
  `compile_fail` 拒绝；多 VMA unmap 的第二个 backend 失败时，前一个虽已移除 PTE，全部 VMA/
  backend owner 仍保留到 retry；memfd unmap delta 只在 VMA transaction 后 commit；`mremap`
  source backend 在 unconfirmed shootdown 中由 gather 继续持有；
- CPU offline：源码合同固定 kernel quarantine retry 早于 active-mm release，失败返回时 CPU 仍
  保持原 active-mm 状态；
- `qemu/system/mm-transition-safety`：两个 CPU 上以 affinity、barrier 和 `/proc/.../status` 的
  sleeping-state 观测固定顺序，覆盖共享 mm 的 writable translation、worker 已阻塞后的
  `mprotect`/远端唤醒、`sched_yield` 和 fork COW kernel-copy；
- ArceOS `mem-stage1-transition`：显式持有 original 与 replacement 两个不同 frame，远端 CPU 缓存
  old VA 后，controller 把 PTE 改成只读且不恢复 WRITE，远端真实 store 必须进入临时
  page-fault handler；handler 记录 WRITE fault、恢复权限并让原指令重试，若远端仍携带 stale
  writable TLB 则不会 fault，测试确定性失败。随后再执行 unmap 并立即复用同一 VA，分别写入
  stale/replacement sentinel，确定性证明远端看到 replacement translation。低层单测独立证明
  shootdown 确认先于旧 frame reclaim。

交付门禁不是“跑过一次”。每次 parent rebase 或 child SHA 变化都重置为 `0/3`。最终 child SHA
必须以 `since_sha=982a13d33020fe9f5895140a37bb3ad31b96ec21` 连续完成三次独立 CI
workflow_dispatch；每次 required static/workspace job 和
ArceOS、StarryOS、Axvisor × x86_64、aarch64、riscv64、loongarch64 全部成功，且每个 QEMU
子任务存在 success marker。cancelled、skipped、timeout、缺 marker 或仅 rerun failed job 均不计绿。
