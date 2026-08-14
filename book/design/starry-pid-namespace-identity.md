# Starry PID namespace 统一身份与所有权

## 背景与风险

Starry 过去同时把 ax-task `TaskId`、线程 TID、进程 PID、进程组 PGID 和会话 SID
表示为裸 `u32`，并由多个 registry 分别管理。这种设计无法表达“同一数字的不同角色”，
也无法区分数字复用前后的两个 generation。失败 clone、非 leader `execve`、zombie、
pidfd、异步通知以及 PID namespace init 退出因而可能各自释放或重新查找同一个数字，
形成双重释放、半发布和 ABA 风险。

本次变更是高风险、跨 crate 的内核身份重构。成功标准不是补齐 Linux 的全部 namespace
功能，而是让 Starry 已实现且 CI 覆盖的 PID ABI 只有一个身份事实来源，并使创建、切换、
退出和回收具备可审计的状态转换及所有权。

## Linux v7.1 对照

对照源码为本地 Linux v7.1，commit
`8cd9520d35a6c38db6567e97dd93b1f11f185dc6`：

- `include/linux/pid.h` 的 `struct pid` 是稳定引用对象，`upid` 数组记录各层 PID
  namespace 中的可见数字；统一的是身份，不是全局整数。
- `enum pid_type` 让同一身份承担 PID/TGID、PGID、SID 等不同角色。
- `include/linux/nsproxy.h` 中 `pid_ns_for_children` 是未来子任务配置；
  `task_active_pid_ns()` 来自当前任务身份，clone 不消费该配置。
- `kernel/fork.c` 先按 `pid_ns_for_children` 分配 PID，再发布任务。
- `fs/exec.c::de_thread()` 保持进程 leader 身份，并把运行执行流切换到该身份。

Starry 保留这些核心不变量，但不在本次实现完整 user/net/ipc namespace 隔离、全部权限
模型或未实现 syscall。

## 类型与事实来源

`starry-kernel::task::pid` 是唯一 PID 身份子系统：

- `PidIdentityId`：内核分配、永不复用的 generation。
- `PidNumber`：一个 namespace 中的非零数字槽。
- `TidNumber`、`TgidNumber`、`PgidNumber`、`SidNumber`：角色化数字。裸整数只允许在
  syscall ABI 解码和最终 ABI 写回处出现；内部解析接口不能互传这些类型。
- `PidBinding { namespace, number }`：从 root 到所属 namespace 的不可变绑定链。
- `PidIdentity`：稳定身份、角色、runtime task link 和 process lifecycle 的统一所有者。
- `PidView`：固定观察者 namespace，负责解析角色和投影可见数字。
- ax-task `TaskId`：只供调度器内部使用，禁止转换成用户 PID；exec 和 namespace 操作不
  修改它。

process、thread group、process group 和 session 的索引分别以 `TgidNumber`、
`TidNumber`、`PgidNumber` 和 `SidNumber` 为 key。pidfd 强持有 `Arc<PidIdentity>`；
信号信息、文件锁、SysV IPC 等历史或异步数据持有 `PidSnapshot`、`PidIdentityId` 或明确
捕获的 view，不靠稍后可能复用的数字重新查找。

### 类型化边界规则

- `Thread::tid()`、`Process::pid()`、`ProcessGroup::pgid()`、`Session::sid()` 直接返回角色
  newtype，不提供会擦除角色的同名 `u32` accessor；只有写入 Linux wire struct、用户指针或
  外部 crate 的固定 ABI 时才调用 `.get()`。
- syscall 的裸 `i32/u32/usize` 参数在入口解析为 typed selector，再传给权限检查、查找和
  操作 helper。例如 scheduler 使用 `SchedulerTarget`，wait 的正数使用明确表示 Linux
  “TGID 或 ptrace TID”语义的 `WaitProcessOrThreadNumber`，perf 使用
  `PerfEventTarget::{AllTasks, Current, Thread(TidNumber)}`。
