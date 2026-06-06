# x86_64 ptrace / native gdb MVP 本地说明

> 这份文档是写给开发者自己看的，目标不是对外宣传，而是把这次工作到底做到了什么、为什么这样做、哪些地方还没做完，尽量讲清楚。

## 1. 这次到底在做什么

这次工作的真正目标，不是“一次性把 Linux ptrace 全补完”，也不是“把 gdb 所有功能都支持掉”。

这次更准确的目标是：

- 在 `x86_64` 上
- 让 StarryOS 具备 **native CLI gdb 的最小可用能力**
- 先支持最核心的那一条调试闭环

这条闭环具体是：

1. `gdb` 启动一个 child
2. child `PTRACE_TRACEME`
3. child `execve` 目标程序
4. tracer 观察到初始 `SIGTRAP` stop
5. tracer 能读写寄存器
6. tracer 能插 software breakpoint
7. child 命中 breakpoint 后再次 stop
8. tracer 能恢复原字节并继续执行
9. tracer 能 single-step 一条指令
10. 最后 child 正常退出

如果上面这条链能成立，就说明：

> StarryOS 在 `x86_64` 上已经具备 native gdb MVP 所需的“最小调试内核语义”。

---

## 2. 一开始我们以为缺什么，后来发现了什么

### 2.1 一开始的判断

最开始的直觉是：

- `wait4` 只会等 zombie
- `SIGTRAP` 只会走普通 signal 路径
- StarryOS 可能还没有 traced-stop 这套语义
- 所以 native gdb 大概率还差一整块 ptrace / wait / stop 逻辑

这个判断不能说全错，但后面读代码和测下来后，发现仓库现状比最开始设想的要好很多。

### 2.2 后来真正确认到的仓库现状

后面确认到，仓库里其实已经有不少 ptrace 基础设施：

- `PTRACE_TRACEME`
- traced stop 状态字段
- `ptrace_stop_current(...)`
- `execve` 后初始 stop pending
- `wait4 / waitid` 对 traced stop 的可见性
- breakpoint 对 traced task 的分流

所以后面问题就从：

> “有没有 ptrace 主干”

变成了：

> “x86_64 上到底还差哪些关键消费面和真实调试器语义”

这点非常重要，因为它决定了我们不是从零写一整套，而是：

- 补 `x86_64` 寄存器 ABI
- 补 `x86_64` single-step
- 用测试把已有主干坐实

---

## 3. 这次已经补齐/验证了哪些能力

下面这些是这次最核心的成果。

### 3.1 基础 stop / regs / continue

已通过：

- `PTRACE_TRACEME`
- `waitpid(..., WUNTRACED)`
- `PTRACE_GETSIGINFO`
- `PTRACE_GETREGS`
- `PTRACE_SETREGS`
- `PTRACE_GETREGSET`
- `PTRACE_SETREGSET`
- `PTRACE_CONT`

这部分证明的是：

> traced child 停住后，tracer 至少已经能“看、改、放走”。

对应测试：

- `test-ptrace-x86-regs`

### 3.2 `execve` 初始 stop

已通过：

- child `TRACEME`
- child `execve`
- parent 观察到初始 `SIGTRAP` stop
- `PTRACE_DETACH`
- `PTRACE_CONT(..., 0)` 抑制信号

这部分证明的是：

> native gdb `run` 模式里最重要的第一个 stop 节点已经存在。

对应测试：

- `test-ptrace-exec-stop`

### 3.3 software breakpoint 最小闭环

已通过：

- tracer 向用户代码写入 `int3` (`0xCC`)
- child 命中 breakpoint 后再次 `SIGTRAP`
- tracer 读取 stop 现场
- tracer 恢复原字节
- child 可以继续正常退出

这部分证明的是：

> StarryOS 不只是会 stop，还已经能支持最基本的 software breakpoint。

对应测试：

- `test-ptrace-x86-breakpoint`

### 3.4 `x86_64 PTRACE_SINGLESTEP`

已补齐并验证：

- `PTRACE_SINGLESTEP` 真正执行一条用户指令
- 单步后重新 stop
- tracer 能在 single-step stop 点再次观察寄存器状态

这部分很关键，因为没有它，很多更真实的 breakpoint 恢复语义都只能用“测试特化路径”绕过去。

对应测试：

- `test-ptrace-x86-singlestep`

### 3.5 更真实的断点恢复流程

已补齐并验证更像真实 gdb 的流程：

1. 命中 breakpoint
2. 把 `RIP` 回退到 breakpoint 地址
3. 恢复原字节
4. `PTRACE_SINGLESTEP` 执行原指令
5. 重新插回 breakpoint
6. 再 `PTRACE_CONT`

这比“跳过断点直接继续”的测试化做法更有说服力。

对应测试：

- `test-ptrace-x86-breakpoint-reinsert`

### 3.6 native gdb batch 入口

