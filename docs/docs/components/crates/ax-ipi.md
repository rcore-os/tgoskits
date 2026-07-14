# `ax-ipi`

> 路径：`os/arceos/modules/axipi`
> 类型：库 crate
> 分层：ArceOS 层 / IPI 运行时基础件
> 版本：`0.5.27`
> 文档依据：`Cargo.toml`、`README.md`、`src/lib.rs`、`src/event.rs`、`src/queue.rs`

`ax-ipi` 是 ArceOS 的跨核回调分发模块。它基于 `ax-hal` 的 IPI 发送能力，为每个 CPU 维护一个本地事件队列，并向上暴露“在某个 CPU 上运行闭包”或“在所有其他 CPU 上广播闭包”的接口。它属于运行时叶子基础件：负责 IPI 事件排队和派发，不负责 SMP bring-up、调度策略或通用消息总线。

## 架构设计
### 设计定位
`ax-ipi` 的核心目标不是实现一个复杂的多核通信框架，而是提供一条很短的工作链：

1. 把闭包包装成可发送的 IPI 事件。
2. 放入目标 CPU 的本地队列。
3. 通过 `ax-hal::irq::send_ipi()` 触发对方 CPU 进入 IPI 中断。
4. hard-IRQ handler 只发布当前 CPU 有 deferred work。
5. `ax-runtime` 在 IRQ 框架清除 hard-IRQ marker 后执行一个有界批次。
6. 如果队列仍非空，向当前 CPU 发送 follow-up IPI，继续下一批。

因此，`ax-ipi` 更像“IPI 回调投递器”，而不是调度器、work queue 或通用 RPC 层。

### 模块结构
- `src/lib.rs`：初始化、单播/广播发送和 IPI handler 主线。
- `src/event.rs`：回调封装，区分单次消费的 `Callback` 与可克隆广播的 `MulticastCallback`。
- `src/queue.rs`：基于 `VecDeque` 的 `IpiEventQueue`，以 FIFO 顺序存放待处理事件。

### 1.3 关键对象
- `Callback`：`Box<dyn FnOnce()>` 封装的单播回调。
- `MulticastCallback`：`Arc<dyn Fn()>` 封装的广播回调，可拆成多个单播回调。
- `IpiEvent`：记录源 CPU ID 与具体回调。
- `IpiEventQueue`：每 CPU 一个的待处理事件队列。
- `IPI_EVENT_QUEUE`：通过 `#[ax_percpu::def_percpu]` 声明的每 CPU 静态 `LazyInit<SpinNoIrq<IpiEventQueue>>`。

### 1.4 发送与处理主线
发送到单个 CPU 的流程如下：

```mermaid
flowchart TD
    A["run_on_cpu(dest, callback)"] --> B{"dest == 当前 CPU ?"}
    B -- 是 --> C["立即执行 callback"]
    B -- 否 --> D["取目标 CPU 的 IPI_EVENT_QUEUE"]
    D --> E["push(src_cpu_id, callback)"]
    E --> F["ax-hal::irq::send_ipi()"]
    F --> G["目标 CPU 进入 ipi_handler()"]
    G --> H["只设置 IPI_DEFERRED_PENDING"]
    H --> I["IRQ EOI 并清除 hard-IRQ marker"]
    I --> J["IRQ-return safe point 最多执行 64 个"]
    J --> K{"队列仍非空?"}
    K -- 是 --> L["向当前 CPU 发送 follow-up IPI"]
    K -- 否 --> M["返回被中断上下文"]
```

实现里的重要细节：

- `run_on_cpu()` 遇到目标就是当前 CPU 时不会排队，而是同步立即执行。
- `run_on_each_cpu()` 会先在当前 CPU 上立刻执行一份，再把 clone 后的回调投给其他 CPU。
- `ipi_handler()` 不访问回调队列，只发布 pending；因此 hard IRQ 不会执行或析构 `Box<dyn FnOnce()>`。
- `drain_deferred_callbacks()` 只能在本地 IRQ 关闭且 hard-IRQ marker 已清除后运行，每次最多执行 64 个回调。
- 同步 raw call 使用 `Queued → Running/Cancelled → Done` 生命周期；调用方成功取消后，迟到回调不会读取调用方栈参数。

### 1.5 能力边界
- `ax-ipi` 队列是 FIFO，但不提供优先级、取消、重试或返回值汇总。
- 远端回调运行在目标 CPU 的 IRQ-return 安全点，本地 IRQ 仍关闭，但已不属于 hard-IRQ 上下文；回调仍必须短小且不可阻塞。
- 安全的回调投递 API 拒绝 hard-IRQ 调用，因为 `Box`/`Arc` 构造和 `VecDeque` 扩容可能分配。
- 这个 crate 没有自己的 feature 门控，但它依赖 `ax-hal` 已开启 `ipi` 能力。