- 两种角色只有在同一 identity 确实同时承担这些角色时，才允许显式经共同的
  `PidNumber` 转换或比较。例如 leader TID 与 TGID、session leader SID 与 TGID；代码中
  不提供 `TidNumber -> TgidNumber` 等通用转换。
- netlink port ID、fd、POSIX timer ID、用户虚拟地址和 perf event ID 不是 PID；它们保持
  自身的 wire/domain 类型，不能因为整数宽度相同而送入 PID resolver。

长期 owner 也使用稳定 generation：`ax-cgroup::ProcessId` 包装 `PidIdentityId`，AIO context
持有 `PidIdentityId`，uprobe 注册表使用不透明 `UprobeTargetId -> Weak<PidIdentity>` 且
event lease 强持有目标 identity。trace pipe 的每条 record 捕获 `PidIdentityId`、typed TGID
和 emission-time comm；`saved_cmdlines` 仍按其外部文本 ABI 使用数字 key，但历史 record
不再从该 cache 反查，因而 PID 复用不会重命名已排队记录。

调度 trace 的跨 crate 边界传递 ax-task `TaskId`，进入 Starry 后才从用户线程的
`PidIdentity` 投影 root TID；只有内核任务缺少 PID identity 时才在 trace wire 层记录
`TaskId`。perf sideband/sample 和 procfs status 的构造阶段分别保存 typed TGID/TID，最终
写入 perf `u32` wire 字段或 procfs 文本时才解包，避免在中间 helper 中交换 PID 角色。

## 正交状态机

不使用一个无法穷举组合状态的巨型枚举。各维度独立转换：

| 维度 | 状态转换 | 不变量 |
| --- | --- | --- |
| PID namespace | `AwaitingInit -> Active -> ShuttingDown -> Dead` | shutdown 后拒绝预留；Dead 不再解析成员 |
| 数字槽 | `Reserved -> Published -> Removed` | 未提交的槽不可见；Removed 后才允许数字复用 |
| 身份发布 | `Reserved -> Published -> Detached` | identity links 和 namespace index 一次发布 |
| runtime task | `Reserved -> Live(WeakAxTaskRef) -> Exited` | runtime link 不拥有调度器任务；未挂接 task 的身份不能当作 live |
| process | `Live -> Zombie -> Reaping -> Reaped` | 唯一 waiter 获得 reap 所有权 |

`PidReservation` 一次持有整条 namespace 链的数字预留，并在 `Reserved` 阶段提供同一个
尚未公开的 `Arc<PidIdentity>`，供 process topology、role lease 和 pidfd 提前准备；此时
所有 namespace slot 的 identity link 仍为空，`PidView` 无法解析。提交前错误由 `Drop`
整体回滚；提交只在 publication gate 内发生。`PidRoleLease<Tid/Tgid/Pgid/Sid>` 表示角色所有权：
普通线程退出释放 TID，leader 的 TID/TGID 保留到 zombie 被唯一回收，PGID/SID 分别由
process group/session 的生命周期持有。正常路径显式按序释放，`Drop` 只做非阻塞兜底。

## 拓扑与锁顺序

唯一锁顺序为：

```text
PID publication gate
  -> PID namespace（root 到 leaf）
  -> PidIdentity
  -> Session
  -> ProcessGroup
  -> Process
```

session 弱索引 process group；group 强持有 session、弱索引 process；process 强持有
group。宽锁内禁止 wake、等待、用户回调和可能触发跨对象析构的 `Drop`。需要释放 lease
或唤醒任务时先把值移出锁，再在锁外执行。

## 创建、切换、exec 与关闭

### clone

`CloneTransaction` 先确定目标 namespace、预留完整 PID 链，并围绕 reservation 提供的
prepared identity 构造 suspended task、process/group/session topology、role lease、pidfd
和 cgroup 准备状态。地址空间、文件表、用户地址校验以及 parent/child TID 写入等显式可失败
步骤全部完成后，才一次发布 identity links 和 namespace index；之后只执行 task/ptrace/
cgroup 的不可失败提交并让任务 runnable。parent/child view 的 PID 只计算一次并用于 clone
返回值与 TID 指针。

