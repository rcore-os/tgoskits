# PR 描述

**标题**: feat(prctl): implement PR_SET_PDEATHSIG and PR_GET_PDEATHSIG with signal delivery

## Bug 描述

StarryOS 未实现 `PR_SET_PDEATHSIG` 和 `PR_GET_PDEATHSIG`（参考 `prctl(2)`）。PostgreSQL 的后台工作进程（checkpointer、bgwriter、walwriter 等）在 `fork` 后通过 `prctl(PR_SET_PDEATHSIG, SIGTERM)` 注册"父进程死亡信号"，使进程在 postmaster 意外崩溃时自动终止。StarryOS 上该调用无效，父进程退出后子进程成为孤儿，不会接收任何信号，导致资源泄漏和 PostgreSQL 重启失败。

## 根本原因分析

`sys_prctl` 中 `PR_SET_PDEATHSIG`/`PR_GET_PDEATHSIG` 命令无 match arm，走到默认分支被静默忽略。内核没有存储 pdeathsig 值的字段，也没有在进程退出时向子进程投递信号的逻辑。Linux 内核在 `task_struct` 中维护 `pdeath_signal` 字段（见 `linux/sched.h`），在 `do_exit` 路径通过 `forget_original_parent` 向设置了 pdeathsig 的子进程发送信号。

## 修复方案

**`os/StarryOS/kernel/src/task/mod.rs`**

`Thread` 结构体新增 `pdeathsig: AtomicU32` 字段，初始值为 0（不发送信号）。提供 `pdeathsig()` 和 `set_pdeathsig(sig: u32)` 两个方法。

**`os/StarryOS/kernel/src/syscall/task/ctl.rs`**

`sys_prctl` 新增两个 match arm：
- `PR_SET_PDEATHSIG`：校验信号编号不超过 64，调用 `current().as_thread().set_pdeathsig(sig)`。
- `PR_GET_PDEATHSIG`：读取当前值，通过 `vm_write` 写入用户空间指针。

**`os/StarryOS/kernel/src/task/ops.rs`**

在 `do_exit` 中，进程退出通知子进程的逻辑之后，遍历当前进程的所有子进程，对每个子进程的主线程读取 `pdeathsig`，若不为 0 则构造 `SignalInfo::new_kernel(signo)` 并调用 `send_signal_to_process` 投递。

## 测试

测试用例位于 `test-suit/starryos/normal/test-prctl-pdeathsig/`，RISC-V 64 QEMU 运行。

测试覆盖：
- `PR_SET_PDEATHSIG(SIGTERM)` 返回 0
- `PR_GET_PDEATHSIG` 读回 SIGTERM
- `PR_SET_PDEATHSIG(0)` 清除信号，`PR_GET_PDEATHSIG` 读回 0
- 信号编号 65 被拒绝（返回 -1）
- 实际信号投递：祖父进程 -> 父进程 -> 子进程三层结构，子进程设置 `PR_SET_PDEATHSIG(SIGUSR1)`，父进程退出后子进程通过管道通知祖父进程已收到信号
