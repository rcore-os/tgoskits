# StarryOS 进程生命周期统一所有权设计

## 1. 状态

- 文档状态：设计中
- 适用范围：StarryOS 进程创建、线程退出、进程最终退出、Zombie 发布、父进程回收、PID namespace shutdown，以及进程拥有资源的申请和释放
- 参考实现：Linux v7.1，重点参考 `kernel/exit.c`、`kernel/signal.c`、`kernel/pid_namespace.c`、`kernel/cgroup/cgroup.c`、`fs/file.c`、`fs/fs_struct.c` 和 `kernel/nsproxy.c`
- 变更性质：高风险、允许破坏 StarryOS 内部 API，不改变 Linux userspace ABI

本文档建立在 `starry-pid-zombie-identity.md` 已完成的 PID generation 设计上。后续实现不得替换 `ProcessIdentity`，也不得增加另一套与它竞争的 Live/Zombie/Reaping/Reaped 状态源。

## 2. 问题与现有证据

当前 `do_exit()` 同时承担以下职责：

1. 认领单线程退出；
2. 发起 group exit 并终止同组线程；
3. 清理 robust futex、`clear_child_tid`、线程私有 fd table 和 scheduler address-space lease；
4. 从 thread group 中删除 TID，并判定是否为最后线程；
5. 清理 cgroup、timer、AIO、ptrace、文件锁、SysV SHM 和 process address-space slot；
6. 关闭 child publication，选择 subreaper，执行 reparent 或 PID namespace shutdown；
7. 冻结 Zombie 数据并执行唯一的 `ProcessIdentity::Live -> Zombie` 发布；
8. 修改 `TASK_TABLE`，通知 parent、tracer、pidfd、vfork waiter 和 thread waiter；
9. 在 PID namespace shutdown 路径中代替父进程回收 child；
10. 释放 namespace thread PID。

这些操作目前主要依靠一个大函数里的手工顺序维持正确性。资源自身分别使用 `AtomicBool`、全局 PID 索引、`Drop`、关系事务和局部锁保护，没有一个类型能证明：

- 某个 TID 只退出一次；
- 只有最后线程能持有进程最终退出权限；
- 进程关系已停止接收 child；
- 所有 wait-visible 之前必须释放的资源已经完成释放；
- Zombie 只能发布一次，并且发布后只执行通知等不可失败操作；
- consuming waiter 只能回收精确的 PID generation；
- 资源释放发生在它的真实 owner 上，而不是依赖退出时按 numeric PID 扫描全局表。

CI 已经提供了跨测例污染证据。Starry grouped system runner 逐个等待测试主进程，却不拥有该测试创建的全部 descendant。`test-ptrace-gdb` 的成功路径存在未 `waitpid()` 的 event grandchild；普通 Linux 语义会把 surviving descendant reparent 给 init，而不会因为原 parent 返回就自动终止它。随后用两个连续二进制构成的确定性回归可以稳定证明：第一个用例返回成功后留下的 `case-leak-child` 会被第二个用例观察到。

这说明需要同时修复两个不同边界：

- Starry 进程生命周期必须统一真实进程和资源的所有权；
- 测试 runner 必须拥有一个 case scope，并在 case 结束时终止和回收该 scope 内的全部任务。

二者不能混为一谈。禁止为了让 grouped CI 变绿而修改普通 parent-exit 语义，使其无条件杀死 child；那会偏离 Linux。也禁止用嵌套 PID namespace 包裹每个测试。该方案会改变测试看到的 PID，并已使 ptrace tracer 与 tracee 对 PID 的观察不一致。

## 3. 目标

### 3.1 必须满足

