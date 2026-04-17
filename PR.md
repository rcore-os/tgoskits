# PR 描述

**标题**: feat(signal): implement SA_RESTART syscall restart semantics

## Bug 描述

StarryOS 未实现 `SA_RESTART` 语义。当阻塞的系统调用（如 `read`、`accept`）被信号中断时，若信号处理函数设置了 `SA_RESTART` 标志，Linux 会自动将该系统调用重新执行；StarryOS 则无论 `SA_RESTART` 是否设置，一律对调用者返回 `EINTR`。PostgreSQL 的 postmaster 和 worker 进程在等待连接或 IPC 时依赖 `SA_RESTART`，否则每次信号交付都会导致意外的 EINTR，触发重试逻辑甚至错误退出。

此外，`check_signals` 的 `a0`（RISC-V 第一参数寄存器，同时用作返回值寄存器）恢复存在缺陷：重启时需要把 `a0` 回填为原始第一参数而非 EINTR 错误码，原代码没有保存原始 `a0`。

## 根本原因分析

RISC-V、AArch64、LoongArch64 三个架构上，系统调用第一参数寄存器和返回值寄存器是同一个物理寄存器（RISC-V: `a0`，AArch64: `x0`，LoongArch64: `a0`）。系统调用执行时内核覆盖了该寄存器（写入 EINTR），若要重启调用，必须在进入 `handle_syscall` 之前保存原始参数值，重启时将 PC 退回 syscall 指令（RISC-V/AArch64/LoongArch64: `SYSCALL_INSN_LEN = 4` 字节，x86_64: 2 字节）并同时恢复参数寄存器。恢复后不能再调用 `set_retval(0)` 清除 EINTR，否则会覆盖刚恢复的参数值。x86_64 上 `rdi`（arg0）和 `rax`（retval）是不同寄存器，不受此问题影响，但修复逻辑对所有架构均正确。

原实现 `check_signals` 不接受 `saved_a0`，也没有在入口处读取它，导致重启时参数丢失。

参考 `sigaction(2)` 中 SA_RESTART 一节及 Linux 内核 `signal.c` 中的 `ERESTARTSYS` 路径。

## 修复方案

**`os/StarryOS/kernel/src/task/mod.rs`**

新增 `SYSCALL_INSN_LEN` 常量：x86_64 为 2，其余架构（RISC-V、AArch64）为 4。

**`os/StarryOS/kernel/src/task/signal.rs`**

新增 `SyscallRestartInfo { saved_a0: usize }`，作为可选参数传入 `check_signals`。

`check_signals` 重构：先 `dequeue_signal`，再取出信号对应的 `SignalAction`，检查返回值是否为 EINTR：
- 若设置了 `SA_RESTART`，将 PC 减去 `SYSCALL_INSN_LEN`，并用 `set_arg0(info.saved_a0)` 恢复原始参数。此时不调用 `set_retval(0)`，因为在 RISC-V/AArch64/LoongArch64 上 `set_retval` 和 `set_arg0` 写同一个寄存器，调用会覆盖刚恢复的参数。
- 否则（信号处理函数未设置 SA_RESTART），将返回值清零以防止后续循环迭代重复处理 EINTR。

随后调用 `handle_signal` 执行实际信号分发，再处理 `SignalOSAction`。

**`os/StarryOS/kernel/src/task/user.rs`**

在 `handle_syscall` 调用之前保存 `saved_a0 = uctx.arg0()` 和 `is_syscall` 标志。信号处理循环中传入 `restart_info`。若循环结束后返回值仍为 EINTR（所有 pending 信号均无 handler，或均未设置 SA_RESTART），则在此处做最终 fallback：退回 PC 并恢复 `a0`。

**`os/StarryOS/kernel/src/syscall/signal.rs`**

`sys_rt_sigtimedwait` 和 `sys_rt_sigsuspend` 内部调用 `check_signals` 时传入 `None` 作为 `restart_info`（这两个调用点本就期望 EINTR 语义，不需要重启）。

## 测试

测试用例位于 `test-suit/starryos/normal/test-sa-restart/`，使用 RISC-V 64 QEMU 运行。

测试覆盖（两个子进程测试，隔离运行）：
- **Test 1**：子进程在管道 `read` 上阻塞，设置 `SA_RESTART`，父进程发送 SIGUSR1 后写入数据。子进程 `read` 应自动重启并读到数据，退出码 0。验证 SA_RESTART 路径正确恢复 a0。
- **Test 2**：子进程在管道 `read` 上阻塞，不设置 `SA_RESTART`，父进程发送 SIGUSR1。子进程 `read` 应返回 -1/EINTR，退出码 0。验证非重启路径不丢失 EINTR。
