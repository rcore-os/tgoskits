---
sidebar_position: 5
sidebar_label: "能力边界"
---

# 能力边界 rdif

`rdif-*` 是能力边界（capability boundary），只定义某类设备向上暴露什么能力，不负责设备发现、iomap、IRQ 注册、任务调度或系统启动顺序。`rdif-block` 承载 owned request、线性 IRQ evidence、分阶段激活和控制器生命周期契约；software ctx、tag/credit、CPU 固定维护域、watchdog、阻塞等待及恢复编排属于 `ax-runtime::block`。其它领域如网络仍可按需保留独立 runtime wrapper，负责 waker、poll、blocking API、buffer pool 等运行时行为。

所有 `rdif-*` crate 位于 `drivers/interface/`，公共基础是 `rdif-base`。

## 能力边界总览

| 能力 | interface crate | runtime crate | 上层消费 |
| --- | --- | --- | --- |
| 块设备 | `rdif-block` | `ax-runtime::block` | block volume service、FS |
| 网络设备 | `rdif-eth` | `rd-net` | net interface service、NET/NET-NG |
| 显示 | `rdif-display` | `rd-display` | display service、Starry fb |
| 输入 | `rdif-input` | `rd-input` | input service、Starry input |
| vsock | `rdif-vsock` | `rd-vsock` | vsock service |
| 平台设备 | `rdif-intc`、`rdif-pinctrl`、`rdif-pcie`、`rdif-clk`、`rdif-timer`、`rdif-systick`、`rdif-serial`、`rdif-pwm`、`rdif-power` | 按需 | HAL、Axvisor backend、平台 glue |

`rdif-base` 定义所有能力 trait 的公共基础：

```rust
pub trait DriverGeneric: Send + Any {
    fn name(&self) -> &str;

    fn raw_any(&self) -> Option<&dyn Any> { None }
    fn raw_any_mut(&mut self) -> Option<&mut dyn Any> { None }
}
```

每个 `rdif-*::Interface` trait 都继承 `DriverGeneric`，并定义该领域能力契约。设备实现 trait 后通过 `PlatformDevice::register()` 注册到 `rdrive`，上层通过 `Device<T>` 弱引用查询。

## rdif-block

`rdif-block` 是块设备能力边界，源码位于 `drivers/interface/rdif-block/`。块请求不暴露 Linux block layer 的 512B sector 公共单位，而使用真实设备的 `lba` / `block_count` / `logical_block_size`。OS glue 负责把上层 byte offset、FS block、Linux-like sector 或分区 region 转换成设备 LBA。

| 源码 | 职责 |
| --- | --- |
| `activation/` | capability、activation plan、control/I/O ownership parts、Ready publication |
| `interface.rs` | 迁移期 legacy `Interface` / `IQueue`，不作为新硬件驱动入口 |
| `request.rs` | `RequestId`、`OwnedRequest`、typed submit result |
| `planner.rs` | request transfer 分段与硬件约束规划 |
| `evidence.rs` | `IrqEvidenceId`、`PendingBlockIrq`、drain/rearm proof |
| `irq.rs` | 迁移期 queue bitmap 事件 |
| `init.rs` | discovery-to-ready 初始化状态机 |
| `lifecycle.rs` | recovery/handoff 与 DMA quiescence proof |
| `info.rs` | 设备信息 |
| `error.rs` | `BlkError`、queue contract error |

接口保留 blk-mq 风格的结构能力，但不复制 Linux 的 polling、elevator 或热插拔实现。runtime 为每个 CPU 建一个 software ctx，通过冻结的 CPU→hctx map 路由到 hardware queue；在调用驱动前先安装 generation-based `RequestId`/tag、deadline、inflight 状态和硬件 credit。credit 深度由 ownership domain 的 activation plan 选择，并由最终 queue descriptor 精确实现；它不是 logical device/namespace 的属性。`InterruptIoDomain::submit_owned()` 只能返回 accepted，或返回完整 `UnacceptedRequest` 及“描述符和 doorbell 从未对硬件可见”的线性证明；accepted 后的错误只能由 IRQ evidence 或 recovery 终结。纯软件设备走独立 `InlineExecuteQueue`，在调用栈中归还完整 request，不分配 tag、waiter 或 IRQ 资源。

