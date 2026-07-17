# StarryOS Linux syscall 兼容性准则

## 1. 适用范围与阅读要求

本准则用于实现、修改和评审 StarryOS 对用户态可见的 Linux syscall / ABI 语义。

只要改动或需求会影响用户态观察到的 syscall 行为，就必须完整阅读本文件。判断依据是语义而不是路径：即使改动位于 task、VFS、namespace、signal、socket、credential、内存管理或其他 helper 中，只要它改变 syscall 的参数解释、返回值、errno、权限、共享关系或状态转换，本准则仍然适用。

以下情况通常不适用：改动只涉及内部重命名、格式化、无行为变化的重构，且能够证明用户态 ABI 和错误路径不变。评审 checklist 必须记录适用或不适用的具体理由。

## 2. 兼容目标

StarryOS 的 syscall 兼容目标不是“常见程序能够运行”，而是让相同前置状态和输入在 StarryOS 与目标 Linux 版本上产生一致的用户态可观察结果：

- syscall 编号、参数宽度、signedness、结构体布局和返回值 ABI 一致；
- 成功结果、失败 errno 以及多个错误同时成立时的错误优先级一致；
- credential、capability、namespace、线程和进程资源共享语义一致；
- 阻塞、唤醒、restart、状态消费和并发可见性一致；
- 支持的 architecture ABI 一致，或明确返回 Linux 对应的 unsupported 错误；
- 测试能够在错误实现上稳定失败，而不只是覆盖成功路径。

如果 Linux 版本之间存在行为差异，必须记录用于比对的 Linux 版本或 commit。不得把某个发行版、libc wrapper 或偶然实测结果无条件推广为所有 Linux 版本的契约。

## 3. 资料来源与证据优先级

### 3.1 Linux man-pages：公开接口入口