已经补了 native gdb batch 的测试入口，用来验证：

- gdb 本体能不能在 Starry guest 内跑起来
- gdb 能不能真的消费 ptrace 语义

这个 case 当前的意义更偏向“系统集成验证”，不是单个 ptrace 语义点验证。

对应测试：

- `test-gdb-native-batch`

---

## 4. 为什么中途会有一些“看起来像挂了”的问题

### 4.1 `user.rs` 跨架构编译失败

中途有一次 CI 失败，看起来像是：

- Starry QEMU matrix 挂了

真正根因其实是：

- 我们把 `x86_64` 专属的 `#DB` / Trap Flag 逻辑放到了通用路径
- 非 `x86_64` 架构没有：
  - `ExceptionKind::Debug`
  - `UserContext.rflags`

所以它不是 “x86_64 ptrace 功能逻辑错了”，而是：

> x86_64-only 代码没有用 `#[cfg(target_arch = "x86_64")]` 收口好。

这个后来已经修掉了。

### 4.2 `test-ptrace-x86-breakpoint` 一度出现 `RSP=0x1`

这个问题一开始很像：

- breakpoint stop 的寄存器现场坏了
- 尤其是 `RSP=0x1`

后来这条测试跑通，说明那时候的问题至少已经不再是当前阻塞项。

它的重要价值在于帮助确认：

> breakpoint stop 的关键风险点在 x86 trap/context 保存链，而不是 ptrace 接口表面本身。

### 4.3 `test-gdb-native-batch` 出现段错误 / timeout

这个问题和 ptrace 语义本身不一定是同一个层面。

我们目前看到的更强证据是：

- `apk add gdb` 阶段就可能出现：
  - `Out of memory`
  - `Segmentation fault`

所以这个 case 当前首先要区分的是：

1. guest 里在线安装 `gdb` 的资源开销是不是过大
2. 是不是先 OOM 了，导致后面的 gdb batch 根本没开始执行
3. 只有在安装阶段稳定通过后，才有资格继续怀疑 ptrace / gdb 语义

这也是为什么现在这个 case 被切成“先只测安装阶段”的诊断模式。

---

## 5. 这次做完后，当前代码到底能覆盖哪些 gdb 功能

这个问题必须说得很谨慎。

### 可以比较有把握说“已覆盖”的

在 `x86_64`、单线程、child 由 `run` 启动的边界下，当前已经能覆盖：

- 初始 `execve` stop
- software breakpoint
- general-purpose registers 读写
- continue
- single-step
- 更真实的 breakpoint restore/reinsert 流程
- non-interactive gdb batch 所需的关键 ptrace 语义基础

### 不能说“已经完整支持”的

下面这些还不能算完成：

- `PTRACE_ATTACH`
- `PTRACE_SEIZE`
- `PTRACE_INTERRUPT`
- 多线程 ptrace group-stop
- `PTRACE_SYSCALL`
- `PTRACE_O_TRACEFORK/CLONE/EXEC`
- `/proc/<pid>/mem`
- hardware watchpoint
- 浮点寄存器 ptrace
- interactive gdb 的 tty/readline 完整体验

所以这次最准确的定位应该是：

> 已完成 `x86_64 native CLI gdb MVP`，但远未完成“完整 Linux ptrace / 完整 gdb 支持”。

---

## 6. 现在为什么可以先交一版

因为现在已经不是“靠代码阅读猜”，而是有一整组测试把最关键链路坐实了。

至少目前已经有这些证据：

- `test-ptrace-x86-regs`
- `test-ptrace-exec-stop`
- `test-ptrace-x86-breakpoint`
- `test-ptrace-x86-singlestep`
- `test-ptrace-x86-breakpoint-reinsert`

这说明：

> 对 native gdb MVP 最关键的那条控制链，已经不只是“概念上可行”，而是“测试上可走通”。

从工程节奏看，这是一个很适合先提交阶段性成果的点。

---

## 7. 下一步最值得做什么

如果这版先交，后续最合理的方向不是“继续随便补更多 opcode”，而是二选一：

### 方向 A：提升可用性

- 让 interactive `gdb -q` 更稳定
- 解决 tty / readline / termios 交互问题
- 把 native gdb 从 batch MVP 推到 interactive MVP

### 方向 B：提升 ptrace 覆盖面

- `PTRACE_SYSCALL`
- `GETFPREGS / SETFPREGS`
- 更多 ptrace event / option

如果只看“用户感知价值”，我更倾向先做方向 A。

---

## 8. 一句话总结

这次工作的本质成果是：

> 在 `x86_64` 上，把 StarryOS 的 ptrace / stop / breakpoint / single-step 语义推进到了足以支撑 native CLI gdb 最小可用流程的程度，并用一组针对性测试把这条 MVP 链坐实了。

但这仍然只是：

> **native gdb MVP**

不是：

> **完整 Linux ptrace / 完整 gdb 兼容实现**