硬件 discovery 不允许同步执行 reset/identify，也不能伪造初始化后才能知道的 namespace、capacity 或 block size。driver 先发布 controller identity、ownership-domain/IRQ 能力和硬件约束，runtime 冻结 `ActivationPlan`，再把 prepared control owner 移到最终 CPU。该线程亲自注册 control IRQ action 后才启动有界初始化状态机。状态机的下一触发条件只能是明确的内部硬件进展、IRQ source 或绝对 `wake_at_ns`；deadline 只用于 reset/clock/power/OCR/PHY 等协议等待，不能探测普通 I/O 是否完成。

`Ready` 也不能把所有 driver queue 再集中到 controller 对象。它先生成只含 catalog/route 的 publication coordinator 与若干 move-only unbound domain；每个 domain 移入最终维护线程，在该线程绑定精确 IRQ source 后变为 `!Send`，只向 coordinator 返回不可复制的 binding proof。coordinator 收齐全部 proof 后才一次性发布 logical-device geometry 与 route。不同 I/O domain 的 portable source ID 必须互斥；多个不同 source 映射到同一物理 shared line 是 OS binding 事实，并使这些 domain 固定到同一 CPU。shared control 只能复用其 I/O domain 的精确 source 子集。

`SharedWithIo` 有两种物理实现，不能用同一种共享对象强行模拟。寄存器和 queue storage 真正可分离时，可以发布独立的 move-only I/O domain；初始化、I/O、恢复共用同一命令引擎时，control part 必须保留唯一 concrete owner，只把最终 queue 描述发布为不可变事实，并在同一维护线程内通过短生命周期 `&mut` 借出 `InterruptIoDomain`。设备 IRQ enable/disable 同样是硬件状态修改，portable control API 必须使用 `&mut self`。禁止用 `Arc<UnsafeCell<_>>`、大锁或两个同时可调用的 trait object 伪造拆分所有权。

平台侧的 binding facts、父 IRQ allocation lease 与 portable activator 同样是一个不可拆分的线性事务。`ax-driver` 只暴露只读查询和 `Discovered → Prepared → Staged → PublicationOwner → Published` 转换；任一失败都返还完整 owner。`Staged` 只能一次性拆成 publication owner 与 unbound domains，不提供会留下“已经取空但类型未改变”状态的重复 `take_*` API。

块设备内部的 IRQ 事实按 source 和 driver ledger slot 分离。IRQ endpoint 先检查真实硬件事实；shared INTx 在没有有效 CQ phase/status 时必须返回 `Unhandled`。捕获成功后，endpoint 把完整 typed snapshot 写入预分配 ledger，只向 runtime 返回包含 source、device generation、slot 和 slot generation 的 `IrqEvidenceId`。runtime 将其包装为不可复制的 `PendingBlockIrq`；维护线程每次只能返回 `Drained`、`Retained` 或 `Recover`。只有 drained proof 可以生成 rearm permit，旧 evidence、mask epoch 或 lifecycle generation 都不能重开新 source。锁竞争不是硬件事实，也不存在 deferred acknowledgement。

timeout 和 recovery 通过 lifecycle 契约保持所有权严格：watchdog 获胜后直接停止 dispatch 并进入恢复，不读完成寄存器补发现成功。recovery 先 mask/synchronize IRQ，再 quiesce DMA、增加 queue epoch，随后才终结请求并返还 buffer。无法证明 DMA 已停止、action 已关闭或 evidence 已收回时，完整 MMIO/CQ/DMA/token owner 进入有界命名 quarantine，而不是伪造 proof、匿名泄漏或继续发布 queue。

## rdif-display / rdif-input / rdif-vsock

这三个能力边界按 `error/types/interface` 或 `addr/event/interface` 拆文件：

| crate | 文件拆分 |
| --- | --- |
| `rdif-display` | `types.rs`（`DisplayInfo`、`PixelFormat`、`FrameBuffer`）、`error.rs`（`DisplayError`）、`interface.rs`（`Interface`、`Event`） |
| `rdif-input` | `event.rs`（`EventType`、`InputEvent`、`AbsInfo`）、`id.rs`（`InputDeviceId`）、`error.rs`（`InputError`）、`interface.rs`（`Interface`、`Event`） |
| `rdif-vsock` | `addr.rs`（`VsockAddr`、`VsockConnId`）、`event.rs`（`VsockEvent`）、`error.rs`（`VsockError`）、`interface.rs`（`Interface`、`Event`） |

接口目标形态：

```rust
pub trait DisplayInterface: rdif_base::DriverGeneric {
    fn info(&self) -> DisplayInfo;
    fn framebuffer(&mut self) -> Result<FrameBuffer<'_>, DisplayError>;
    fn need_flush(&self) -> bool;
    fn flush(&mut self) -> Result<(), DisplayError>;
    fn handle_irq(&mut self) -> DisplayEvent;
}
```