1. `ProcessIdentity` 保持唯一的 PID generation 和 wait/pidfd 生命周期事实源。
2. 线程自退出、最后线程的进程退出、parent/reaper 回收是三个显式且不可混用的 owner。
3. 最后线程权限由 thread-group lock 内的状态转换产生，并通过 move-only token 转移，不能由调用方根据 `threads().is_empty()` 重新推导。
4. 进程最终退出由一个类型化 transaction 串行执行；Zombie 发布前的阶段顺序由类型保证，发布后禁止 fallible 操作。
5. 资源按 lifetime 和真实 owner 分成 thread、mm、process、PID identity、relation、namespace 和 execution scope，不再将 numeric PID 当成所有权。
6. clone 在 visible point 前完成全部 fallible prepare；visible point 后只执行不可失败 commit。
7. consuming wait 认领精确的 `Arc<ProcessIdentity>`，完成 topology retire 后才释放 namespace process PID，保持 PID ABA 防护。
8. 所有通知都在权威状态发布后发生，并且不在宽锁内执行 callback/wake。
9. grouped test runner 使用独立 case scope，case 主进程返回后终止、等待并验证 scope 为空，再开始下一用例。
10. 保持 wait/waitid/pidfd/ptrace/procfs、subreaper、PID namespace 和 signal 的 Linux ABI 语义。

### 3.2 非目标

- 不把所有资源强行塞进一个大锁或一个大结构。
- 不新增与 `ProcessIdentity` 并行的 `ProcessDataLifecycle` enum。
- 不把 `TASK_TABLE`、thread-group members、PID namespace pid map 合并成一个表；它们分别表达 scheduler lookup、thread-group membership 和 namespace-local PID publication。
- 不在本次设计中改变 numeric PID ABI、wait status 编码或 signal ABI。
- 不用超时、重试上限、忽略错误、测试特判或退出时全局扫描作为最终修复。
- 不要求普通 child 随 parent exit 被杀死；该语义只属于显式 execution scope。

## 4. Linux v7.1 语义基线

Linux 没有把“进程退出”实现成一次析构，而是把所有权拆成多个阶段：

| Linux 阶段 | 主要 owner | Starry 对齐要求 |
|---|---|---|
| `exit_signals()` / `PF_EXITING` | 当前 thread | 当前 TID 只能认领一次退出，退出后不再被选择为正常 signal target |
| per-thread detach | 当前 thread | robust futex、clear-child-tid、thread fd/mm/scheduler state 由 thread owner 释放 |
| `signal->live` 最后成员判定 | thread group | 最后线程判定和 CPU accounting 必须在同一锁事务中完成 |
| `exit_mm/files/fs/nsproxy` | 当前 thread 持有的资源引用 | 先从 task 脱钩，再由引用所有权完成真正释放 |
| `group_dead` cleanup | last live thread | timer、group accounting 等只允许 last-thread owner 执行一次 |
| `exit_notify()` | exiting process + tasklist lock | close child publication、reparent、发布 Zombie，然后通知 parent/pidfd |
| `release_task()` | parent wait 或 autoreaper | consuming reaper 解除 topology/PID publication，不能由退出线程提前执行 |
| delayed task release | RCU/epoch | wait-visible record、PID removal和最终对象释放是不同阶段 |
| `zap_pid_ns_processes()` | PID namespace child reaper | namespace shutdown 关闭 PID 分配，杀死并等待 namespace member，再允许 init 被回收 |
| `cgroup_task_exit/release` | cgroup membership | self-exit callback 与 reap/final release 分阶段，不把 cgroup仅当查询索引 |

因此 Starry 的目标状态不是一个万能 enum，而是一组具有唯一转换权限的 owner。状态和资源可以分层，但转换权限不能重复。

## 5. 当前权威 owner 矩阵

