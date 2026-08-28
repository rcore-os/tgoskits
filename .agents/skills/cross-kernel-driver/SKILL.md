---
name: cross-kernel-driver
description: 按设备类型创建、重构、审查或优化本 TGOSKits 工作区 `drivers/` 下的可移植 Rust 驱动软件包。新增或修改跨 Rust 内核驱动、划分驱动核心层、能力边界层、操作系统适配层和运行时层、通过 `mmio-api` 处理内存映射输入输出、通过 `dma-api` 处理直接内存访问、设计中断回调所有权、控制/中断/队列端点契约、队列局部完成状态，或审计驱动代码对操作系统接口的耦合时使用。
---

# 跨内核驱动

## 概述

通过分离稳定硬件逻辑与操作系统接口耦合，使可复用驱动软件包能够跨 Rust 内核使用。目标分层如下：驱动核心层负责寄存器、描述符、状态机、队列和事件；能力边界层负责内存映射输入输出、直接内存访问、中断和队列契约；操作系统适配层负责探测、设备资源映射、中断注册和任务调度；运行时层负责阻塞、轮询、异步任务或工作任务接入。中断驱动设备应进一步拆成控制端点、中断处理端点和队列端点，使每个端点只有一个明确所有者与同步契约。

进行非平凡驱动设计或重构前，完整阅读 `references/architecture.md`。

## 工作流程

1. 检查目标设备、`drivers/` 下现有软件包、根 `Cargo.toml`，以及 `platforms/axplat-dyn/src/drivers` 下可能存在的平台适配代码。
2. 可复用硬件或知识产权核软件包放在 `drivers/<device-type>/...`。现有布局适合，或需要避免歧义时增加厂商或系列子目录。
3. 保持 `src/` 与操作系统无关。目标内核适配、扁平设备树探测、板卡设置、设备资源映射、中断注册和操作系统唤醒放在测试、示例、平台适配层或独立适配软件包中。
4. 供本仓库使用的新驱动软件包加入工作区 `members`；其他工作区软件包需要依赖时，也加入根 `[workspace.dependencies]`。
5. ArceOS 或动态平台接入继续使用 `platforms/axplat-dyn/src/drivers/blk` 等既有平台模块名，即使可复用软件包位于 `drivers/block`。
6. 使用小型能力特征或接口对象，不建立单体 `KernelHal`。分别定义内存映射输入输出、直接内存访问、中断事件、队列和唤醒或轮询边界。
7. 把队列建模为独立运行单元，优先提供 `submit`、`reclaim` 和消费队列局部同步事件的接口。生产块设备路径使用有所有权的批量提交和中断触发完成排空，不增加请求轮询。
8. 中断驱动设备分离控制、中断和队列端点。控制端点负责启动、配置和服务操作；中断端点同步硬件事件；队列端点依据队列局部状态提交或回收工作。
9. 能移入注册中断回调时，把生命周期敏感的中断处理端点直接移入回调。优先使用 `FnMut`、有所有权的装箱回调或等价注册令牌，不要用 `Arc<Mutex<_>>` 共享中断处理器。
10. 中断处理器只把硬件事件同步到队列局部完成状态。队列不得通过锁住中断处理器或重新读取共享且具破坏性的中断状态推进工作。
11. 中断路径返回稳定事件：有所有权端点通常使用 `handle_irq(&mut self) -> Event`，无状态原始提取器可使用 `handle_irq() -> Event`。普通设备由操作系统适配层决定后续执行方式；网络中断必须遵守下文的固定处理器轮询组契约，只能激活自身所有者执行任务。
12. 中断与任务路径共享可变状态时，必须有明确排他协议：任务侧先屏蔽精确中断源，再取得锁并修改；中断侧只访问预先注册且生命周期稳定的状态。记录生命周期和安全契约。无法证明时，使用原子待处理位和延后工作任务。
13. 完成前运行格式化和定向静态检查。

## 依赖规则

- 可复用驱动的普通 `[dependencies]` 不得引入操作系统特定软件包。
- 操作系统特定测试或运行时软件包放入 `[dev-dependencies]`，除非该软件包本身就是操作系统适配层。
- 根工作区已经声明的依赖使用 `foo.workspace = true`。
- 内存映射输入输出边界优先采用最新 `mmio-api`，直接内存访问边界优先采用最新 `dma-api`。2026 年 4 月 28 日查询到 `mmio-api = "0.2.1"`、`dma-api = "0.7.2"`；升级前重新运行 `cargo search` 或 `cargo info`。
- 根 `[workspace.dependencies]` 已包含 `dma-api`。需要广泛接入 `mmio-api` 时，把它加入根依赖并由成员软件包通过工作区引用。

## 内存映射输入输出

- 可移植驱动核心不得直接调用操作系统的 `ioremap` 或 `iomap`。
- 在操作系统适配层实现或使用 `mmio_api::MmioOp`；映射、解除映射、失败处理和映射生命周期都留在该层。
- 按相邻软件包风格，把已映射区域作为 `mmio_api::Mmio`、`mmio_api::MmioRaw`、`NonNull<u8>` 或类型化寄存器包装器传入驱动核心。
- 不安全指针构造靠近映射边界，并写明安全契约。

## 直接内存访问

- 把直接内存访问视为能力边界，不是方便分配内存的快捷方法。
- 操作系统适配层实现 `dma_api::DmaOp`，再通过 `dma_api::DeviceDma::new(dma_mask, &impl)` 创建设备能力。
- 驱动核心优先使用 `DArray`、`DBox`、`SArrayPtr`、`DmaDirection`、`DmaAddr`、`DmaHandle` 和 `DmaMapHandle`，不要自行维护松散总线地址。
- 每条路径都处理地址掩码或宽度、对齐、缓存同步方向、所有权与生命周期、零复制传输所有权，以及总线地址与中央处理器虚拟地址的区别。