## 核心功能
### 功能概览
- 初始化每 CPU 的 IPI 队列。
- 向指定 CPU 投递单次回调。
- 向所有其他 CPU 广播回调。
- 在 IPI 中断中只发布 deferred work，并在 IRQ-return 安全点有界执行。

### 使用场景
- `init()`：由 `ax-runtime/src/mp.rs` 在次核 bring-up 路径中调用，为当前 CPU 建立 IPI 队列。
- `ipi_handler()`：由 `ax-runtime/src/lib.rs` 的 hard-IRQ 路径调用，只设置 pending。
- `drain_deferred_callbacks()`：由 `ax-runtime` 的 IRQ-return preemption hook 调用。
- `run_on_cpu()` / `run_on_each_cpu()`：是这个 crate 的核心公开 API，也是 `ax-api` / `ax-runtime` 暴露 IPI 能力的底层基础。

### 边界说明
- 它不是 SMP 启动器；启动 CPU 的逻辑在 `ax_runtime::start_secondary_cpus()`。
- 它不是通用异步执行框架；没有 future、返回值或 work stealing。
- 它也不是调度器；闭包只在 IPI 到达后的 IRQ-return 安全点运行。

## 依赖关系
```mermaid
graph LR
    ax-hal["ax-hal (ipi)"] --> ax-ipi["ax-ipi"]
```

- `ax-runtime`：负责在启动链中初始化队列，并在 IRQ 处理里调用 `ipi_handler()`。
- `ax-api` / `ax-runtime`：把 IPI 能力向上层 feature 与 API 暴露。

## 开发指南
### 接入方式
```toml
[dependencies]
ax-ipi = { workspace = true }
```

通常只有在上层 feature 打开 `ipi` 时，最终镜像才会真正把它编进去。

### 注意事项
1. `init()` 是每 CPU 初始化，不是全局初始化；修改时必须同时考虑 BSP 和 AP 路径。
2. `run_on_each_cpu()` 当前包含“立即执行当前 CPU”这一步，修改时不能无意改变这个语义。
3. `Callback` / `MulticastCallback` 的封装关系要保持清晰，避免把广播路径退化成共享可变闭包。
4. 不要把复杂的等待、应答、重传协议塞进 `ax-ipi`；这层应该继续保持单纯。
5. 不得把 callback 的执行或析构移回 hard IRQ；批次上限和 follow-up IPI 是 hard-IRQ 延迟边界的一部分。

### 4.3 开发建议
- IPI 回调应尽量短小，只做必要的跨核通知或状态翻转。
- 若需要可取消定时或异步 work queue，应该另建专门机制，而不是滥用 IPI 队列。
- 若引入更复杂的 IPI 事件类型，优先扩展事件内容，而不是让 `ax-ipi` 直接理解高层业务语义。

## 测试
### 测试覆盖
`ax-ipi` 同时使用 crate 内生命周期测试、runtime source-contract 测试和真实 SMP 路径：

- `ax-runtime` 在启用 `ipi`/`smp`/`irq` 组合下的启动与中断处理；
- API 层对 IPI 能力的集成；
- 多核环境下回调能否确实落到目标 CPU。

### 单元测试
- `IpiEventQueue` 的 FIFO 行为。
- `MulticastCallback::into_unicast()` 的语义是否保持“一份广播拆成多份单播”。
- 当前 CPU 快路径是否绕过排队。
- hard-IRQ pending publication 不调用也不析构已排队回调。
- 同步调用 timeout 后的迟到回调不访问调用方参数。

### 集成测试
- QEMU/真实多核环境下的单播与广播是否都能触发。
- IRQ-return drain 是否遵守 64 个回调的批次上限，并通过 follow-up IPI 继续剩余工作。
- 与 `ax-runtime` 的 IRQ 注册/处理中断链是否匹配。

### 覆盖率
- 对 `ax-ipi`，SMP 集成覆盖比局部行覆盖率更关键。
- 涉及队列结构或广播语义的改动，都应覆盖“当前 CPU”“远端 CPU”“广播”三条路径。

## 跨项目定位
### ArceOS
`ax-ipi` 是 ArceOS 在打开 IPI feature 后的跨核通知基础件。它为运行时和 API 层提供最小可用的 IPI 回调能力。

### StarryOS
StarryOS 当前没有直接把 `ax-ipi` 作为独立系统层来扩展，更多是通过共享的 ArceOS 运行时栈间接受用。因此它在 StarryOS 中仍是叶子基础件，而不是并发主控层。

### Axvisor
当前仓库里的 Axvisor 没有直接依赖 `ax-ipi` 形成自己的 IPI 子系统；如果未来复用这套能力，也更可能把它当成宿主侧的跨核通知底座，而不是 hypervisor 调度层。