| 事实或资源 | 当前权威 owner | 必须保留或修正 |
|---|---|---|
| thread exit 已开始/完成 | `Thread` 的 `exit_started` / `exit` | 保留一次性认领，封装成 thread-exit token |
| live thread set、group exit code、退出 CPU time | `starry_process::Process::tg` | 保留为 last-thread 判定权威源，返回 move-only owner |
| Live/Zombie/Reaping/Reaped | `ProcessIdentity::state` | 保持唯一状态源 |
| public PID 和 pidfd readiness | `PROCESS_TABLE + ProcessIdentity` | 保持 generation 精确性和 registry-first 锁序 |
| parent/children/group topology | `ProcessRelationTxn` | 保持单一关系事务，纳入 exit/reap token 调用边界 |
| scheduler TID lookup | `TASK_TABLE` | 只是运行时索引，不能推导 Zombie/Reaped |
| namespace-local PID | `axnsproxy::PidNamespaceState` | thread PID 在 thread exit 后释放，process PID 在 reap 后释放 |
| address-space generation | `ProcessMemoryOwner` | 保持 mm generation owner，清楚区分 task lease 和 process slot |
| cgroup membership | `ProcessCgroupState` | 保持 authoritative membership，补齐 execution scope 能力 |
| timer/AIO/SHM/ptrace/locks | 多个 process field 或 PID-keyed 全局表 | 逐步迁移到 process-owned handle/table，删除 PID 扫描式 owner |

## 6. 目标类型与权限

### 6.1 `ThreadExitOwner`

`Thread::begin_exit()` 不再只返回 `bool`，而是返回：

```rust
pub(crate) fn begin_exit(&self) -> Option<ThreadExitOwner<'_>>;
```

`ThreadExitOwner` 不可 `Clone`，持有当前 thread 的退出权限。它负责：

- per-thread perf/rseq 清理；
- robust futex 和 `clear_child_tid`；
- 释放当前 thread 的 fd-table share；
- 从 scheduler address space detach；
- 冻结当前 thread CPU time；
- 请求 thread-group 原子删除 TID。

只有持有该 token 的代码能发布 `Thread::exit`、从 `TASK_TABLE` 删除 TID 和释放 namespace thread PID。这样 `exit_started` 不再只是一个容易被遗漏检查的布尔值。

### 6.2 `LastThreadExitOwner`

`Process::exit_thread()` 改为返回：

```rust
pub enum ProcessThreadExit {
    Remaining(RemainingThreadExit),
    Last(LastThreadExitOwner),
}
```

`LastThreadExitOwner` 在 `Process::tg` 锁内产生，包含：

- 精确 `Arc<Process>` generation；
- 已冻结的 group exit code；
- 已冻结的 process CPU time；
- 最后退出 TID 和 leader nice snapshot 所需信息。

它不可公开构造、不可复制，也不能通过查询 API 重新获得。重复退出同一 TID返回错误，并且绝不会再次返回 `Last`。

`exit_code` 必须随 token 冻结并写入 `ZombieSnapshot`。wait/waitid 不再在 Zombie 阶段读取可变 `Process::tg.exit_code`。

### 6.3 `ProcessExitTransaction`

kernel 层用类型状态表达最后线程退出顺序：

```text
ProcessExitTransaction<Owned>
    -> ProcessExitTransaction<RelationsClosed>
    -> ProcessExitTransaction<ResourcesReleased>
    -> ProcessExitTransaction<ZombieFrozen>
    -> PublishedProcessExit
```

各阶段职责：

1. `Owned`
   - 消费 `LastThreadExitOwner`；
   - 绑定精确 `Arc<ProcessData>` 和 `Arc<ProcessIdentity>`；
   - 验证 identity 仍是同一 generation 的 Live 数据。
2. `RelationsClosed`
   - 关闭 child publication；
   - 选择并锁定 live subreaper，或进入 PID namespace shutdown；
   - 产生精确的 reparented/retained child snapshot；
   - 不在之后重新扫描 children 来发送 `pdeathsig`。
3. `ResourcesReleased`
   - 消费每个 process-owned resource group 的 exit handle；
   - drain AIO/SHM 等可能 pin mm 的资源；
   - 释放 process address-space slot；
   - 证明 wait-visible 前必须消失的资源已完成释放。