提交前错误由 reservation 的 `Drop` 回滚整条数字链。若不可失败阶段违反内核不变量并发生
unwind，`CloneTransaction::Drop` 还会关闭预分配 pidfd、退出 topology，并对“已发布但尚未
交给 scheduler 的 identity”执行非阻塞 abort：将 runtime、publication 和 process lifecycle
依次收敛为 `Exited`、`Detached`、`Reaped`，再解除整条 namespace binding。这个路径只是不变量
兜底，不承担等待、wake 或正常任务退出。

### namespace 配置

`NsProxy` 的 PID 字段只保留持久的 `pid_ns_for_children`。当前 active namespace 总是从
当前线程 `PidIdentity` 的最内层 binding 推导。`unshare(CLONE_NEWPID)` 和 PID namespace
`setns` 只替换经过完整校验的候选 `NsProxy`，不迁移当前任务；clone 不再 `take()` 未来
namespace。

### exec/de-thread

镜像准备和 sibling 清理完成后进入不可失败提交阶段。调用线程接管原 leader 的
`PidIdentity` 和 TID lease；进程 PID、namespace bindings、process pidfd 保持不变。调用
线程原 identity 转为 exited 并释放，其 thread pidfd 不重定向。ptrace、signal 和 task
lookup 同步切换。

### PID namespace init 退出

publication gate 下先转换为 `ShuttingDown`，拒绝并发 clone，再枚举本 namespace 已发布
identity、终止并等待后代，清空索引后进入 `Dead`。显式 shutdown guard 执行等待；
`Drop` 不阻塞。

namespace 整体进入 `Dead` 是“zombie 数字保留到 reap”规则的明确边界：死亡 namespace 不再
提供任何数字解析，因此 shutdown 在所有 live descendant 退出后清空数字索引，不为尚未由
外层 parent 消费的 zombie 保留一个半存活 registry。wait 拓扑、zombie snapshot 与 pidfd
继续通过强 `Process`/`PidIdentity` 所有权完成 reap 和终态观察，不重新依赖 dead namespace
的数字；identity 后续释放 role 时允许发现对应 namespace slot 已随整体 teardown 移除。

## 已实现 ABI 的角色映射

| ABI | 解析角色 |
| --- | --- |
| `getpid/getppid`、process-directed signal、默认 pidfd | TGID |
| `gettid`、`tkill`、sched/affinity、ptrace、`process_vm_*`、thread pidfd | TID |
| `tgkill`、`rt_tgsigqueueinfo` | TGID + TID |
| group signal、`setpgid/getpgid`、group wait/priority | PGID |
| `setsid/getsid` | SID |
| `wait*` | typed `Any/Identity/Group/PidFd` selector |
| `perf_event_open` | `AllTasks`、`Current` 或 TID |

`kill`、`waitpid/waitid`、priority、job control、scheduler、ptrace 和 pidfd 在 syscall 边界
将正数、零、负数及 flag 组合解析成私有 typed selector；特殊值不会进入通用数字查找。
procfs 固定 mount view；用户拥有的异步对象捕获创建者 view；系统 trace 使用 root view。
`nl_pid` 仍是 netlink port ID，仅自动默认值来自调用者 TGID。

## 非目标

- 不新增当前不存在的 Linux syscall。
- 不实现完整 user/net/ipc namespace 隔离或新的权限模型。
- 不保留 `axnsproxy`、`starry-process`、旧裸 PID registry、兼容 re-export 或 deprecated
  API。
- 不把调度器 `TaskId` 暴露为用户 PID。
- 不要求物理板验证；本地交付门槛是确定性 x86_64 回归、kernel axtest、workspace test、
  clippy 和格式检查，四架构完整 grouped QEMU 统一由必需 CI 验证。

## 回归证据与验证计划

修复前的旧 grouped runner 确定性复现：前一 binary 调用 `setsid()` 留下持锁后代，后一
binary 无法取得锁，日志包含：

```text
CASE_TASK_ISOLATION_LEAK_PUBLISHED
CASE_TASK_ISOLATION_FAILED: descendant from previous case still owns lock
STARRY_GROUPED_TEST_FAILED
```

