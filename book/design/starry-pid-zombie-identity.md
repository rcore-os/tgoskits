# Starry 稳定 PID 与 zombie 身份

## 文档状态

本文记录 Starry 进程生命周期修复、Linux 对照和确定性回归证据。生命周期只由一个稳定的 PID generation 状态机拥有；`Process` 仅负责线程组与关系拓扑。

后续 rebase 一律以 dev 的 `ProcessIdentity` 实现为准，不重新引入第二套 PID/zombie registry。

## 原问题

旧实现用两个独立 registry 表示一个 Linux 进程：

- `PROCESS_TABLE` 在 runtime resource 存活时保存弱 `ProcessData`；
- `ZOMBIE_TABLE` 从 final exit 到 wait 消费期间保存另一份强 `Process` snapshot。

live 阶段打开的 pidfd 保存 `ProcessData` event，exit 后打开的 pidfd 又创建私有 event，导致：

1. numeric PID lookup 跨两个锁完成，live/zombie 边界不原子；
2. pidfd 本身不保留 generation-specific `Process` identity；
3. 只监听 `EPOLLRDNORM` 的 epoll 看不到 zombie readiness；
4. reap 删除 zombie entry 后，不唤醒等待 `EPOLLHUP` 的旧 pidfd；
5. 多个 waiter 可重复执行非原子的 free/registry cleanup；
6. numeric PID 复用后，旧 pidfd 可能误认新进程；
7. 关系拓扑退休期间 PID 可能过早再次分配。

旧测试用 `kill(child, 0)` 证明 zombie，但这只证明 PID 存在，不能确定“已退出且未 reap”，也无法稳定覆盖上述状态转换。

## Linux 与 PREEMPT_RT 参考

原修复参考 Linux `v7.2-rc4` 提交 `1590cf0329716306e948a8fc29f1d3ee87d3989f` 及 `7.2-rc4-rt3`。RT patch 未改变相关 `pid`、pidfs、exit、signal 生命周期，因此主线语义同样适用。

- `struct pid` 是引用计数 generation identity，并拥有 `wait_pidfd`；numeric PID 复用会得到不同对象；
- `pidfd_poll()` 在可观察 exit 时返回 `EPOLLIN | EPOLLRDNORM`，reap detach 后再加 `EPOLLHUP`；
- `do_notify_pidfd()` 发布 exit readiness；
- `__unhash_process()` 在 reap 时 detach 并唤醒稳定 pidfd wait queue；
- `wait_task_zombie()` 保持 `WNOWAIT` 非消费，并用原子状态转换选出唯一消费 waiter；
- `pidfd_send_signal()` 解析稳定 PID object：未 reap zombie 对 signal 0 和允许的非零 signal 仍可解析，reap 后返回 `ESRCH`。

Linux v7.1 的 `exit_notify()`、`do_wait()`、`copy_process()`、PID namespace 分配与 reparent 顺序用于后续关系审计。PREEMPT_RT 改变可睡眠锁实现，但不改变 PID generation 与 wait 语义。

## 状态模型

```text
Live {
    Weak<ProcessData>
}
  |
  | final thread 在 PID registry write transaction 中发布 exit
  v
Zombie {
    Arc<Process>,
    Arc<PollSet>,
    frozen credentials,
    wait metadata,
    frozen CPU time
}
  |
  | 一个消费 wait 以 exact Arc identity claim
  v
Reaping
  |
  | 退休 parent/group/session 拓扑，PID 仍被 reservation 占用
  v
Reaped
```

numeric PID 只用于查表。`Arc<ProcessIdentity>` 才是 generation identity；长生命周期 pidfd 或 waiter 校验时必须比较精确 identity，不能只比较数字。

所有权拆分：

- `ProcessData`：live runtime resources，如 address space、files、credentials view；
- `ProcessIdentity`：稳定 Linux 可见身份与 lifecycle；
- `Process`：thread group 与 parent/group topology；
- PID registry：同一 generation 在任一时刻只表示一种 lifecycle state；
- `PollSet`：从 live 构造一直跟随 identity 到 zombie/reap。

## 关系事务

