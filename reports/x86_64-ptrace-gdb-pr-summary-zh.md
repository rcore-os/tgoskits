# x86_64 Native GDB MVP 阶段总结（PR 用）

## 这次完善了什么

本次改动围绕 `x86_64 native CLI gdb MVP` 做了最小可用链路补齐，重点完善了以下能力：

1. `x86_64` ptrace 寄存器访问能力  
   - `PTRACE_GETREGS / SETREGS`
   - `PTRACE_GETREGSET / SETREGSET`
   - `PTRACE_GETSIGINFO`

2. traced stop 主链验证  
   - `PTRACE_TRACEME`
   - `waitpid(..., WUNTRACED)`
   - `execve -> SIGTRAP` 初始 stop
   - `PTRACE_CONT`

3. software breakpoint 能力  
   - 写入 `int3`
   - breakpoint 命中后重新 stop
   - 恢复原字节并继续执行

4. `x86_64 PTRACE_SINGLESTEP`  
   - 支持单步执行一条用户态指令
   - 单步后重新以 `SIGTRAP` 返回给 tracer

5. 更接近真实调试器的断点恢复流程  
   - 回退 `RIP`
   - 恢复原指令
   - single-step 执行原指令
   - 重新插回 breakpoint
   - 再继续执行

6. native gdb batch-mode 测试入口  
   - 用于验证 StarryOS 上 native gdb 对 ptrace 能力的实际消费路径

## 当前已经做到什么程度

在以下边界内：

- `x86_64`
- native CLI gdb
- 单线程 toy 程序
- 调试 `run` 启动的 child

当前已经具备：

- 初始 `execve` stop
- software breakpoint
- general-purpose registers 读写
- continue
- single-step
- 更真实的 breakpoint restore/reinsert 流程

这意味着 StarryOS 在 `x86_64` 上已经具备 **native gdb MVP** 所需的关键 ptrace 语义。

## 当前还没有覆盖什么

本次改动**不是**完整 Linux ptrace 实现，当前仍未覆盖或未完整验证的能力包括：

- `PTRACE_ATTACH`
- `PTRACE_SEIZE`
- `PTRACE_INTERRUPT`
- 多线程 ptrace group-stop 语义
- `PTRACE_SYSCALL`
- `PTRACE_O_TRACEFORK/CLONE/EXEC`
- `/proc/<pid>/mem`
- hardware watchpoint
- 浮点寄存器 ptrace
- interactive gdb 的 tty/readline 完整体验

## 测试覆盖

本次主要通过以下 `x86_64` 测试验证：

- `test-ptrace-x86-regs`
- `test-ptrace-exec-stop`
- `test-ptrace-x86-breakpoint`
- `test-ptrace-x86-singlestep`
- `test-ptrace-x86-breakpoint-reinsert`
- `test-gdb-native-batch`

## 结论

本次 PR 的成果定位应为：

> 完成 `x86_64 native CLI gdb MVP` 的关键内核语义补齐与测试覆盖。

而不是：

> 完成完整 Linux ptrace / 完整 gdb 兼容实现。