4. `ZombieFrozen`
   - 冻结 credential、nice、exit code、ptrace tracer、clone-child、wait-parent、CPU time；
   - 生成完整 `ZombieSnapshot`，不再依赖 Live `ProcessData` 查询。
5. `PublishedProcessExit`
   - 执行唯一的 `Live -> Zombie`；
   - 从 `TASK_TABLE` 移除最后 TID；
   - 只允许 parent/tracer/pidfd/vfork/thread waiter 通知和 namespace autoreap handoff；
   - 所有操作必须不可失败，best-effort signal 发送需要明确记录 Linux 允许忽略的目标消失条件。

在 `Owned -> ZombieFrozen` 之间若发现内部不变量破坏，必须 fail-stop，而不是局部跳过清理后继续发布 Zombie。此时 userspace 进程已经不可恢复，伪造一个“成功退出”会隐藏资源和状态损坏。

### 6.4 `ProcessReapTransaction`

现有 `claim_reap()` 已经提供唯一 `Zombie -> Reaping` 权限，但 API 仍以 `Option<ProcessCpuTime>` 暴露。改为：

```text
ProcessIdentity::claim_reap(exact_process)
    -> Option<ProcessReapTransaction>
ProcessReapTransaction::retire_relations(self)
    -> RetiredProcessIdentity
RetiredProcessIdentity::finish(self)
    -> ProcessCpuTime
```

`ProcessReapTransaction` 持有精确 identity 和完整 Zombie snapshot。它负责：

- 保持 `PROCESS_TABLE` 中的 Reaping identity，阻止 PID reuse；
- 解除 parent/group topology；
- 执行 cgroup reap-stage release；
- 在 registry lock 内完成 Reaped 并删除精确 identity；
- 最后释放各层 namespace process PID；
- 发布 pidfd HUP。

任何 numeric PID lookup 都不能代替 exact identity matching。

### 6.5 `ForkTransaction`

clone 当前拥有多套独立 rollback token。目标 API 为：

```text
ForkTransaction<Prepared>
    -> Result<ForkTransaction<Published>, ForkError>
ForkTransaction<Published>
    -> CommittedChild
```

`Prepared` 持有 PID reservation、process resources、relation publication、cgroup membership、task registration、pidfd/user writes、ptrace stop 和 scheduler staging。所有可能失败和分配都必须在 publish 前完成。

publication lock 下的 visible point 应一次完成 namespace PID、relation、`TASK_TABLE`/`PROCESS_TABLE` 和 cgroup publication。进入 `Published` 后只能执行不可失败的 ownership transfer 和 scheduler activation。禁止在多个已经 publish 的 token 之间继续传播 `?`。

### 6.6 `ExecTransaction`

exec 不创建新的 process identity，但会替换 mm/image、清理 siblings、关闭 CLOEXEC fd 并可能 de-thread。它分为：

```text
ExecTransaction<PreparedImage>
    -> ExecTransaction<PointOfNoReturn>
    -> CommittedImage
```

argv/env、ELF、fresh mm、credential 和 fd-close batch 均在 `PreparedImage` 完成。终止 sibling 后进入 point-of-no-return，后续必须不可失败。mm replacement、image publication、signal reset、CLOEXEC close 和 TID/TGID rebind 由同一 transaction 排序。

## 7. 资源所有权重组

统一生命周期不等于所有字段共享一把锁。资源按真实 lifetime 分组：

### 7.1 Thread-owned

- robust futex registration；
- `clear_child_tid`；
- rseq state；
- thread signal/ptrace stop state；
- thread fd-table share；
- scheduler task 和 scheduler address-space lease；
- thread PID publication。

这些由 `ThreadExitOwner` 消费。`Thread::clear_rseq_state()` 必须纳入退出路径，不能保留未调用的清理 API。

### 7.2 MM-generation-owned

- `AddrSpace`；
- private futex domain；
- process slot 和每 task scheduler slot；
- 与 mm 绑定的 AIO ring、SHM attachment、uprobe 等 handle。