parent、children、process-group、session-group 的写操作进入统一 `ProcessRelationTxn`：

1. 进入锁前预留可能增长的容器容量；
2. parent child-set 按稳定 PID 排序加锁；
3. process-group member-set 按稳定 PGID 排序加锁；
4. 临界区内不分配、不执行 callback；
5. 同一事务关闭 child publication、选择 reaper、移动 child 和更新 group；
6. 被移除的 `Arc/Weak` 在释放锁后 drop。

这样避免 reparent/retire 锁序反转，也避免“先从 source 删除，再等 destination lock”期间对象在两个 group 都不可见。

候选 subreaper 在 publication 时必须再次验证其 child publication 仍开放；若候选同时退出，则重新选择 live ancestor 或 namespace init。

## 已实现边界

当前实现遵守：

- 删除 `starry-process::Process` 中重复 lifecycle flag；
- `ProcessData` 构造时同时创建稳定 `ProcessIdentity`；
- final thread exit 为 one-shot，并冻结每线程累计 CPU time；
- 在唯一 `Live -> Zombie` publication 前关闭 child publication 并完成 reparent；
- reaper 选择与 child move 属于同一关系事务；
- freeze credentials、wait metadata、process CPU time 到 zombie snapshot；
- consuming wait 通过 exact identity 唯一 claim `Zombie -> Reaping`；
- 只有 winner 计入 child CPU time、退休 topology 和释放 PID；
- `Reaping` 仍占用 numeric PID，但不再允许公开 open/lookup；
- public lookup 在 PID registry read lock 内同时检查 state，与 write-locked reap claim 线性化；
- pidfd 保存 `Arc<ProcessIdentity>`，live 和 post-exit pidfd 共用同一 exit event；
- zombie poll 返回 `IN | RDNORM`，reap 后再返回 `HUP`；
- exit 唤醒 `RDNORM`，唯一 reap 路径唤醒 `HUP`；
- wait 先向用户复制 status，再尝试 consuming transition；
- `WNOWAIT` 只观察不消费；
- pidfd wait 匹配 generation，不匹配复用后的 numeric PID；
- clone 的 PID/task/scheduler publication 使用准备事务，失败逆序回滚。

## fork 与可见性

Linux `copy_process()` 在 task 可见前完成 fallible 准备，并明确规定可见点之后不得失败。Starry 对应顺序：

1. 预留所有 PID namespace ID；
2. 准备 stack、TLS、context、address space、cgroup、scheduler identity；
3. scheduler stage 成功，但 child entry gate 保持关闭；
4. 在 publication gate 下发布 PID maps、relationships、TASK_TABLE、PROCESS_TABLE；
5. commit rollback token；
6. 释放 publication gate；
7. infallible activate child entry。

任何 placement/admission 失败都必须发生在用户可见 PID/TID 之前。

## PID namespace

每个 `ProcessIdentity` 保存不可变的 namespace lineage。clone 在 innermost 到 root 的每层都预留 local PID，匹配 Linux `alloc_pid()` 的多层 `upid` 模型。

namespace shutdown 与 clone final publication 共用 gate：

- shutdown winner 关闭 allocation 并使 prepared clone 回滚；
- clone winner 完整发布后，shutdown 才能枚举它；
- reserved entry 在 shutdown membership predicate 中可见，失败 clone rollback 时推进 member epoch；
- reparent 只选择同 namespace live subreaper，最终退到该 namespace 稳定 init identity；
- shutdown 反复采样 monotonic member epoch，处理后到达的 zombie，直到 live/reserved/zombie 均完成。

完整的 namespace-relative PID ABI 翻译仍需独立完成。不能只改 `clone()` 返回值而不同时迁移 wait、signal、sched、ptrace 和 lookup resolver。

## pidfd 与 wait 行为