优先查阅 [Linux man-pages](https://man7.org/linux/man-pages/) 对应的 `man2`、`man3`、`man7` 页面，确认参数含义、flags、返回值、errno、权限要求、版本历史和相关接口。

man-pages 是公开接口的重要说明，但经常不会规定所有错误优先级、并发细节或实现中的状态转换。页面未明确说明时，不能凭直觉补全语义，必须继续查 Linux 源码和测试。

### 3.2 Linux kernel 文档与源码：实现语义依据

使用 [Linux kernel documentation](https://www.kernel.org/doc/html/latest/) 和 kernel.org 上的 Linux 源码确认：

- `include/uapi/` 中的常量、结构体和用户 ABI；
- `include/linux/syscalls.h`、`SYSCALL_DEFINE*` 入口和实际 subsystem 实现；
- `arch/*/entry/syscalls/`、`arch/*/tools/syscall*.tbl`、`asm-generic/unistd.h` 等架构 syscall 表；
- compat syscall、32/64 位结构体转换和 sign extension；
- capability、LSM、credential、namespace 和资源共享检查；
- 路径解析、参数校验和权限检查的实际执行顺序；
- 状态在何时查询、消费、回滚、发布和唤醒。

错误优先级或状态转换不明确时，应沿 Linux 调用链读到能够解释用户态结果的位置，并记录目标版本或 commit。只搜索同名函数而不阅读调用者、callee 和错误转换不足以证明兼容性。

### 3.3 POSIX：标准化接口契约

对 POSIX 定义的接口，查阅 [The Open Group POSIX.1-2024](https://pubs.opengroup.org/onlinepubs/9799919799/) 的 System Interfaces 与 Rationale。

POSIX 用于确认可移植契约；Linux 扩展、Linux 特有 errno 或 Linux 明确偏离 POSIX 的行为仍以目标 Linux ABI 为兼容依据。发现差异时必须在实现、测试或评审结论中明确区分“POSIX 要求”和“Linux 行为”。

### 3.4 LTP 与 kernel selftests：回归场景参考

查阅 [Linux Test Project](https://linux-test-project.readthedocs.io/) 的 syscall 测试和 Linux kernel `tools/testing/selftests/`，寻找已有的边界、权限、并发和回归场景。

测试用例是重要证据，但不是独立规范。测试缺失不表示行为未定义；测试与文档或源码冲突时，必须查清目标版本、feature 配置和测试前置条件。

### 3.5 libc 与观测工具：辅助证据

glibc、musl 源码用于判断 libc wrapper 是否改写参数、重试、转换返回值或在用户态实现部分功能；`strace` 用于确认程序实际发出的 syscall 和观察结果。

libc wrapper、`strace` 输出、博客、问答和单次实验都不能单独作为内核语义结论。需要比对内核 ABI 时，优先编写直接调用 `syscall(SYS_...)` 的最小测试，避免 wrapper 掩盖原始返回值或 errno。

## 4. Syscall 正确性比对流程

### 4.1 建立接口清单

在读实现前写出本次改动涉及的 syscall、参数、flags、结构体、credential/capability、资源对象和用户态可观察状态。改动 helper 时，反向列出所有可能受影响的 syscall 入口。

不得只按 PR 标题检查一个 syscall；共享 helper、错误转换和状态对象可能同时影响多个入口。

### 4.2 核对 ABI 边界

逐项核对：

- syscall number 与 architecture 映射；
- 每个参数的位宽、signedness、截断和 sign/zero extension；
- `NULL` 是否允许，以及空指针和无效指针分别何时产生错误；
- 用户结构体的 `repr(C)`、字段类型、padding、alignment、32/64 位和 compat 布局；
- flags 的合法组合、未知位处理和版本差异；
- 大小、offset、fd、pid、time、pointer 等边界值，包括 `0`、`-1`、最小/最大值和溢出。

syscall dispatch 层不得先把有符号 ABI 参数转换成无符号类型再做负值校验，也不得用宿主 `usize` 隐式替代固定宽度 Linux ABI。

### 4.3 核对成功语义和错误优先级

为每条路径构造至少一个成功场景和主要失败场景。多个错误条件同时成立时，必须核对 Linux 实际检查顺序，例如：

- path 不存在与权限不足；
- fd 无效与 flags 非法；
- capability 缺失与用户指针无效；
- 目标已存在与父目录不可写；
- 对象类型错误与操作不支持。

不能仅确认“会失败”；必须确认返回的是相同 errno。不要为通过单个测试而在 syscall 顶层硬编码 errno，应让错误来自与 Linux 等价的校验和状态边界。

### 4.4 核对权限与隔离边界

权限相关 syscall 必须检查实际使用的 credential（real/effective/saved/fs uid/gid）、supplementary groups、capability 所在 user namespace，以及检查发生的时机。

namespace、fd table、fs context、signal handler、VM 等共享资源必须核对操作影响的是调用线程、线程组、进程还是 namespace 中的所有对象。尤其检查：

- `clone`、`fork`、`exec`、`unshare`、`setns` 的复制与共享边界；
- 多线程进程中只应影响调用线程的操作是否误改共享 process state；
- capability 检查是否覆盖同一能力的所有 syscall 入口；
- lookup、permission check 与 mutation 之间是否存在 TOCTOU 或锁边界错误。

### 4.5 核对状态转换与并发

对 wait、signal、epoll、futex、socket、timer、process lifecycle 等有状态 syscall，画出或明确列出状态转换：状态由谁产生、谁观察、何时消费、`WNOWAIT`/peek 类操作是否保留状态、失败时是否回滚，以及谁负责 wakeup。

必须检查正常路径、错误路径、并发 interleaving、阻塞/非阻塞模式、信号中断和 restart 行为。锁内不得执行可能 fault、sleep、阻塞或回调未知代码的操作，除非所用锁和调用契约明确允许。

### 4.6 形成可追溯结论

实现或评审结论应记录：

- 比对的 Linux man-page、源码文件/函数和目标版本或 commit；
- POSIX 条款（如果适用）以及 Linux 扩展或偏差；
- LTP/selftest 或直接 syscall 对照场景；
- StarryOS 中从 dispatch 到状态边界的对应调用链；
- 每个关键 errno、权限检查和共享关系为何与 Linux 一致。

“参考 Linux”“与 man page 一致”或“测试通过”都不足以替代上述证据。

## 5. 测试要求

新增 syscall、语义变更和 bug 修复必须在 `test-suit/starryos` 的正确 case 中增加确定性回归测试，除非有具体、可审查的理由证明无法测试。

测试必须：

- 尽量通过 `syscall(SYS_...)` 直接触达 ABI，特别是 errno、nullable pointer 和 libc 可能转换结果的接口；
- 同时断言返回值和 errno，不把任意失败视为通过；
- 覆盖本次修复的边界组合及错误优先级，而不只覆盖 happy path；
- 对权限测试明确 root/non-root、uid/gid、capability 和 namespace 前置条件；
- 对共享资源测试使用真实多线程/多进程关系，并证明未操作的参与者保持原状态；
- 对状态消费测试分别覆盖 peek/consume、重复调用和并发观察；
- 在 buggy 实现上必然失败，在修复实现上通过；
- 被 Starry test runner 发现、构建、安装并由明确的 `cargo xtask starry test qemu ... -c <case>` 命令执行。

如果 host Linux 支持对应接口，应使用相同测试输入做 Linux/Starry 差分验证。涉及 architecture ABI 时，至少覆盖改动声明支持的架构，并优先验证参数布局或 syscall table 不同的架构。

## 6. Review Checklist

涉及 Starry syscall 语义的评审必须逐项回答：

1. 是否找全直接和间接受影响的 syscall 入口？
2. ABI 类型、signedness、pointer、结构体和 flags 是否与目标 architecture Linux 一致？
3. 成功结果、errno 和错误优先级是否有源码或实测依据？
4. credential、capability、namespace 与线程/进程共享边界是否正确？
5. 状态产生、观察、消费、回滚和 wakeup 是否正确？
6. 是否检查 compat、并发、阻塞、信号中断和极值输入？
7. 回归测试是否直接覆盖原始 ABI，并在错误实现上失败？
8. 测试是否由项目 runner 实际发现和执行？
9. 结论是否记录 Linux 版本、权威资料和 Starry 对应调用链？

任一关键问题无法回答时，不得仅凭 CI 通过认定 syscall 兼容性正确。