```rust
pub trait InputInterface: rdif_base::DriverGeneric {
    fn device_id(&self) -> InputDeviceId;
    fn physical_location(&self) -> &str;
    fn unique_id(&self) -> &str;
    fn get_event_bits(&mut self, ty: EventType, out: &mut [u8]) -> Result<bool, InputError>;
    fn read_event(&mut self) -> Result<InputEvent, InputError>;
    fn get_prop_bits(&mut self, out: &mut [u8]) -> Result<usize, InputError>;
    fn get_abs_info(&mut self, axis: u8) -> Result<AbsInfo, InputError>;
    fn handle_irq(&mut self) -> InputEventState;
}
```

```rust
pub trait VsockInterface: rdif_base::DriverGeneric {
    fn guest_cid(&self) -> u64;
    fn listen(&mut self, port: u32) -> Result<(), VsockError>;
    fn connect(&mut self, id: VsockConnId) -> Result<(), VsockError>;
    fn send(&mut self, id: VsockConnId, buf: &[u8]) -> Result<usize, VsockError>;
    fn recv(&mut self, id: VsockConnId, buf: &mut [u8]) -> Result<usize, VsockError>;
    fn recv_avail(&mut self, id: VsockConnId) -> Result<usize, VsockError>;
    fn disconnect(&mut self, id: VsockConnId) -> Result<(), VsockError>;
    fn abort(&mut self, id: VsockConnId) -> Result<(), VsockError>;
    fn poll_event(&mut self) -> Result<Option<VsockEvent>, VsockError>;
    fn handle_irq(&mut self) -> VsockIrqEvent;
}
```

IRQ 路径只返回稳定事件和唤醒等待方；不能在 IRQ handler 中执行阻塞 I/O、长流程状态推进或广域锁持有。

## rdif-pinctrl

`rdif-pinctrl` 是 pinctrl、GPIO、GPIO IRQ 的能力边界，分成三个独立 endpoint：`Interface`、`GpioBank`、`GpioIrqHandler`。`Interface` 只描述 pins/groups/functions/configs/states 这些 Linux pinctrl 模型中的稳定语义，但用 `PinId`、`GroupId`、`FunctionId`、`GpioLineId`、`MuxValue` 和 typed `PinConfig` 表达，不引入全局字符串 registry、packed `unsigned long` config、devm/module/debugfs 语义。`PinState` 应用顺序固定为先 mux 再 pin config。

GPIO line 所有权通过 `GpioLineHandle` 表达。consumer 先向 `GpioBank` request line，后续 direction/read/write 必须带 handle，避免裸 `PinId` 被多个调用方重复配置。GPIO IRQ 与 GPIO control path 分离：`Interface::take_irq_handler(source_id)` 把 `Box<dyn GpioIrqHandler>` 所有权移交给 OS runtime，runtime 再把 handler move 进 IRQ registration closure；task/control path 不共享 handler。`GpioIrqHandler::handle_irq()` 只返回 pending line mask、edge/level/error/overflow 事件，不做 OS wakeup、任务调度、IRQ 注册或 GPIO consumer 回调。

FDT/ACPI 解析不进入 `rdif-pinctrl` portable core。`rdrive` / `ax-driver` probe glue 负责把 FDT consumer node 的 `pinctrl-names` + `pinctrl-N`、SoC-specific `rockchip,pins`、`gpio-ranges`、`gpios` / `gpio` 等解析成 `PinState`、`MuxSetting`、`PinConfig` 或 `GpioLineId`。ACPI 第一版只暴露 `AcpiPinStateSpec` / `AcpiGpioLineSpec` 这类 typed metadata；仓库尚无 Linux-style ACPI pinctrl state parser 时，probe glue 必须返回明确的 `PinctrlError::UnsupportedFirmware(FirmwareKind::Acpi)`，不能静默 fallback。

## 文件拆分规则

新增 crate 默认遵循以下布局：

```text
src/
  lib.rs          # re-export only
  error.rs       # error type and conversions
  types.rs       # public data types
  interface.rs   # trait and event contract
  device.rs      # runtime device wrapper, if this is rd-* crate
  irq.rs         # irq event handling, if needed
  queue.rs       # queue/request/event stream, if needed
```

`lib.rs` 只做模块声明和 re-export，不承载核心实现。已有大文件在迁移触及时必须拆分。