| ABI | 要求 |
| --- | --- |
| `pidfd_open` | 原子解析一个 live 或 zombie generation；reap claim 后失败 |
| `pidfd_send_signal` | zombie 在 reap 前可解析；旧 pidfd 在 reap 后返回 `ESRCH` |
| `wait4`/`waitpid` | 先复制 status，再由一个 waiter 消费 zombie |
| `waitid` | `WNOWAIT` 非消费；普通 wait 唯一 reap |
| `poll`/`ppoll` | zombie 为 `POLLIN | POLLRDNORM`；reaped pidfd 再有 `POLLHUP` |
| `epoll_wait` | 单独监听 `EPOLLRDNORM` 可见 exit；reap 唤醒 `EPOLLHUP` waiter |
| `getpgid`/`getsid` | zombie 仍可见，进入 `Reaping` 后返回 `ESRCH` |

signal 0 和允许的非零 signal 在未 reap zombie 上保持 Linux 行为，不改变已冻结的 exit status。

## 确定性红测

原始 QEMU 命令：

```bash
cargo xtask starry test qemu --arch x86_64 \
  -c qemu/system/syscall-test-pidfd-send-signal
```

旧实现的正式 runner 为 `STARRY_GROUPED_TESTS_FAILED`：71 项通过、7 项失败。失败包括：

- `EPOLLRDNORM`-only interest 看不到未 reap zombie；
- event mask 缺 `EPOLLRDNORM`；
- reap 前已注册的 `EPOLLHUP` waiter 超时；
- reap 不发布 `EPOLLHUP`；
- post-reap poll/epoll mask 不符合 Linux。

额外红测：

- `repeated_thread_exit_does_not_report_last_twice`：重复移除同 TID 再次报告 last-thread；
- `syscall-test-waitid-pidfd`：被消费 child 的 frozen CPU time 未计入 parent；
- `reaping_identity_is_not_publicly_resolvable`：在 `Zombie -> Reaping` claim 后设置 test barrier，旧实现仍允许 `get_process/getpgid/getsid` 解析已消费 PID；
- late fork publication：父进程关系退出 snapshot 后仍可挂入新 child；
- destination group lock contention：旧 move 顺序使 process 暂时不属于任何 group；
- namespace subreaper 退出 race：已关闭的 reaper 仍接收 child。

这些测试均要求修复前稳定失败，修复后使用同一断言通过。

## 验证证据

历史修复里程碑包括：

- `cargo test -p starry-process`：27 项通过；
- `cargo xtask clippy --package starry-process`：1/1；
- `cargo xtask clippy --package starry-kernel`：当时 22/22 feature checks；
- `reaping_identity_is_not_publicly_resolvable` 在 Starry kernel axtest 中通过；
- host Linux waitid/pidfd：66/66；
- x86_64 `syscall-test-waitid-pidfd`：66/66，正式 marker 通过；
- x86_64 `syscall-test-pidfd-send-signal`：78/78，覆盖 zombie readiness 与 post-reap HUP。

这些是对应历史提交的证据。每次 rebase 或生命周期冲突后，必须重新运行当前 head 的定向 test/clippy；CI pending、cancelled 或旧 head 的结果都不能作为当前通过。

## 已知相邻问题

除非修复稳定 identity 所必需，以下问题保持独立：

- `waitid(P_PIDFD)` 尚未把 `PIDFD_NONBLOCK` 完整映射为 nonblocking wait；
- `pidfd_getfd` permission 仍近似 kill-style，而非 Linux `ptrace_may_access(PTRACE_MODE_ATTACH_REALCREDS)`；
- thread-pidfd 的 `siginfo` self-check 需要 TID/TGID 审计；
- 完整 PID namespace resolver 与 numeric PID ABA stress；
- `Reaping` 内部 barrier 无法从 userspace 确定控制，因此 syscall suite 只保留 ABI 覆盖，内部线性化由 kernel deterministic test 保证。

## 完成条件

1. `ProcessIdentity` 仍是唯一 PID/zombie/reap authority；
2. public lookup 不可解析 `Reaping/Reaped`；
3. pidfd 保留 exact generation，PID 复用不能形成 ABA；
4. child publication、reparent、group move 与 final exit 具有统一锁序；
5. clone 可见点之后没有 recoverable failure；
6. wait/pidfd/poll Linux ABI 定向 QEMU 通过；
7. Starry process/kernel clippy 和当前 PR CI terminal 通过。