新 runner 在 x86_64 QEMU 中让同一用例两阶段通过并打印
`STARRY_GROUPED_TESTS_PASSED`。修复前 namespace 回归中第二次 fork 观察不到持久的
`pid_ns_for_children`；修复后同一 x86_64 namespace case 为 19/19 通过。

kernel axtest 覆盖链式预留失败回滚、发布前不可见、错 role 解析拒绝、root/child view
投影、namespace shutdown 拒绝新预留、真实 live descendant 的 `Pending -> wake -> Dead`
等待路径，以及 PID 数字复用后旧 `PidIdentityId` lookup 失效。新增测试第一次运行因把
“只有 TID lease、尚无 runtime link”的 identity 错当成可查身份而稳定红灯（406/407）；
修正 fixture 为“只有 PGID role、数字可查但 TGID/TID role 不存在”后，同一 x86_64 kernel
QEMU 为 407/407 通过。

严格延迟发布的回归先临时恢复旧的“只有 Published identity 才能取得 role”实现；prepared
clone 在 `pid_identity_state_machine_rules_hold` 中确定性以 `BadState` panic。恢复
`Reserved` 阶段 role 准备后，同一 x86_64 kernel QEMU 为 407/407。测试还覆盖已发布、未
挂接 scheduler task 的异常事务由 `CloneTransaction` fallback 完整解除数字索引。pidfd ABA
用例强制复用相同数值后，旧 pidfd 返回 `ESRCH`，modern-fd family 为 260/260；非 leader
exec identity transfer 用例为 17/17。`starry-kernel` 的 25 个 clippy 配置（包括 aarch64
PMU 路径）全部通过。

聚焦 PID ABI 还产生了三组确定性红绿证据：

- `pidfd_open(non-leader TID, 0)` 曾因通用 role resolver 抹平“数字不存在”和“identity
  存在但无 TGID role”而返回 `ESRCH`；pidfd 专用 typed TGID 分支现在只对后者返回
  `ENOENT`，不存在或已 reap 仍为 `ESRCH`，原用例转绿。
- reap 会解除 namespace 数字绑定，但强持有旧 generation 的 pidfd 仍必须观察
  `Reaped` 终态。移除 pidfd poll 对 `Published` 的错误前置条件后，原红灯重新得到
  `POLLIN | POLLRDNORM | POLLHUP`；数字 lookup 仍无法找到已解绑 identity。
- robust futex owner word 由用户 active view 的 `gettid()` 写入；退出清理曾以 root TID
  比较，导致 grouped runner 的 PID namespace 中 owner-death 位不更新。清理现在使用 typed
  `user_tid()`，并通过 user-u32 compare-exchange/retry 原子写回，普通与 pending robust
  owner-death 原红灯均转绿。

pidfd fdinfo 用例同时去除了对 outer PID 数值的硬编码：按 Linux 规则校验 `Pid` 等于
`NSpid` 首项，`NSpid` 末项等于 child namespace TID；这样 procfs observer view 与目标
identity 的内层 binding 都被验证，而 runner 新增的外层 PID namespace 不会污染预期值。

在最新 `origin/dev` 上运行 x86_64 完整 grouped case 后，还稳定暴露了 7 个与 PID
身份边界直接相关的红灯：`bug-zombie-process-queries`、`bug-zombie-syscalls`、
`syscallguard-final-wait-runtime`、`test-capget`、`test-clone-tls`、
`test-ptrace-seize-traceexec` 和 `test-waitid-pidfd`。修复分别覆盖 zombie 的稳定 process
selector、wait selector 的 `ECHILD`/pidfd parent 语义、leader runtime 退出后仍 live 的
process 可见性、Linux v7.1 negative capget errno、child active-view TID、ptrace event
snapshot 投影和 pidfd wait 的稳定 generation；相同 7 个聚焦 x86_64 case 已全部转绿。

