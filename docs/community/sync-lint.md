---
sidebar_position: 4
sidebar_label: "同步内存序检查"
---

# 同步内存序检查

`sync-lint` 是仓库里的一个静态检查工具，入口命令是：

```bash
cargo xtask sync-lint
```

它的目标是：检查Atomic原子变量的使用，抓那些**承担同步语义**、但仍然写成了 `Relaxed` 的用法。

典型情况包括：

- 这个原子变量是不是在决定“别的线程/任务/CPU 能不能继续执行”
- 它是不是在负责“发布状态，再唤醒别人”

如果是，那么用 `Relaxed` 就有可能过弱，尤其是在 AArch64、RISC-V 这类弱内存序架构上。

## 当前检查范围

### 1. 在等待条件里使用 `Relaxed`

当前实现主要检查两种高置信等待场景：

- `wait_until` / `wait_timeout_until` / `wait_while` 这类等待接口的条件闭包
- 带有明显阻塞或让出动作的 `while` 循环条件，例如循环体里出现 `thread::yield_now()`、`thread::sleep(...)`、`spin_loop()` 或 `park()`

这类代码会被报告：

```rust
WQ.wait_until(|| COUNTER.load(Ordering::Relaxed) == NUM_TASKS);

while !READY.load(Ordering::Relaxed) {
    thread::yield_now();
}
```

原因是这里的原子变量不是“看一眼统计值”，而是在决定：

- 当前线程是否继续等待
- 当前阶段是否已经完成
- 另一个执行流是否已经把状态准备好

### 2. 用 `Relaxed` 写状态后立刻 `notify` / `wake`

这类代码也会被报告：

```rust
COUNTER.fetch_add(1, Ordering::Relaxed);
WQ.notify_one(true);

GO.store(true, Ordering::Relaxed);
WQ.notify_all(true);
```

这种模式通常表示：

1. 先发布状态
2. 再唤醒等待者去观察这个状态

如果发布动作还是 `Relaxed`，等待者就可能虽然被唤醒了，但看不到你刚刚写入的最新状态。

### 3. 同一个同步原子混用了强序和 `Relaxed`

如果某个原子已经被检查器认定为“同步变量”，比如：

- 它出现在等待条件里
- 它承担了“写状态后唤醒别人”的职责

那么检查器还会继续看这个原子在同一文件里的其他访问。

如果它一边用了：

- `Acquire`
- `Release`
- `AcqRel`
- `SeqCst`

另一边又用了：

- `Relaxed`

那么这些 `Relaxed` 访问也会被报告。

例如：

```rust
READY.store(true, Ordering::Release);
WQ.notify_all(true);

if READY.load(Ordering::Relaxed) {
    do_work();
}
```

这类情况通常说明：这个原子已经明显承担同步语义，但仍然有一部分访问保留在 `Relaxed`，需要重新确认是否真的足够。

## 当前不检查什么

为了避免误报，当前实现依然刻意没有做“大而全”的规则。

目前不会主动检查这些情况：

- 纯统计/计数用途的原子变量；但如果一个“计数器”本身参与阶段同步、等待条件或唤醒流程，它仍然可能被报告
- `Acquire` / `Release` 是否成对匹配
- `AcqRel` 或 `SeqCst` 是否过强
- 更复杂的跨函数发布模式
- 基于任意控制流和任意数据结构的通用 dataflow 推理
- lock-free 算法内部的状态机细节

也就是说，当前规则是一个**保守版、低误报**检查器。

## 典型提示长什么样

提示格式是：

```text
<path>:<line>:<column>: <message> [<rule>]
```

例如：

```text
test-suit/arceos/rust/task/wait_queue/src/main.rs:44:13:
Relaxed atomic write is immediately followed by a wake/notify operation
[suspicious_relaxed_publish_before_notify]
```

或者：

```text
test-suit/arceos/rust/task/parallel/src/main.rs:40:12:
Relaxed atomic load is used in a wait condition
[suspicious_relaxed_wait_condition]
```

也可能看到：

```text
some/path.rs:27:8:
Relaxed atomic access is mixed with stronger orderings on the same synchronization variable
[suspicious_relaxed_mixed_ordering]
```

## 这些提示分别是什么意思

### `suspicious_relaxed_wait_condition`

意思是：

- 某个 `Atomic*` 的 `load(Ordering::Relaxed)` 被拿来做等待/阻塞/自旋条件
- 这个值不再只是“统计信息”
- 而是在决定当前执行流能不能往前推进

一般应考虑把这类读改成：

- `Ordering::Acquire`

对应的写一侧如果也承担发布语义，通常要改成：

- `Ordering::Release`

### `suspicious_relaxed_publish_before_notify`

意思是：

- 你刚刚对原子变量做了 `Relaxed` 写入
- 然后马上 `notify_one` / `notify_all` / `wake`

