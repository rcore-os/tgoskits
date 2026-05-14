---
sidebar_position: 5
sidebar_label: "Panic 递归保护"
---

# Panic 递归保护

本文档说明在 TGOSKits / ArceOS / StarryOS 中引入 panic/oops 递归保护的目的、
基本原理、当前解决的问题，以及后续可继续演进的方向。

它关注的是 **异常路径健壮性**，而不是某个具体 bug 本身。

## 目的与意义

在当前 Starry lockdep 调试中，已经观察到这样一种现象：

- 先触发一个 lockdep 违例
- 如果违例后立即直接停机，系统可以稳定结束
- 如果违例后继续走普通 panic 路径，则可能出现 page fault、卡住、串口无响应等次生故障

这说明这里其实有两类不同问题：

1. 是否应该进入 panic
2. 进入 panic 之后，异常路径本身是否足够健壮

前者通常属于具体功能或锁顺序问题；后者则属于通用的内核异常路径设计问题。

把 panic/oops 递归保护单独设计出来的意义在于：

- 它不只服务于 lockdep
- 它同样适用于 panic、oops、BUG、die 等其他异常收尾路径
- 它能降低“主故障之后又在异常路径里继续放大故障”的概率

因此，这类机制应被视为一个独立主题，而不是夹带在某个具体修复里顺手处理。

## 基本原理

这类机制的设计思路，当前主要参照 Linux 的 panic/oops 路径实现。

原因不是为了机械地“和 Linux 保持一致”，而是因为 Linux 长期处理过大量：

- SMP 并发 panic
- 异常路径递归输出
- 控制台/锁/调试路径在故障态下再次放大问题

等实际问题，因此它在 panic/oops 路径上的分层思路具有直接参考价值。

Linux 没有试图用一个简单布尔值解决所有异常路径问题，而是把问题拆成两层。

### 1. Panic 主路径所有权

Linux 用 `panic_cpu` 这样的全局原子状态来约束：

- 只有一个 CPU 执行 panic 主路径
- 其他并发进入 panic 的 CPU 不再重复跑完整 panic 流程

典型逻辑是：

```c
old_cpu = PANIC_CPU_INVALID;
this_cpu = raw_smp_processor_id();

if (atomic_try_cmpxchg(&panic_cpu, &old_cpu, this_cpu)) {
        /* go ahead */
} else if (old_cpu != this_cpu)
        panic_smp_self_stop();
```

它解决的是 **SMP 并发 panic 主路径冲突**。

### 2. 异常路径全局状态

Linux 用 `oops_in_progress` 这样的全局状态来表达：

- 当前已经进入异常收尾路径
- 输出、控制台、锁、调试逻辑应转入更保守模式

它不是“阻止 panic”，而是给其他组件一个全局提示：

- 现在不要再走复杂路径
- 现在不要再轻易递归输出
- 现在不要再轻易进入带锁或高风险调试逻辑

它解决的是 **异常路径内部再次触发复杂输出/锁路径** 的问题。

### 3. 递归 panic 降级

在这两层之外，还需要对 panic handler 自身做分级：

- 首次 panic：尽量打印信息、尽量打印 backtrace、再停机
- 递归 panic：不再做复杂格式化、不再做完整 backtrace，直接最小化收尾

也就是说，异常路径不应总是假设“自己还能安全执行完整诊断逻辑”。

### 当前实现与 Linux 的关系

当前仓库的实现是 **参考 Linux 思路后的本地化版本**，而不是逐行对齐的复刻。

相同点在于：

- 都区分“panic 主路径所有权”和“异常路径全局状态”
- 都试图减少异常路径中的递归输出、重复执行和复杂控制流
- 都把“首次 panic”和“递归/并发进入 panic”区别对待

差异主要在于：

- 当前实现更小、更直接，主要围绕 `axpanic`、`axruntime` 和 `axlog` 三层展开
- 当前实现加入了 panic-path backtrace one-shot 门控，这是基于当前代码形态做的额外收敛
- 当前实现还没有覆盖 Linux 中更完整的 console / printk / bug / die 生态，也没有引入 Linux 那类更细的异常子状态和控制台收尾机制

换句话说，当前实现已经吸收了 Linux 方案最关键的分层原则，但在覆盖范围和成熟度上仍然更轻量，也还保留着继续扩展的空间。

## 解决了什么问题

基于上面的分层思路，对当前仓库来说，这套设计主要解决以下几类问题。

### 1. 避免多 CPU 重复进入 panic 主路径