`CLONE_VM` 共享同一 mm owner。资源需要直接持有 mm-generation handle，不能只按 process PID 查找。最后一个 process reference 和最后一个 scheduler lease 的释放顺序必须可见。

### 7.3 Process-owned

- signal actions/manager 和 job control；
- interval/POSIX/CPU timers；
- wait/vfork publication；
- ptrace tracer/tracee links；
- cgroup membership 和 namespace refs；
- PID-owner file-lock records；
- process accounting。

为这些资源提供命名明确的 `begin_exit()` / `finish_exit()` 或 move-out handle。禁止新增通用 callback list；callback list 会隐藏锁序、错误语义和资源依赖。

### 7.4 Registry-owned indexes

全局表可以保留作为查找索引，但不能成为资源 lifetime owner：

- AIO context 应由 process/mm handle 持有，global map 只是 ID lookup；
- ptrace 应持有双向 generation-safe link，退出时消费本进程的 link，而不是扫描 `processes()`；
- POSIX/flock lock 应持有 owner token，并在 fd/process owner 释放时解除，不能只靠 `release_pid_locks(pid)`；
- SysV SHM attachment 应属于 mm/process attachment set，manager 只拥有 segment registry。

每迁移一个资源后删除旧的 PID-keyed退出兜底，不能长期保留新旧两条释放路径。

## 8. 锁序和发布规则

### 8.1 全局顺序

1. clone/PID namespace publication gate；
2. `PROCESS_TABLE` registry；
3. 单个 `ProcessIdentity` raw state；
4. process thread-group lock；
5. process relation locks，继续遵守：group binding -> children ascending PID -> child parent -> group members ascending PGID；
6. process resource task-context locks；
7. mm/manager locks的子系统专用顺序。

禁止 identity state lock 反向获取 `PROCESS_TABLE`。禁止持有 IRQ/raw lock 获取 sleepable lock。禁止在 relation/resource 广锁内 wake waiter、发送 signal 或 drop 可能获取未知锁的对象。

### 8.2 状态先于通知

- fatal signal 必须先 publication，再释放 ptrace-stop/job-stop；
- Zombie snapshot 和 `Live -> Zombie` 必须先于 parent/tracer/pidfd wake；
- `Thread::exit` 必须先于 thread pidfd/join wake；
- Reaped 和 registry removal 必须先于 pidfd HUP；
- scope closing 必须先阻止新 task 加入，再终止现有成员，最后等待 empty。

### 8.3 不可失败边界

以下边界后不得执行返回可恢复错误的操作：

- clone visible point；
- exec sibling teardown；
- last-thread removal；
- `ProcessIdentity::Live -> Zombie`；
- `Zombie -> Reaping` claim。

需要内存的 snapshot、relation capacity 和 notification batch 必须提前 reserve。退出阶段允许记录 userspace 地址错误或目标已经消失，但不能以此跳过内核资源所有权转移。

## 9. Case execution scope

grouped test runner 的 owner 不是测试主进程，而是一个显式 `CaseExecutionScope`：

```text
create scope
    -> attach test root before exec
    -> allow fork/clone/setsid descendants to inherit scope
    -> wait test root
    -> close scope against new membership
    -> kill all remaining members
    -> reap/wait until scope empty
    -> destroy scope
    -> run next case
```

实现优先复用 `ax-cgroup` membership，不再维护第二份 runner descendant set。需要补齐 Linux `cgroup.kill` 对应的内核能力：在 membership transaction 下关闭加入，获取 generation-safe member handle，发送 SIGKILL，并等待 authoritative membership 为空。`setsid()`、double-fork 和 reparent 都不能逃离 scope。

runner 不得使用 shell process group 作为最终方案，因为 `setsid()` 可以逃离 process group；不得用 `/proc` 扫描 PID，因为存在 PID reuse 和扫描窗口；不得用固定延迟等待资源自行消失。

## 10. 确定性回归