第一轮四架构 CI 还在 aarch64 `perf-hw-freq` 中稳定捕获了 kernel-context PMU overflow：
硬中断发生在无 Starry `Thread` 的内核任务上，旧采样路径调用 `as_thread()` 并以
`expect("kernel task")` panic。修复后采样只通过 `try_as_thread()` 取得可选 Linux 身份；
用户线程仍按 perf event 捕获的 observer view 投影 typed TGID/TID，内核任务则在 wire
层写入零值，绝不把调度器 `TaskId` 伪装成 Linux PID。aarch64 专属 axtest 同时固定了
“内核任务没有 sample PID/TID 身份”的边界。

后续四架构 CI 在 loongarch64 的 `test-ext4-inode-unique` 暴露了 runner 预算问题：用例在
120 秒时已完成到第 1292/2048 个同步文件操作，没有出现 inode 唯一性断言失败，但被新加的
统一单例 timeout 终止。测试仍完整保留 2048 次 `open/write/fsync/stat/fstat` 和两两 inode
校验；共享 runner 的默认预算继续为 120 秒，只按明确 binary 名称给该同步写入密集用例
240 秒，并将选出的预算作为显式参数贯穿 deadline、超时日志与 namespace 清理。axbuild
静态契约测试先在全局 120 秒实现上红灯，再固定默认值、唯一例外及参数传递，避免把一次
LoongArch TCG 性能差异变成所有用例的全局放宽。

下一轮 aarch64 CI 继续暴露两个已被 fail-fast 遮住的边界。`perf-hw-rdpmc` 在 typed
selector 把 `pid=0` 正确解释为当前任务后进入 per-task counting 路径，但该路径只接受
sampling ring mmap，导致 counting metadata mmap 返回 `ENODEV`；修复复用稳定预留的
programmable counter 生成单页 `perf_event_mmap_page`，测试按 metadata 的 1-based index
读取 `PMXEVCNTR_EL0`，不再把当前任务事件错误假设为 system-wide `PMCCNTR_EL0`。测试先以
disabled 状态 mmap，`ioctl(ENABLE)` 后通过 `sched_yield` 触发明确的 sched-in 发布，再比较
EL0 直读与 `read(perf_fd)`。

同一轮 `test-pagecache-cap` 已完成 1400 个磁盘文件的创建、映射、触页以及 `<100MB` 内存
增量断言，但在逐个 `munmap`/`unlink` 的清理阶段超过默认 120 秒。覆盖规模和阈值保持不变；
共享 runner 继续默认 120 秒，只为该精确 binary 名称增加 240 秒预算，并由 axbuild 静态
契约与 `test-ext4-inode-unique` 的例外一起固定。

四架构转绿复跑中，riscv64 又在 `test-stat-family` 最后一个 `fstatfs(-1)` 断言通过后静默，
最终命中外层 1800 秒 QEMU timeout。用例随后唯一的动作是 teardown 中
`system("rm -rf ...")`；同时 runner 在单 binary 120 秒超时后向 namespace init 发
`SIGKILL`，却用无 deadline 的阻塞 `waitpid` 回收，因此 descendant 若停在 raw wait，既不
会进入 `do_exit`，runner 也无法打印失败或继续。修复把隔离回归的持锁后代明确停在 raw
pipe read，PID namespace shutdown 在发布 fatal signal 后 force-wake 所有 runtime thread；
runner cleanup 另设 30 秒 deadline，失败立即中止 suite。stat fixture 改用已知路径的直接
`unlink`/`rmdir` 清理，保留全部 stat ABI 断言且不再引入额外 shell fork/wait 生命周期。

本地最终验证包括：`cargo xtask test` 的 45 个 std package 全部通过，x86_64
`starry-kernel` QEMU axtest 407/407，通过 `starry-kernel` 的 25 个 clippy 配置、
`axbuild` clippy、893/893 单元测试、lock-lint、fmt 和 diff check。按交付约定，x86_64、
aarch64、riscv64、loongarch64 的当前分支完整 grouped QEMU 不在本地重复运行，统一以 CI
结果为准；queued、cancelled 或 fail-fast 跳过的任务不计为成功。
