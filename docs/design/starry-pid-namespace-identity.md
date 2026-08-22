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
- Unix `SCM_CREDENTIALS` 在 skb 中强持有 `struct pid`，接收时经 `pid_vnr()` 按当前
  namespace 投影；`SO_PEERCRED` 同样保存稳定 `struct pid` 而不是创建时的裸 PID。参见
  [`scm_cookie`/`scm_set_cred`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/include/net/scm.h#L44-L80)、
  [`unix_scm_to_skb`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/net/unix/af_unix.c#L1987-L2006) 和
  [`cred_to_ucred`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/net/core/sock.c#L1704-L1714)。
- SysV `IPC_SET` 只更新 owner UID/GID 与低 9 位权限，不能从用户 buffer 覆盖 creator、
  size、PID、attach count 等只读统计字段。参见
  [`ipc_update_perm`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/ipc/util.c#L679-L697) 和
  [`shmctl_down`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/ipc/shm.c#L995-L1034)。

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

`task/pid.rs` 暂时把数字槽、identity、role lease、namespace reservation 和 view 投影保留
在同一模块中。该文件超过通常的模块规模目标，但这些类型共同实现一条原子发布状态机；在
状态转换与锁边界尚在本次高风险重构中稳定前拆分，会把私有中间状态扩大成跨模块接口并增加
漏掉回滚分支的风险。后续拆分应按“namespace slot/reservation”和“identity role/view”两个
私有子模块进行，并保持当前 axtest 先行；本次不把纯机械拆分混入语义修复。

process、thread group、process group 和 session 的索引分别以 `TgidNumber`、
`TidNumber`、`PgidNumber` 和 `SidNumber` 为 key。pidfd 强持有 `Arc<PidIdentity>`；
信号信息、文件锁、SysV IPC 等历史或异步数据持有 `PidSnapshot`、`PidIdentityId` 或明确
捕获的 view，不靠稍后可能复用的数字重新查找。

Unix transport 不能依赖 `starry-kernel`，因此 `ax-net::UnixCredentials` 保留 PID/UID/GID
数值作为通用 OS 的 ABI fallback，并允许 OS glue 附加不透明、引用计数的 generation。
Starry 在创建 peer、建立连接和发送消息时附加 `Arc<PidIdentity>`；transport 只 clone 和排队
该对象；`SCM_CREDENTIALS` 或 `SO_PEERCRED` 写回用户态时，Starry 才把它 downcast 并按接收者
active view 投影。若目标在接收者 namespace 不可见则写 PID 0，与 Linux `pid_vnr()` 一致。
这一边界避免让可复用网络 crate 反向依赖内核 PID 类型，也避免 transport 固化某个观察者的
裸数字。保持旧的纯数值结构无法正确支持 namespace；把投影回调存入消息则会携带 OS 行为和
观察者状态，因而没有采用。

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

锁按状态域分层，不把互不嵌套的锁类声明成一个虚假的全局总序：

```text
PID 发布：publication gate
  -> PID namespace（root 到 leaf）
  -> PidIdentity

job control：Process.job_control
  -> Session
  -> PidIdentity role/binding（仅创建新 group）
  -> ProcessGroup membership（同时持有时按 PGID 递增）
  -> Process.group pointer

回收：Process.job_control
  -> parent.children
  -> ProcessGroup membership
```

session 弱索引 process group；group 强持有 session、弱索引 process；process 强持有
group。session registry 在创建新 group 时串行化相同 PGID 的竞争；`setsid`、`setpgid`、
group 迁移和 retire 由目标 process 的 `job_control` 串行化。父子关系的同类 `children` 锁按
祖先到后代嵌套，并且不与 identity/session 创建锁交叉持有。宽锁内禁止 wake、等待、用户
回调和可能触发跨对象析构的 `Drop`。需要释放 lease、文件对象或唤醒任务时先把值移出锁，
再在锁外执行。

## 创建、切换、exec 与关闭

### clone

`CloneTransaction` 先确定目标 namespace、预留完整 PID 链，并围绕 reservation 提供的
prepared identity 构造 suspended task、process/group/session topology、role lease、pidfd
和 cgroup 准备状态。地址空间、文件表、用户地址校验、child TID 以及 pidfd 数字 copyout 等
会中止事务的步骤全部完成后，才一次发布 identity links 和 namespace index；随后安装此前
隐藏的 pidfd、尝试写 `CLONE_PARENT_SETTID`，再执行 task/ptrace/cgroup 的不可失败提交并让
任务 runnable。parent/child view 的 PID 只计算一次并用于 clone 返回值与 TID 指针。

pidfd 使用 `PreparedFileDescriptor` 在父进程文件表中原子预留数字，但在 identity 发布前不
出现在普通查找、`fcntl`、`/proc/<pid>/fd` 或复制出的独立文件表中；预留计入
`RLIMIT_NOFILE`，普通 fd 分配跳过该数字，`dup2/dup3` 与预留竞争时返回 Linux 对应的
`EBUSY`。发布 identity 后才 install 文件对象。`CLONE_PARENT_SETTID` 与 Linux v7.1 一样在
任务可见后写入；若另一线程并发 unmap 令写入失败，不回滚已经发布的 child。

提交前错误由 reservation 和 `PreparedFileDescriptor` 各自的 `Drop` 回滚整条数字链与 fd
预留。若不可失败阶段违反内核不变量并发生 unwind，`CloneTransaction::Drop` 还会退出
topology，并对“已发布但尚未交给 scheduler 的 identity”执行非阻塞 abort：将 runtime、
publication 和 process lifecycle 依次收敛为 `Exited`、`Detached`、`Reaped`，再解除整条
namespace binding。这个路径只是不变量兜底，不承担等待、wake 或正常任务退出。

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

## 逐 syscall 标准映射

下表列出本次重构直接修改、或通过 PID identity、role selector、稳定 snapshot、prepared fd、
IPC metadata 与 Unix credential 间接受影响的 syscall。结论以 Linux v7.1
`8cd9520d35a6c38db6567e97dd93b1f11f185dc6` 为实现对照；man-pages 链接用于公开 ABI，
依赖内部状态时同时给出固定 commit 源码。仅把 `current().as_thread()` 等机械调用点替换、但
不改变参数、返回值或用户可见状态的 `mount`、`uname` 等入口不列入 affected surface；procfs、
trace pipe、perf record 与 eBPF helper 的非 syscall 文件 ABI 在前文分别说明。

| Syscall | 评审结论 | 对应标准 | 简要依据 |
| --- | --- | --- | --- |
| `getpid` | 符合 | [`getpid(2)`](https://man7.org/linux/man-pages/man2/getpid.2.html) | 返回调用者 active view 中的 TGID。 |
| `getppid` | 符合 | [`getpid(2)`](https://man7.org/linux/man-pages/man2/getpid.2.html) | parent snapshot 按调用者 active view 投影，不从可复用数字反查。 |
| `gettid` | 符合 | [`gettid(2)`](https://man7.org/linux/man-pages/man2/gettid.2.html) | 返回调用线程 identity 在 active view 中的 TID。 |
| `clone` | 符合（本次修复后） | [`clone(2)`](https://man7.org/linux/man-pages/man2/clone.2.html)、[Linux `copy_process`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c#L1876-L1889) | PID 链、pidfd 与 parent TID copyout 按提交边界发布；失败完整回滚。 |
| `clone3` | 符合（本次修复后） | [`clone(2)`](https://man7.org/linux/man-pages/man2/clone.2.html)、[Linux `copy_process`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c#L1876-L1889) | 与 `clone` 共享同一 typed transaction，flags 与输出沿原 ABI 解码。 |
| `fork` | 符合 | [`fork(2)`](https://man7.org/linux/man-pages/man2/fork.2.html) | 复用 clone transaction；child identity 在返回给 parent 前完成发布。 |
| `vfork` | 符合 | [`vfork(2)`](https://man7.org/linux/man-pages/man2/vfork.2.html) | 复用 clone transaction，同时保留既有 VM/parent wait 语义。 |
| `execve` | 符合 | [`execve(2)`](https://man7.org/linux/man-pages/man2/execve.2.html)、[Linux `de_thread`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/exec.c#L1101-L1260) | non-leader exec 接管 leader identity，TGID/process pidfd 不变。 |
| `execveat` | 符合 | [`execveat(2)`](https://man7.org/linux/man-pages/man2/execveat.2.html)、[Linux `de_thread`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/exec.c#L1101-L1260) | 与 `execve` 共享不可失败 identity transfer 提交。 |
| `wait4` | 符合 | [`wait(2)`](https://man7.org/linux/man-pages/man2/wait.2.html) | typed PID/PGID selector 与唯一 reap ownership 保留 zombie 到消费完成。 |
| `waitpid` | 符合 | [`wait(2)`](https://man7.org/linux/man-pages/man2/wait.2.html) | 正数、零、负 PGID 和任意 child 分支各自解析，不混用角色。 |
| `waitid` | 符合 | [`waitid(2)`](https://man7.org/linux/man-pages/man2/waitid.2.html) | `P_PID`/`P_PGID`/`P_PIDFD` 与 `WNOWAIT` 保持稳定 generation。 |
| `kill` | 符合 | [`kill(2)`](https://man7.org/linux/man-pages/man2/kill.2.html) | PID、当前组、任意进程和负 PGID 由专用 selector 解析。 |
| `tkill` | 符合 | [`tkill(2)`](https://man7.org/linux/man-pages/man2/tkill.2.html) | 只按 TID role 查找目标线程。 |
| `tgkill` | 符合 | [`tgkill(2)`](https://man7.org/linux/man-pages/man2/tkill.2.html) | 分别验证 TGID 与 TID role，拒绝错误 thread-group 组合。 |
| `rt_sigqueueinfo` | 符合 | [`rt_sigqueueinfo(2)`](https://man7.org/linux/man-pages/man2/rt_sigqueueinfo.2.html) | process-directed target 按 TGID 解析，排队 siginfo 捕获稳定发送者 snapshot。 |
| `rt_tgsigqueueinfo` | 符合 | [`rt_sigqueueinfo(2)`](https://man7.org/linux/man-pages/man2/rt_sigqueueinfo.2.html) | TGID+TID 组合解析且 siginfo PID 按接收者 namespace 投影。 |
| `pidfd_open` | 符合 | [`pidfd_open(2)`](https://man7.org/linux/man-pages/man2/pidfd_open.2.html) | TGID/TID role 错误区分 `ENOENT` 与 `ESRCH`，pidfd 强持有 generation。 |
| `pidfd_getfd` | 符合 | [`pidfd_getfd(2)`](https://man7.org/linux/man-pages/man2/pidfd_getfd.2.html) | 通过 pidfd identity 取得稳定目标 process，不按数字重新解析。 |
| `pidfd_send_signal` | 符合 | [`pidfd_send_signal(2)`](https://man7.org/linux/man-pages/man2/pidfd_send_signal.2.html) | zombie/reaped 状态从 pidfd generation 判断，不受数字复用影响。 |
| `getpgid` | 符合 | [`getpgid(2)`](https://man7.org/linux/man-pages/man2/getpgid.2.html) | 目标按 TGID 查找，结果投影其 PGID role。 |
| `getpgrp` | 符合 | [`getpgrp(2)`](https://man7.org/linux/man-pages/man2/getpgrp.2.html) | 返回当前 process group 的 typed PGID。 |
| `setpgid` | 符合（本次修复后） | [`setpgid(2)`](https://man7.org/linux/man-pages/man2/setpgid.2.html) | job-control 锁串行化 topology 检查、role 建立与成员迁移。 |
| `getsid` | 符合 | [`getsid(2)`](https://man7.org/linux/man-pages/man2/getsid.2.html) | 目标按 TGID 查找，返回 session 持有的 SID role。 |
| `setsid` | 符合（本次修复后） | [`setsid(2)`](https://man7.org/linux/man-pages/man2/setsid.2.html) | 并发调用恰好一个提交，role 冲突返回 `EPERM` 而非 panic。 |
| `unshare` | 符合 | [`unshare(2)`](https://man7.org/linux/man-pages/man2/unshare.2.html)、[`pid_namespaces(7)`](https://man7.org/linux/man-pages/man7/pid_namespaces.7.html) | `CLONE_NEWPID` 只替换 future-child namespace，不迁移调用者。 |
| `setns` | 符合 | [`setns(2)`](https://man7.org/linux/man-pages/man2/setns.2.html)、[`pid_namespaces(7)`](https://man7.org/linux/man-pages/man7/pid_namespaces.7.html) | PID namespace fd 只更新 `pid_ns_for_children`。 |
| `sched_getaffinity` | 符合 | [`sched_setaffinity(2)`](https://man7.org/linux/man-pages/man2/sched_setaffinity.2.html) | `pid=0` 与正 TID 由 scheduler selector 区分。 |
| `sched_setaffinity` | 符合 | [`sched_setaffinity(2)`](https://man7.org/linux/man-pages/man2/sched_setaffinity.2.html) | target 解析使用 TID role，不把 TGID/PGID 混入。 |
| `sched_getscheduler` | 符合 | [`sched_setscheduler(2)`](https://man7.org/linux/man-pages/man2/sched_setscheduler.2.html) | `pid=0` 代表调用线程，正数代表可见 TID。 |
| `sched_setscheduler` | 符合 | [`sched_setscheduler(2)`](https://man7.org/linux/man-pages/man2/sched_setscheduler.2.html) | policy 操作绑定稳定目标线程 identity。 |
| `sched_getparam` | 符合 | [`sched_getparam(2)`](https://man7.org/linux/man-pages/man2/sched_getparam.2.html) | 参数读取目标由 typed TID selector 决定。 |
| `getpriority` | 符合 | [`getpriority(2)`](https://man7.org/linux/man-pages/man2/getpriority.2.html) | process、process group 与 user selector 不再共享裸 PID parser。 |
| `setpriority` | 符合 | [`setpriority(2)`](https://man7.org/linux/man-pages/man2/setpriority.2.html) | `PRIO_PROCESS`/`PRIO_PGRP` 分别使用 TGID/PGID role。 |
| `capget` | 符合（本次修复后） | [`capget(2)`](https://man7.org/linux/man-pages/man2/capget.2.html) | PID 目标按调用者 view 解析，负 PID 与不存在 PID 的 errno 已按 Linux 固定。 |
| `capset` | 符合 | [`capset(2)`](https://man7.org/linux/man-pages/man2/capget.2.html) | target TGID 查找不依赖调度器 `TaskId`。 |
| `prlimit64` | 符合 | [`prlimit(2)`](https://man7.org/linux/man-pages/man2/getrlimit.2.html) | `pid=0` 与可见目标 TGID 明确区分，并持有稳定 process。 |
| `ptrace` | 符合 | [`ptrace(2)`](https://man7.org/linux/man-pages/man2/ptrace.2.html) | target 按 TID role；exec/exit event 捕获稳定 identity snapshot。 |
| `process_vm_readv` | 符合 | [`process_vm_readv(2)`](https://man7.org/linux/man-pages/man2/process_vm_readv.2.html) | target 参数按 TID role 解析并绑定稳定地址空间 owner。 |
| `process_vm_writev` | 符合 | [`process_vm_writev(2)`](https://man7.org/linux/man-pages/man2/process_vm_readv.2.html) | 与 readv 使用相同 TID/view 边界。 |
| `get_robust_list` | 符合（本次修复后） | [`get_robust_list(2)`](https://man7.org/linux/man-pages/man2/get_robust_list.2.html) | 目标按 TID 解析；退出 owner word 使用用户 active-view TID。 |
| `set_tid_address` | 符合 | [`set_tid_address(2)`](https://man7.org/linux/man-pages/man2/set_tid_address.2.html) | clear-child-tid 的返回值和退出清理由当前 thread identity 提供。 |
| `perf_event_open` | 符合（本次修复后） | [`perf_event_open(2)`](https://man7.org/linux/man-pages/man2/perf_event_open.2.html) | `pid=-1/0/>0` 使用专用 selector；record PID/TID 按 event observer view 投影。 |
| `mq_notify` | 符合 | [`mq_notify(2)`](https://man7.org/linux/man-pages/man2/mq_notify.2.html) | `SIGEV_THREAD_ID` 目标保存稳定 TID identity/snapshot。 |
| `timer_create` | 符合 | [`timer_create(2)`](https://man7.org/linux/man-pages/man2/timer_create.2.html) | timer owner 与 `SIGEV_THREAD_ID` 不保存可复用裸数字。 |
| `timer_settime` | 符合 | [`timer_settime(2)`](https://man7.org/linux/man-pages/man2/timer_settime.2.html) | timer lookup 继续使用 timer ID，通知 owner 则来自创建时的稳定 identity。 |
| `msgget` | 符合（本次修复后） | [`msgget(2)`](https://man7.org/linux/man-pages/man2/msgget.2.html) | 新 queue 的 last-sender/receiver snapshot 为 `None`，ABI PID 初值为 0。 |
| `msgsnd` | 符合 | [`msgsnd(2)`](https://man7.org/linux/man-pages/man2/msgsnd.2.html) | 完成发送时捕获发送者 identity snapshot。 |
| `msgrcv` | 符合 | [`msgrcv(2)`](https://man7.org/linux/man-pages/man2/msgrcv.2.html) | 完成接收时捕获接收者 identity snapshot。 |
| `msgctl` | 符合（本次修复后） | [`msgctl(2)`](https://man7.org/linux/man-pages/man2/msgctl.2.html) | `msg_lspid/msg_lrpid` 按 observer view 投影，`IPC_SET` 只更新允许字段。 |
| `shmget` | 符合（本次修复后） | [`shmget(2)`](https://man7.org/linux/man-pages/man2/shmget.2.html) | 创建者 snapshot 与 last-operator 状态分离，lookup 不伪造一次 shmop。 |
| `shmat` | 符合 | [`shmat(2)`](https://man7.org/linux/man-pages/man2/shmat.2.html) | attach 完成后才捕获 last-operator snapshot 并更新 attach count。 |
| `shmdt` | 符合 | [`shmdt(2)`](https://man7.org/linux/man-pages/man2/shmdt.2.html) | detach 按稳定 process generation 归属映射并更新 last operator。 |
| `shmctl` | 符合（本次修复后） | [`shmctl(2)`](https://man7.org/linux/man-pages/man2/shmctl.2.html)、[Linux `shmctl_down`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/ipc/shm.c#L995-L1034) | PID 字段按 observer view 投影；`IPC_SET` 仅更新 UID/GID/权限并保留只读统计。 |
| `bind` | 符合 | [`bind(2)`](https://man7.org/linux/man-pages/man2/bind.2.html)、[`netlink(7)`](https://man7.org/linux/man-pages/man7/netlink.7.html) | Netlink 自动 port ID 使用调用者 TGID 数值，但仍是独立 port-ID domain。 |
| `socket` | 符合（本次修复后） | [`socket(2)`](https://man7.org/linux/man-pages/man2/socket.2.html)、[`unix(7)`](https://man7.org/linux/man-pages/man7/unix.7.html) | Unix socket 创建时保存稳定 process generation 与数值 fallback。 |
| `socketpair` | 符合（本次修复后） | [`socketpair(2)`](https://man7.org/linux/man-pages/man2/socketpair.2.html)、[Linux `unix_socketpair`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/net/unix/af_unix.c#L1808-L1836) | 两端 peer credential 强持有同一创建者 generation。 |
| `connect` | 符合（本次修复后） | [`connect(2)`](https://man7.org/linux/man-pages/man2/connect.2.html)、[Linux Unix connect credentials](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/net/unix/af_unix.c#L1628-L1777) | stream/seqpacket connection 交换稳定 client/listener identity。 |
| `listen` | 符合（本次修复后） | [`listen(2)`](https://man7.org/linux/man-pages/man2/listen.2.html)、[Linux `unix_listen`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/net/unix/af_unix.c#L809-L839) | listener peer credential 捕获 generation，后续 connect clone 该引用。 |
| `accept` | 符合（本次修复后） | [`accept(2)`](https://man7.org/linux/man-pages/man2/accept.2.html) | accepted socket 保留连接方稳定 credential。 |
| `accept4` | 符合（本次修复后） | [`accept4(2)`](https://man7.org/linux/man-pages/man2/accept.2.html) | 与 `accept` 共享 peer identity 语义，同时保留 flags。 |
| `sendto` | 符合（本次修复后） | [`sendto(2)`](https://man7.org/linux/man-pages/man2/send.2.html)、[Linux Unix SCM enqueue](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/net/unix/af_unix.c#L1987-L2006) | 发送时捕获调用进程 generation，消息排队后不依赖裸 PID。 |
| `sendmsg` | 符合（本次修复后） | [`sendmsg(2)`](https://man7.org/linux/man-pages/man2/sendmsg.2.html)、[Linux Unix SCM enqueue](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/net/unix/af_unix.c#L1987-L2006) | 自动凭据与显式 payload 经过同一稳定 credential 边界。 |
| `write` | 符合（本次修复后） | [`write(2)`](https://man7.org/linux/man-pages/man2/write.2.html)、[Linux Unix SCM enqueue](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/net/unix/af_unix.c#L1987-L2006) | Unix datagram write 路径同样附加发送者 generation。 |
| `recvmsg` | 符合（本次修复后） | [`recvmsg(2)`](https://man7.org/linux/man-pages/man2/recvmsg.2.html)、[`scm_set_cred`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/include/net/scm.h#L69-L80) | `SCM_CREDENTIALS` 在写回时按接收者 active view 投影。 |
| `setsockopt` | 符合 | [`setsockopt(2)`](https://man7.org/linux/man-pages/man2/setsockopt.2.html)、[`unix(7)`](https://man7.org/linux/man-pages/man7/unix.7.html) | `SO_PASSCRED` 只控制接收方是否要求凭据，不固化 PID view。 |
| `getsockopt` | 符合（本次修复后） | [`getsockopt(2)`](https://man7.org/linux/man-pages/man2/getsockopt.2.html)、[Linux `SO_PEERCRED`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/net/core/sock.c#L1903-L1915) | `SO_PEERCRED` 保存稳定 pid，并在查询者 namespace 中经 `pid_vnr` 投影。 |
| `io_setup` | 符合 | [`io_setup(2)`](https://man7.org/linux/man-pages/man2/io_setup.2.html) | AIO context owner 使用不可复用 `PidIdentityId`，不改变 context ID ABI。 |
| `io_destroy` | 符合 | [`io_destroy(2)`](https://man7.org/linux/man-pages/man2/io_destroy.2.html) | context ownership 按创建者 generation 核对，避免 PID 复用后误认领。 |
| `io_submit` | 符合 | [`io_submit(2)`](https://man7.org/linux/man-pages/man2/io_submit.2.html) | submit 只进入当前 generation 所属 context。 |
| `io_getevents` | 符合 | [`io_getevents(2)`](https://man7.org/linux/man-pages/man2/io_getevents.2.html) | event consumption 绑定稳定 context owner。 |
| `io_pgetevents` | 符合 | [`io_pgetevents(2)`](https://man7.org/linux/man-pages/man2/io_pgetevents.2.html) | 与 `io_getevents` 共用 generation ownership，同时保留临时 signal mask。 |
| `io_cancel` | 符合 | [`io_cancel(2)`](https://man7.org/linux/man-pages/man2/io_cancel.2.html) | cancel 不会命中复用 PID 的旧 AIO context。 |
| `open` | 符合（本次修复后） | [`open(2)`](https://man7.org/linux/man-pages/man2/open.2.html) | 普通 fd 分配跳过尚未发布的 clone pidfd reservation。 |
| `openat` | 符合（本次修复后） | [`openat(2)`](https://man7.org/linux/man-pages/man2/openat.2.html) | 与 `open` 共用保留槽感知的 fd allocator。 |
| `openat2` | 符合（本次修复后） | [`openat2(2)`](https://man7.org/linux/man-pages/man2/openat2.2.html) | 与 `openat` 共用保留槽感知的 fd allocator。 |
| `creat` | 符合（本次修复后） | [`creat(2)`](https://man7.org/linux/man-pages/man2/creat.2.html) | 创建 fd 不会取得 prepared pidfd 的隐藏数字。 |
| `close` | 符合（本次修复后） | [`close(2)`](https://man7.org/linux/man-pages/man2/close.2.html) | 未发布 reservation 对普通 fd lookup 不可见，不能被并发 close 解除。 |
| `close_range` | 符合（本次修复后） | [`close_range(2)`](https://man7.org/linux/man-pages/man2/close_range.2.html) | 批量关闭跳过隐藏 reservation，clone 失败仍由 transaction 回滚。 |
| `dup` | 符合（本次修复后） | [`dup(2)`](https://man7.org/linux/man-pages/man2/dup.2.html) | 新 fd 分配跳过隐藏 reservation。 |
| `dup2` | 符合（本次修复后） | [`dup2(2)`](https://man7.org/linux/man-pages/man2/dup.2.html) | 定向覆盖与 prepared pidfd 竞争时返回 Linux 对应的 `EBUSY`。 |
| `dup3` | 符合（本次修复后） | [`dup3(2)`](https://man7.org/linux/man-pages/man2/dup.2.html) | 与 `dup2` 使用相同 reservation 冲突判断并保留 flags。 |
| `fcntl` | 符合（本次修复后） | [`fcntl(2)`](https://man7.org/linux/man-pages/man2/fcntl.2.html)、[`dup2(2)`](https://man7.org/linux/man-pages/man2/dup.2.html) | file-lock owner 使用 generation；clone 隐藏 fd reservation 不可查，定向覆盖返回 `EBUSY`。 |
| `flock` | 符合 | [`flock(2)`](https://man7.org/linux/man-pages/man2/flock.2.html) | process-scoped lock owner 不再由可复用数字标识。 |
| `poll` | 符合 | [`poll(2)`](https://man7.org/linux/man-pages/man2/poll.2.html)、[Linux `pidfd_poll`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/pidfs.c#L304-L321) | pidfd zombie/reap readiness 来自稳定 identity lifecycle。 |
| `ppoll` | 符合 | [`ppoll(2)`](https://man7.org/linux/man-pages/man2/poll.2.html)、[Linux `pidfd_poll`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/pidfs.c#L304-L321) | 与 `poll` 使用同一 pidfd event source。 |
| `epoll_wait` | 符合 | [`epoll_wait(2)`](https://man7.org/linux/man-pages/man2/epoll_wait.2.html)、[Linux `pidfd_poll`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/pidfs.c#L304-L321) | pidfd exit/reap wakeup 不依赖 namespace 数字仍可解析。 |
| `bpf` | 符合 | [`bpf(2)`](https://man7.org/linux/man-pages/man2/bpf.2.html)、[`bpf-helpers(7)`](https://man7.org/linux/man-pages/man7/bpf-helpers.7.html) | current PID/TGID helper 从 Starry identity 投影，不泄漏 ax-task `TaskId`。 |

## PR #1775 开发历程回溯

PR [#1775](https://github.com/rcore-os/tgoskits/pull/1775) 的 250 个提交、issue discussion
与 review 记录包含一次更大范围的 task/runtime 重构。这里不移植其 scheduler 实现，而把其中
已经通过实际应用或后续提交暴露的 PID 身份风险当作本次重构的独立审查输入。逐项映射如下：

| #1775 历史节点 | 对本分支的结论与证据 |
| --- | --- |
| [`c1056638f`](https://github.com/rcore-os/tgoskits/commit/c1056638f)、[`08ade2341`](https://github.com/rcore-os/tgoskits/commit/08ade2341)：final exit、leader/runtime 所有权 | 已覆盖。`PidIdentity` 将 runtime exit、process zombie 与唯一 reap 分离；kernel axtest 和 `syscallguard-final-wait-runtime`、zombie/waitid grouped case 覆盖 leader runtime 先退出的路径。 |
| [`e16e76234`](https://github.com/rcore-os/tgoskits/commit/e16e76234)、[`deadb9f27`](https://github.com/rcore-os/tgoskits/commit/deadb9f27)：PID namespace、job control、reaped group leader | 已覆盖。PGID/SID role lease 独立于 TGID reap，`pgid_and_sid_roles_keep_a_reaped_number_published` 固定 reaped group leader；并发 `setsid()` 红测另行固定 topology 提交竞争。 |
| [`ea481cde2`](https://github.com/rcore-os/tgoskits/commit/ea481cde2)、[`2a218e954`](https://github.com/rcore-os/tgoskits/commit/2a218e954)、[`71c02b7b0`](https://github.com/rcore-os/tgoskits/commit/71c02b7b0)：SIGCHLD、cgroup、proc observer view | 已覆盖。异步数据捕获稳定 snapshot/view，完整 x86_64 grouped 回归覆盖 signal、cgroup 与 proc PID namespace 路径。 |
| [`50d708d89`](https://github.com/rcore-os/tgoskits/commit/50d708d89)、[`01e9322cc`](https://github.com/rcore-os/tgoskits/commit/01e9322cc)、[`503c8da55`](https://github.com/rcore-os/tgoskits/commit/503c8da55)、[`dbc60c88a`](https://github.com/rcore-os/tgoskits/commit/dbc60c88a)：ptrace、robust list、file lock、perf target | 已覆盖。对应 typed selector/snapshot 已进入本设计，`ptrace`、robust owner-death、file lock 与 perf grouped case 均由完整 x86_64 回归执行。 |
| [`bcb4e009d`](https://github.com/rcore-os/tgoskits/commit/bcb4e009d)：Unix socket stable credentials | 发现本分支缺口并作为红测。嵌套 PID namespace 发送者在外层接收方观察到的 `SCM_CREDENTIALS` 与 `SO_PEERCRED` 曾错误返回内层 PID 1；同一回归修复后均返回外层 child PID，并继续覆盖发送者 reap 后的排队凭据。 |
| [`ffe3cd478`](https://github.com/rcore-os/tgoskits/commit/ffe3cd478)：generic PID boundary 与 `shmctl(IPC_SET)` | PID selector 部分已覆盖；`IPC_SET` 审计发现新的确定性缺口：旧实现把整个 `shmid_ds` 从用户态复制进内核，允许伪造 creator、size、operation PID 与 attach count。现仅更新 UID/GID/权限并在锁外访问用户内存。 |

#1775 当前 head 的 CI 包含失败和 fail-fast cancellation，因此没有把它视为本分支的通过证据；
只采用可追溯的历史问题、具体提交和能够在本分支独立红绿的测试。scheduler policy、runtime
ownership API 与 std profile review 属于 #1775 自身 surface，本次 PID identity 重构不修改这些
边界，标记为不适用。

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

审查阶段又补充了五组直接针对提交边界的红绿证据：

- 并发 `setsid()` 原先在两个线程都通过 session-leader 检查后，由第二个线程在
  `Session::new` 的 SID role `expect` 触发 kernel panic。axtest 先稳定复现该 panic；修复用
  process 级 `job_control` 锁串行化检查与 topology 转换，role 冲突返回 `EPERM`。用户态 8
  线程 barrier 回归确认恰好一个成功、其余 7 个返回 `EPERM`，会话 syscall case 为 19/19。
- clone 原先在 PID identity 发布前把 pidfd 安装进共享文件表，并提前写
  `CLONE_PARENT_SETTID`。axtest 先证明 prepared descriptor 可被普通查找观察；修复加入隐藏
  fd reservation，覆盖不可见性、限额计数、普通分配跳过、独立文件表 clone 不继承、失败
  回滚复用与最终 install，kernel QEMU axtest 为 407/407。
- SysV IPC 的稳定快照重构把创建者错误初始化为消息队列“最后发送/接收者”和共享内存“最后
  操作者”。新增用户态断言先得到 msgctl 19/21、shm-family 92/93 红灯；改为显式可空快照后
  新对象按 Linux ABI 报告 `msg_lspid=0`、`msg_lrpid=0`、`shm_lpid=0`，同一用例分别为
  21/21、93/93。
- PR #1775 的 `shmctl(IPC_SET)` 历史促使复核同一 ABI。用户态把 creator UID/GID、segment
  size、creator/last-op PID 与 attach count 写成伪值后，旧实现错误接受其中 5 个只读字段，
  同一 shm-family case 为 94/99（5 项失败）。修复先在锁外完整读取输入，再仅应用 owner UID/GID 与权限
  低 9 位；同一 case 为 99/99，真实 attach/detach 统计不再被伪值污染。
- PR #1775 的 Unix stable credential 历史促使增加跨 PID namespace 回归。嵌套 namespace
  init 以内部 PID 1 建立 stream peer 并发送 datagram，外层接收者的 `SO_PEERCRED` 和
  `SCM_CREDENTIALS` 原先均错误报告 PID 1，同一核心断言为 36/38（2 项失败）。transport 改为持有稳定
  identity、用户写回时按接收者 view 投影后，同一核心断言为 38/38；再加入发送者已被
  `waitpid` 回收后读取排队凭据的历史保护，并显式验证 namespace sender 的 fork 成功，最终
  case 为 41/41。

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

继续回看 PR #1775 的开发链后，`e747f9d72`（`fix(starry-perf): publish dynamic rdpmc
metadata`）与本分支新进入的 per-task counting 路径直接重叠。Linux
[`perf_event_open(2)`](https://man7.org/linux/man-pages/man2/perf_event_open.2.html) 与固定提交
[`dac3e89a2c90c2feeb471e1f22a2512ad424b792`](https://github.com/torvalds/linux/blob/dac3e89a2c90c2feeb471e1f22a2512ad424b792/kernel/events/core.c#L6825-L6865)
中的 `perf_event_update_userpage()` 在每个调度片边界用 sequence、`index` 和 `offset` 发布
`count = offset + rdpmc(index - 1)`；inactive 时必须把 `index` 清零并把完整计数保留在
`offset`。本分支旧页始终固定为 `index=1, offset=0, lock=0`，CI 中 `rdpmc` 与
`read(fd)` 相差约 33 倍；确定性红测进一步得到 inactive `index=1, offset=0`。修复让
scheduler hooks 发布原子奇偶 sequence，active 页暴露稳定 programmable index，inactive
页只暴露累计 offset；测试用 `usleep(10000)` 必然形成 sched-out/in，并断言 active
offset 已保留上一切片，得到 `index: 0 -> 1 -> 0`，最终 mmap count 与 `read(fd)`
精确相等，直接覆盖跨调度片累计。

同一历史提交还提供了 metadata VMA 的安全与生命周期红测。旧实现错误接受 8192-byte
mmap（实际只分配一页）；收紧为 4096 后，`sys_mmap` 又把设备返回的 `EINVAL` 吞掉并落入
普通文件 fallback；透传错误后，旧强引用仍使 `munmap` 后重映射返回 `EBUSY`；最后
`RESET` 只清 accumulator、不更新 mmap offset。最终实现仅把 VMA 的强引用作为页所有者，
event/scheduler 侧保存可升级的弱引用，第二个 live mmap 保持 `EBUSY`，`munmap` 后可重新
mmap；`DeviceMmap::None` 成为唯一 file fallback 标记，设备错误原样返回；disabled
`RESET` 同步把 mmap 和 fd 计数清零。上述阶段均用同一
`cargo xtask starry test qemu --arch aarch64 -c qemu/system/perf-hw-rdpmc` 取得红/绿证据。

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

审查修复后的本地最终验证包括：`cargo xtask test` 的 44 个 std package 全部通过，x86_64
`starry-kernel` QEMU axtest 407/407，完整 x86_64 Starry QEMU 的 system 430/430 且 4/4 case
通过；聚焦 grouped case 中 session syscalls 为 19/19、msgctl 为 21/21、shm-family 为
99/99、Unix passcred 为 41/41。`ax-net` 的 3 个与 `starry-kernel` 的 25 个 clippy 配置、
sync-lint、fmt 和 diff check 同样全部通过。非本地架构的完整 grouped QEMU 仍由
必需 CI gate 验证；queued、cancelled 或 fail-fast 跳过的任务不计为成功。
