---
sidebar_position: 5
sidebar_label: "能力边界"
---

# 能力边界 rdif

`rdif-*` 是能力边界（capability boundary），只定义某类设备向上暴露什么能力，不负责设备发现、iomap、IRQ 注册、任务调度或系统启动顺序。块设备已移除原 runtime crate，`rdif-block` 直接承载设备 LBA 语义的 submit/poll capability boundary；其它领域如网络仍可按需保留 runtime wrapper，负责 waker、poll、blocking API、buffer pool 等运行时行为。

所有 `rdif-*` crate 位于 `drivers/interface/`，公共基础是 `rdif-base`。

## 能力边界总览

| 能力 | interface crate | runtime crate | 上层消费 |
| --- | --- | --- | --- |
| 块设备 | `rdif-block` | 已删除，直接消费 submit/poll 边界 | block volume service、FS |
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
| `interface.rs` | `Interface` trait、`IrqHandler` |
| `request.rs` | `RequestId`、块请求提交 |
| `planner.rs` | `QueueTopology`、queue 管理 |
| `irq.rs` | `IrqSourceInfo`、IRQ 事件 |
| `info.rs` | 设备信息 |
| `error.rs` | `BlockError` |

接口保留 blk-mq 风格的结构能力：设备可报告 `QueueTopology`，OS 可创建一个或多个 queue，每个 queue 使用 queue-local `RequestId`/tag，经 `submit_request()` 提交、经 `poll_request()` 回收完成。

块设备内部的 IRQ 事件按 source 和 queue 分离。`Interface::irq_sources()` 返回的是 `rdif-block` 能力边界内的事件 source 列表，每个 `IrqSourceInfo { id, queues }` 描述该硬件事件 source 可能影响的 queue mask。当前 ArceOS `ax-driver` glue 只取 legacy source `0` 的 handler。

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