## 接口形状

排他访问是自然契约时使用 `&mut self`。可移植抽象不得要求调用者传入操作系统锁。只有中断回调能够调用处理器时，把处理器移入回调并公开 `handle(&mut self, ...)`，不要把它设计为可克隆共享对象。

ArceOS 块设备接入通过 `rdif_block::BlockController` 暴露可移植控制器，通过 `rdif_block::HardwareQueue` 暴露只能移动的队列。通道创建、等待策略、中断注册和任务通知留在操作系统适配层或运行时层。可移植边界从接受提交开始持有直接内存访问所有权，直到请求终结。

优先使用小接口：

```rust
pub trait HardIrqHandler {
    fn ack(&mut self) -> IrqAck;
}

pub trait HardwareQueue {
    fn id(&self) -> usize;
    fn submit_batch_owned(
        &mut self,
        requests: &mut OwnedRequestBatch,
        accepted: &mut dyn SubmissionSink,
    ) -> BatchSubmitResult;
    fn commit_submissions(&mut self) -> Result<(), BlkError>;
    fn drain_completions(
        &mut self,
        completed: &mut dyn CompletionSink,
    ) -> Result<(), BlkError>;
}
```

`submit_batch_owned` 只移走已接受的前缀，未接受请求仍由运行时持有。`commit_submissions` 每批只发布一次已接受描述符。只有队列所有者收到匹配且已确认的中断事件后，才能调用 `drain_completions`。

硬中断处理器只识别、清除或屏蔽中断源，并把 `IrqAck` 发布到预分配状态。不得排空队列、改变直接内存访问所有权、分配内存或完成任务。审查时始终坚持“中断只同步状态，任务才推进流程”。

复杂运行时可以返回分离部件：

```rust
pub struct DeviceParts {
    pub control: Arc<ControlPort>,
    pub irq: IrqHandler,
    pub queues: QueueSet,
}
```

把 `irq` 移入操作系统中断回调。任务或工作任务只持有 `control` 和队列端点，不持有中断处理器。

### 网络轮询组契约

中断驱动网络设备必须拆成 `rdif_eth::NetDeviceParts`。每个 `NetPollGroupParts` 独占一个屏蔽与重新使能域、接收和发送队列、任务上下文中的 `NetPollIrqControl`，以及只能移动的 `NetHardIrqEndpoint`。运行时为每个物理中断亲和域选择一个 `owner_cpu`，并保证：

```text
中断回调处理器 == 轮询组所有者处理器 == 队列处理处理器
```

- 每个网络中断动作以禁用、不可重入和 `IrqAffinity::Fixed(owner_cpu)` 注册。同一物理 `IrqId` 上的共享动作必须使用同一处理器，否则初始化失败。
- 硬中断只执行有界屏蔽、确认、状态快照并发布目标组。不得分配内存、触碰直接内存访问数据、调用协议代码、调用任意唤醒器、等待传输门控或唤醒无关组。
- 固定处理器队列所有者独占直接内存访问队列排空与补充、预算、背压和队列为空后的原子 `rearm_and_check()`。重新使能时发现待处理事件，应立即重新安排同一组。
- `DmaBuffer` 和提交错误保持只能移动，使所有权穿过队列与协议间的单生产者单消费者环时恰好转移一次。
- 安全数字输入输出设备的嵌套中断源随网络部件一起移动。控制器卡中断状态、先进先出缓冲区排空、固件命令和无线网络控制事务都在同一所有者域执行；控制调用方提交事务，不直接访问设备总线或内存映射寄存器。
- 禁止加入 `IrqAffinity::Any`、中断转移到远端处理器、带外接收回调、全组唤醒、周期设备轮询、额外触发任务或无中断后备路径。缺少固定路由、工作任务固定、中断源或原子屏蔽与重新使能能力时，物理网络初始化必须失败。

驱动有意在任务设置和中断完成之间共享登记表或队列映射时，采用类似 xHCI 的排他协议：任务上下文在修改前屏蔽同一设备中断器或消息信号中断源；中断上下文不取得同一锁，只访问中断启用前已建立生命周期的条目。这能避免同锁中断重入死锁，但不意味着硬中断可以分配内存、阻塞、调用任意唤醒器或进入无关操作系统回调。

分离队列设计中，中断处理器不得取得任务上下文可能持有的队列互斥锁。若中断与队列共享寄存器块，中断端点应成为共享或破坏性中断状态的唯一读取和清除者，再通过预分配原子状态分发结果。块设备维护任务独占 `HardwareQueue`；硬中断只确认、锁存和通知，绝不调用 `drain_completions`。

## 验证

运行：

```bash
cargo fmt
cargo xtask clippy --package <crate>
```

平台适配层变化时再运行：

```bash
cargo xtask clippy --package axplat-dyn
```

通用 ArceOS 适配器变化时运行相应软件包，例如：

```bash
cargo xtask clippy --package ax-driver-net
```

驱动软件包现已通过静态检查，但不在 `scripts/test/clippy_crates.csv` 中时，在同一修改中加入。

`drivers/*/tests` 下的板卡或裸机测试可能需要软件包专用运行器或实体硬件，应视为目标特定验证，不默认认定为可在持续集成中安全执行。

## 参考资料

- `references/architecture.md`：跨内核驱动的详细分层、所有权、并发和审查规则。