所有 bug 修复先证明 old implementation 必然失败，再实现 green。至少包含：

### 10.1 最后线程权限

- 两个 thread 并发退出时仅一个获得 `LastThreadExitOwner`；
- 同一 TID 重复退出不能重复计 CPU time、不能再次返回 last owner；
- last owner 冻结的 exit code 不受后续 thread-group 查询或修改影响。

### 10.2 Exit transaction 顺序

- relations 未关闭时类型上不能发布 Zombie；
- mm process slot、AIO/SHM attachment 未释放时不能构造 `ZombieFrozen`；
- Zombie 发布失败不能继续执行 parent/pidfd notification；
- notification observer 一旦被唤醒，必能看到完整 Zombie snapshot；
- rseq、robust futex 和 clear-child-tid 都由同一个 thread owner 完成一次。

### 10.3 Reap generation

- 两个 waiter 只能有一个获得 reap transaction；
- Reaping 期间 public PID lookup 失败，但 PID 仍不可复用；
- topology retire 后才允许 namespace PID reuse；
- pidfd 保留旧 generation，并在 reap 后观察 HUP。

### 10.4 Resource owner

- process exit 后 AIO/SHM/lock/ptrace owner set 为空；
- PID reuse 不会清理新 generation 的资源；
- `CLONE_VM` 共享 mm 时一个 process exit 不会提前销毁另一 process 的 mm resources；
- private fd table 和 shared fd table 各自在最后真实 owner 退出时关闭一次。

### 10.5 Grouped case isolation

- case A double-fork 后父进程成功返回，descendant 持续 `pause()`；
- runner 结束 scope 后 case B 必须确认旧 task 不存在；
- descendant 执行 `setsid()` 后仍不能逃离；
- case 内 tracer/tracee 看到的 PID namespace 不改变；
- scope teardown 必须等待 authoritative membership empty，不能只验证主进程退出。

当前 `test-case-task-isolation` 的 producer/verifier 是这一层的 deterministic red。它在 scope 实现完成前不应作为永久失败提交进入 CI。

## 11. Linux ABI 保持矩阵

生命周期重构完成时必须逐项保持：

- `waitpid`/`wait4`：PID/PGID/Any、`WNOHANG`、`WUNTRACED`、`WCONTINUED`、`WALL`、`WCLONE`、`WNOTHREAD`；
- `waitid`：`P_ALL/P_PID/P_PGID/P_PIDFD`、`WNOWAIT`、`siginfo` 和用户指针 fault 后可重试；
- pidfd：精确 generation、Zombie `IN|RDNORM`、reap 后 `HUP`、signal/getfd validation；
- ptrace：tracer PID、event-stop、SIGKILL、tracer exit detach 和 zombie wait；
- procfs：Live/Zombie lookup 与 `/proc` 枚举的既有边界，`TracerPid/PPid/Threads`；
- subreaper：只选择同 PID namespace 的最近 live subreaper；
- PID namespace init exit：关闭 publication、SIGKILL members、reap child、等待 namespace empty；
- parent death signal：对 relationship transaction 返回的精确 child snapshot 发送；
- normal parent exit：child 被 reparent，不被 execution-scope 规则误杀。

## 12. 分阶段迁移

每个阶段形成可独立 review 的提交，移除被替代的旧实现，不保留两套 owner：

1. **设计和回归基线**
   - 提交本文档；
   - 保留 grouped cross-case deterministic red 作为后续 scope 修复证据，不先提交持续红的 CI case。
2. **线程和最后线程权限**
   - 引入 `ThreadExitOwner` 和 `LastThreadExitOwner`；
   - 冻结 exit code/CPU time；
   - 补并发和重复退出 red/green。
3. **进程最终退出 transaction**
   - 拆分 `do_exit()`；
   - 建立 relations/resources/snapshot/publication 类型状态；
   - 通知统一移到 publication 之后。