如果多个 CPU 同时进入 panic，而每个 CPU 都继续执行完整的 panic handler，就可能发生：

- 重复打印
- 重复回溯
- 多条异常路径互相干扰
- 更复杂的锁、输出、停机竞态

因此需要一个类似 `panic_cpu` 的所有权机制，把 panic 主路径收敛到单 CPU。

### 2. 避免异常路径再次进入带锁输出

当前 panic 路径里，典型动作包括：

1. `ax_println!("{}", info)`
2. `ax_println!("{}", axbacktrace::Backtrace::capture())`
3. `ax_hal::power::system_off()`

其中前两类动作都会进入格式化和输出链，而 `axlog::print_fmt()` 正常情况下使用
`SpinNoIrq` 打印锁。对 panic 路径来说，这存在几个风险：

- 再次进入 lockdep 可见的锁路径
- 解锁时恢复 IRQ / preemption，触发更复杂控制流
- 在异常路径内部继续递归打印

因此需要一个类似 `oops_in_progress` 的全局标志，让输出路径在异常态下绕过普通锁路径。

### 3. 避免 panic 中重复执行 backtrace 这类复杂动作

`Backtrace::capture()` 不一定直接获取 console 锁，但它本身仍是复杂动作，可能涉及：

- 栈遍历
- frame pointer / return address 读取
- 启用 DWARF 时的后续符号化
- 动态内存分配

因此，backtrace 不应在 panic 路径里被无限重试。更保守的策略是：

- panic 路径中至多尝试一次 backtrace
- 递归或嵌套失败时不再重复进入

### 4. 把“主故障”和“次生故障”区分开

以 lockdep 为例：

- lockdep 违例本身是主故障
- 违例后 panic 路径再次卡死、page fault、串口无响应，是次生故障

panic/oops 递归保护的目的就是尽量减少第二类问题，让系统至少能：

- 输出足够的错误信息
- 停在可观察、可收尾的状态
- 不要让异常路径自身继续把问题扩大

## 当前实现如何落到这些原则上

当前分支上的实现已经把上述设计拆成了几块：

- `axpanic`：保存 panic 主路径状态、oops 状态、以及 panic-path backtrace 门控
- `axruntime` panic handler：负责 panic 入口分类、primary panic 编排、递归/并发 panic 的本地降级
- `axlog`：在 `oops_in_progress()` 为真时绕过普通打印锁

它们分别对应：

- `panic_cpu` 风格：panic 主路径所有权
- `oops_in_progress` 风格：异常路径全局状态
- panic backtrace one-shot：复杂诊断动作的最小化门控

这意味着当前实现并不是把所有异常路径都一次性做“全覆盖”，而是先把最容易放大故障的几条主链收敛起来：

- panic 主路径只保留一个 owner
- panic/oops 态下的打印路径先进入保守模式
- backtrace 这类复杂诊断动作增加最小门控

## 后续可能的改进

当前实现已经建立了第一层保护，但它仍然是一个偏“最小闭环”的版本，后面还可以继续沿着同一思路演进。

### 1. 更细的 backtrace 策略

目前 panic-path backtrace 已经有第一层门控：

- 最多只尝试一次

后续还可以继续细化，例如：

- 按平台启用/禁用
- 按构建配置启用/禁用
- 在某些高风险场景下跳过完整 backtrace

### 2. 更底层的 console/printk 降级

当前已经绕过了 `axlog` 这一层普通打印锁，但底层 console 路径是否还需要进一步降级，
仍取决于具体平台实现。

后续可以考虑：

- 为 panic/oops 路径提供更原始的 console fast path
- 明确区分普通输出和异常态输出

### 3. 纳入更多异常入口

目前主要围绕 panic handler 展开。后续可继续评估是否要把这些入口统一接入：

- BUG
- die
- lockdep fatal
- trap/exception 中的致命收尾路径

### 4. 更贴近真实场景的回归验证

这类机制最终不能只靠 unit test 或单一回归 app 证明，还需要持续验证更贴近真实系统的问题链：

- Starry lockdep 最小复现
- ArceOS C test 中的 `httpclient` 等 system-level 场景
- 更底层平台/console 路径的异常收尾表现

## 文档边界

本文档记录的是设计目标和机制分层，不固定以下实现细节：

- 具体使用 `AtomicBool`、`AtomicUsize` 还是其他原子类型
- 哪些路径应统一纳入 `oops_in_progress`
- backtrace/console 降级策略的最终粒度

这些细节应根据后续实现、平台行为和回归验证结果继续收敛。
