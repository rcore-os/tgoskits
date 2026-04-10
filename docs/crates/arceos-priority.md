# `arceos-priority` 技术文档

> 路径：`test-suit/arceos/task/priority`
> 类型：测试入口 crate
> 分层：测试层 / ArceOS 任务优先级回归
> 版本：`0.1.0`
> 文档依据：`Cargo.toml`、`src/main.rs`、`qemu-riscv64.toml`

`arceos-priority` 通过 5 个计算量不同、优先级不同的任务，验证 `ax_set_current_priority()` 这条任务优先级设置链是否仍然可用。它会同时检查计算结果是否正确，并记录每个任务的离开时间；在特定调度器与单核条件下，还会额外断言短任务的完成顺序是否符合优先级预期。

最关键的边界是：**这不是调度器性能 benchmark，也不是通用优先级框架；它只是一个围绕“设置当前任务优先级后，系统行为是否还自洽”的回归入口。**

## 1. 架构设计分析
### 1.1 工作负载设计
源码里定义了 5 组 `TaskParam`：

- 4 个短任务：`nice = 19 / 10 / 0 / -10`
- 1 个长任务：`nice = 0`

每个任务都对一组固定数据执行 `load()` 计算，`load()` 的运行时间基本随输入值线性增长。这样设计的目的是：

- 让任务持续足够久，能观察到调度差异
- 保持总结果可被精确校验

### 1.2 真实调用关系
```mermaid
flowchart LR
    A["thread::spawn"] --> B["ax-task::spawn_raw"]
    B --> C["任务启动"]
    C --> D["ax_set_current_priority(nice)"]
    D --> E["ax-task::set_priority"]
    E --> F["调度器按策略运行"]
    F --> G["join + 结果汇总"]
```

`ax_set_current_priority()` 在 `ax_api::task` 中最终调用的是 `ax-task::set_priority(prio)`，而 `ax-task` 的实现明确说明：在 CFS 下这个值是 `nice`，范围通常是 `-20..19`。

### 1.3 为什么顺序断言被限定得很窄
源码只在下面这个条件下做完成顺序断言：

- `cfg!(feature = "sched-cfs")`
- `available_parallelism() == 1`

这非常重要。它说明当前作者并没有声称“所有环境下优先级都会表现成固定顺序”，而只是说：

- 在 CFS
- 且单核

时，较高优先级的短任务应更早离开。

而默认 `qemu-riscv64.toml` 使用 `-smp 4`，所以在自动回归里，这个顺序断言通常不会触发。当前自动化更偏向“API 与计算结果 smoke test”，而不是严格调度次序验证。

## 2. 核心功能说明
### 2.1 测试覆盖内容
这个 crate 实际覆盖了三件事：

1. `ax_set_current_priority()` 可以被调用且不会破坏任务执行。
2. 多个带不同 `nice` 的任务仍能正确完成工作并 `join`。
3. 在窄场景下，优先级差异会反映到离开时间顺序上。

### 2.2 为什么还要比较 `expect` 与 `actual`
如果只看离开时间，不足以判断调度路径是否正确。源码先对所有任务输入做一次基线求和，再把各任务结果汇总比较：

- `expect`：主线程串行计算得到
- `actual`：子任务并发完成后汇总得到

这样就能确认优先级调度没有把任务执行结果本身搞坏。

### 2.3 边界澄清
它不负责：

- 证明某个调度器一定更高效
- 给出稳定的跨平台性能排序
- 评估多核抢占细节

它只在可控场景下验证优先级设置接口及其基本可观察效果。

## 3. 依赖关系图谱
```mermaid
graph LR
    test["arceos-priority"] --> ax-std["ax-std(alloc, multitask)"]
    ax-std --> ax-api["ax_api::task"]
    ax-api --> ax-task["ax-task / scheduler"]
```

### 3.1 直接依赖
- `ax-std(alloc, multitask)`：需要堆对象、线程和 `join`。
- `sched-rr` / `sched-cfs`：通过 feature 透传到底层调度器。

### 3.2 关键间接依赖
- `ax_api::task::ax_set_current_priority`
- `ax-task::set_priority`
- 底层调度器实现（FIFO/RR/CFS 中的一种）

### 3.3 主要消费者
- `ax-task` 调度器优先级路径改动后的回归。
- `cargo arceos test qemu` 自动收集的任务语义测试集合。

## 4. 开发指南
### 4.1 推荐运行方式
```bash
cargo xtask arceos run --package arceos-priority --arch riscv64
```

或：

```bash
cargo arceos test qemu --target riscv64gc-unknown-none-elf
```

### 4.2 修改时的注意点
1. 若要验证顺序，必须明确是单核还是多核。
2. 若要验证 CFS 的 `nice` 语义，必须同时确认对应 feature 已打开。
3. 不要把输出时间当成性能基准报告；这里只能做相对语义检查。

### 4.3 更强验证的推荐方式
如果你要真正验证“高优先级更早完成”的行为，建议额外准备：

1. 单核 QEMU 配置
2. 显式打开 `sched-cfs`
3. 保持当前工作负载规模稳定

否则默认 4 核配置下，顺序本来就不应被写死。

## 5. 测试策略
### 5.1 当前自动化形态
`qemu-riscv64.toml` 使用：

- `-smp 4`
- `success_regex = ["Priority tests run OK!"]`
- panic 关键字失败匹配

说明它已进入自动回归，但默认更像“优先级设置 smoke test”。

### 5.2 成功标准
- 所有任务计算都能完成
- `actual == expect`
- 在满足单核 CFS 条件时，离开时间顺序满足断言
- 最终打印 `Priority tests run OK!`

### 5.3 风险点
- 多核下不要误把日志顺序波动理解为 bug。
- 调度器 feature 切换后，要重新评估当前断言是否仍合理。

## 6. 跨项目定位分析
### 6.1 ArceOS
它是 ArceOS 调度器优先级语义的专门回归入口，但只关注任务侧行为，不属于可复用子系统。

### 6.2 StarryOS
StarryOS 不直接运行它；不过共享任务调度实现改动时，这种短路径回归依然具有预警价值。

### 6.3 Axvisor
Axvisor 也不会直接依赖它。它的跨项目意义只在于：先用简单工作负载确认共享调度基础没有回退，再进入更复杂场景。