4. **Reap transaction**
   - 用 exact identity token 替换裸 `reap_process()`；
   - 冻结完整 wait snapshot，迁移 wait/waitid/namespace autoreap consumers。
5. **资源 owner 迁移**
   - 依次迁移 ptrace、AIO、SHM、PID locks、timers 和 cgroup；
   - 每迁移一项即删除对应 PID scan/fallback；
   - 明确 io_uring 是否完全由 fd ownership 覆盖。
6. **Fork/exec transaction**
   - clone visible point 后不可失败；
   - exec point-of-no-return 后不可失败；
   - 删除重复 rollback 路径。
7. **Case execution scope**
   - 复用 cgroup membership，补齐 kill-and-wait-empty；
   - 接入 grouped runner；
   - 让 `test-case-task-isolation` red 转 green。
8. **遗留清理**
   - 删除未使用 API、重复 lifecycle flag、PID-keyed cleanup 和无 owner 的全局 registry；
   - 更新相关设计文档与测试指南。

## 13. 验证与性能门槛

### 13.1 静态与 crate 验证

- `cargo fmt`；
- 每个修改 crate 运行对应 `cargo xtask clippy --package <crate>`；
- `starry-process` unit tests；
- Starry kernel axtest 中的真实 process/wait/pidfd/ptrace tests；
- 不在 `ax-task` fake system test 中新增生命周期红测。

### 13.2 QEMU

- StarryOS 四架构 grouped QEMU；
- Axvisor 四架构本地 QEMU；
- 重点单独跑 ptrace、wait/pidfd、PID namespace、SHM/AIO 和 case-isolation；
- 对曾经失败的 grouped 顺序做重复运行，确认不存在跨用例残留。

### 13.3 性能

在每个可能影响调度、锁或退出热路径的阶段，对比当前分支与同环境 `dev`：

- task switch/wakeup benchmark；
- process/thread create-exit-wait benchmark；
- Starry grouped 每阶段耗时；
- Axvisor guest entry/timer benchmark。

任一阶段相对 `dev` 慢 20% 以上即视为实现问题，停止扩大改动并定位 owner、锁竞争或重复清理；不等待远程 CI 结束后再处理。

## 14. 风险与回滚

主要风险：

- last-thread token 与 TASK_TABLE/PID namespace removal 次序改变；
- relation transaction 和 PID namespace shutdown 形成新锁环；
- 将 PID-keyed global registry 迁移为 direct owner 时破坏 `CLONE_VM` 或 fd sharing；
- wait/waitid 在 user pointer fault 时过早消费 Zombie；
- ptrace fatal-signal publication 和 stop release 次序回退；
- cgroup scope kill 与普通 process exit 语义串线；
- 类型状态引入额外锁或 allocation，造成调度/退出性能下降。

回滚单位是上述独立迁移阶段，而不是在新 owner 旁边恢复旧 fallback。一个阶段若不能保持 deterministic tests、四架构构建和性能门槛，就回退该阶段的完整 owner 转移，修正设计后重新实现。

## 15. 完成条件

只有同时满足以下条件才能认为进程生命周期重构完成：

1. `ProcessIdentity` 仍是唯一 Live/Zombie/Reaping/Reaped 权威状态机；
2. thread exit、last-thread process exit、parent/reaper 各有不可复制的显式 owner；
3. `do_exit()` 不再直接拼接所有子系统释放步骤；
4. Zombie 发布前资源和 topology 顺序由类型保证，发布后无 fallible 操作；
5. clone/exec 的 visible/point-of-no-return 后无可失败提交；
6. ptrace/AIO/SHM/locks 等不再以 numeric PID 扫描作为主要所有权；
7. grouped runner 使用 authoritative execution scope，确定性跨测例 red 转 green；
8. Linux ABI 矩阵、Starry/Axvisor 四架构 QEMU 和本地性能门槛全部满足；
9. 被替代的重复状态、释放路径、fallback 和遗留 API 已删除；
10. PR 描述、设计文档和验证证据与最终实现同步。