这通常是在“告诉别人状态已经准备好”。  
这类写一般应考虑改成：

- `Ordering::Release`

### `suspicious_relaxed_mixed_ordering`

意思是：

- 同一个原子在同一文件里既出现了强序访问，也出现了 `Relaxed` 访问
- 并且这个原子已经有足够证据表明自己是同步变量，而不是纯统计值

这类提示通常不是单独成立的“文本匹配”，而是结合了前面的等待/唤醒语义一起判断出来的。

一般应先回头看这个原子的职责，再决定是否把相关 `Relaxed` 访问也统一成：

- 读侧 `Ordering::Acquire`
- 写侧 `Ordering::Release`

## 一般怎么修

一个简单的经验法则是：

- **读侧**如果在“等待某个状态成立”，优先考虑 `Acquire`
- **写侧**如果在“发布这个状态，然后唤醒别人”，优先考虑 `Release`

最常见的修法是把：

```rust
flag.store(true, Ordering::Relaxed);
```

改成：

```rust
flag.store(true, Ordering::Release);
```

把：

```rust
while !flag.load(Ordering::Relaxed) {
    thread::yield_now();
}
```

改成：

```rust
while !flag.load(Ordering::Acquire) {
    thread::yield_now();
}
```

## 一个完整例子

下面这个模式很典型：

```rust
COUNTER.fetch_add(1, Ordering::Relaxed);
WQ1.notify_one(true);

WQ1.wait_until(|| COUNTER.load(Ordering::Relaxed) == NUM_TASKS);
```

它的问题不是“`COUNTER` 是计数器，所以一定错”，而是：

- 这个计数器不只是统计用途
- 它实际上承担了“阶段同步”的职责
- 主线程靠它判断是否所有 worker 都到齐了
- worker 在更新它之后立刻唤醒等待者

因此它更像“同步变量”，而不是普通计数器。

更合适的写法是：

```rust
COUNTER.fetch_add(1, Ordering::Release);
WQ1.notify_one(true);

WQ1.wait_until(|| COUNTER.load(Ordering::Acquire) == NUM_TASKS);
```

这里的含义是：

- `Release`：发布“我已经到达这个阶段”
- `Acquire`：观察者在判断阶段完成时，要看到这个发布过的状态

## 如果你认为这是误报

第一阶段规则已经比较保守，但仍然保留了显式忽略入口。

可以在代码上方写：

```rust
// sync-lint: ignore suspicious_relaxed_wait_condition
```

或者：

```rust
// sync-lint: ignore suspicious_relaxed_publish_before_notify
```

或者：

```rust
// sync-lint: ignore suspicious_relaxed_mixed_ordering
```

也可以写成通用忽略：

```rust
// sync-lint: ignore
```

当前实现的匹配规则要点是：

- 注释里必须至少包含 `sync-lint: ignore`
- 如果后面再带具体规则名，就只忽略那一条规则
- 如果不带具体规则名，就会把当前 `sync-lint` 的规则都忽略掉
- 只写 `// sync-lint:` 这种前缀并不会生效
- 忽略注释必须位于被报告代码上方的 1 到 3 行内

例如：

```rust
// sync-lint: ignore suspicious_relaxed_wait_condition
wq.wait_until(|| counter.load(Ordering::Relaxed) == 1);
```

表示只忽略 `suspicious_relaxed_wait_condition`。

```rust
// sync-lint: ignore
wq.wait_until(|| counter.load(Ordering::Relaxed) == 1);
```

表示通用忽略。

```rust
// sync-lint:
wq.wait_until(|| counter.load(Ordering::Relaxed) == 1);
```

这不会被识别成忽略注释。

如果你确实要忽略，建议把理由写清楚，例如：

```rust
// sync-lint: ignore suspicious_relaxed_wait_condition
// stats-only counter, not used for synchronization
```

请注意，只有在你能明确说明“这个原子变量不承担同步语义”时，才建议这样做。

## 提交前建议

如果你改动了并发、任务、等待队列、唤醒、原子状态机等相关逻辑，建议在本地先跑一遍：

```bash
cargo xtask sync-lint
```

这样能比 CI 更早发现问题，也更方便你在本地直接跳到报错位置修改。

## 结论

可以把 `sync-lint` 理解为一个简单的问题筛子：

- 它不试图判断所有原子变量
- 它只抓那些“看起来像同步变量，但用了过弱内存序”的高置信模式

如果你的代码被它挡住，先不要把它理解成“工具不允许用 `Relaxed`”，而应该先问自己：

- 这个原子变量是不是在控制别的线程/任务/CPU 的推进时机？
- 它是不是在发布一个会被别人观察到的阶段状态？

如果答案是“是”，那么优先考虑把它改成 `Acquire/Release` 风格的同步。
