# USB Host 端点生命周期

## 背景与问题

本设计统一 `crab-usb`、xHCI、EHCI、DWC2、libusb 后端及 StarryOS usbfs 对 USB interface、alternate setting 和 endpoint 的生命周期管理。

直接触发本次重构的问题发生在 OrangePi-5-Plus Starry robot 流程。合入 #1970 后，第二次 UVC `PUT_BALL` 在 pause 阶段打印：

```text
usb queue: completion address 0x18a95e0 is not registered
```

随后任务超时。旧 xHCI 实现会在 `Configure Endpoint` 和 `SET_INTERFACE` 完成前注册 pending transfer ring。由于 completion route 仅以 `(SlotId, Dci)` 为键，pending ring 静默覆盖了仍可能接收旧 Transfer Event 的 active ring，导致旧 URB 永远不能进入终态。

这个问题不是单一哈希表覆盖错误。旧接口允许调用方从 `Device` 取走 endpoint，把 USB core、HCD 和 usbfs 变成互不一致的多个事实源；取消也混淆了“软件提出取消”和“硬件已经停止引用 DMA”。若只在 xHCI 增加一个兼容分支，同一类 use-after-free、丢 completion 和断连后访问硬件的问题仍会在 configuration、release、EHCI、DWC2 和 usbfs 中存在。

## 依据

设计以以下语义为准：

- Linux 源码基线：本地 `/home/zhourui/linux-src` 的 `8cd9520d35a6`。
- USB core：`drivers/usb/core/message.c::usb_set_interface()`、`usb_disable_endpoint()`。
- HCD core：`drivers/usb/core/hcd.c::usb_hcd_flush_endpoint()`、`usb_hcd_disable_endpoint()`。
- xHCI：`drivers/usb/host/xhci.c` 和 `xhci-ring.c` 的 `Stop Endpoint`、`Set TR Dequeue Pointer`、`Configure Endpoint`、`new_ring` 提交逻辑。
- EHCI：`drivers/usb/host/ehci-hcd.c::ehci_urb_dequeue()`、`ehci_endpoint_disable()`，以及 `ehci-q.c` 的 async unlink/IAA 状态机。
- DWC2：`drivers/usb/dwc2/hcd.c::dwc2_hcd_urb_dequeue()` 和 `hcd_intr.c` 的 `CHHLTD` 处理。
- xHCI 1.2：4.3.6、4.6.6、4.6.9、4.6.10。

所有后端共同遵守的核心不变量是：

> 禁止新提交，取消并排空请求，等待 HCD 证明硬件停止引用，完成配置切换并发布新 endpoint，最后才允许释放旧硬件和 DMA 所有权。

## 范围与非目标

本次变更覆盖：

- `crab-usb` 的公开 interface/endpoint API；
- xHCI、EHCI、DWC2 和 umod/libusb 后端；
- Hub 拓扑断连传播；
- UVC、HID、USB serial、usbfs 和仓库内 USB 测试调用方；
- StarryOS UVC 板卡生命周期回归。

本次不增加 EHCI 或 DWC2 isochronous 支持。两个后端仍显式返回 `NotSupported`，避免把不完整的调度、带宽和完成语义混入生命周期修复。umod 使用 libusb 的 claim/cancel/event/release 能力，不模拟内核 HCD 命令。

## 共享对象与所有权

### `InterfaceSession`

`Device::claim_interface(interface, alternate)` 返回唯一的 `InterfaceSession`。session 拥有：

- interface number；
- 当前 alternate setting；
- 当前 alternate 的 `EndpointHandle` 集合；
- 与 `Device` 注册项共享的 session 状态。

`InterfaceSession` 不可复制。`set_alternate()` 和 `release()` 在执行任何 HCD 操作前校验 session 确实属于传入的 `Device`，错误设备不能冻结或重配置 endpoint。

`Device` 内部注册表保留全部已发布 endpoint core，使 release、configuration change 和 disconnect 始终可以定位并排空硬件资源。调用方不能删除或转移 endpoint 所有权。

删除的旧边界包括：

- `Device::endpoint()`；
- `take_endpoints()`；
- `take_endpoints_for_interface()`；
- `retire_request_after_quiesce()` 及其 capability 探测；
- usbfs 的 endpoint/interface 双份路由表和 backend 特判。

### `EndpointHandle`

`InterfaceSession::endpoint(address)` 返回 cloneable `EndpointHandle`。handle 只是一项 capability；HCD endpoint core 仍由 `Device` 注册表追踪。

handle 状态为：

```text
Active --freeze/release--> Revoked
Active --disconnect-----> Disconnected
Revoked --rollback------> Active
Revoked --disconnect----> Disconnected
```

- `Active`：允许提交。
- `Revoked`：拒绝新提交并返回 `TransferError::EndpointRevoked`；已提交请求仍可等待终态。
- `Disconnected`：拒绝提交并返回 `TransferError::Disconnected`。

不存在旧 API 别名或兼容包装层。公开类型名为 `EndpointHandle`，用于明确它不是可从 controller 所有权中移出的 endpoint 对象。

### interface/device 状态

逻辑状态机为：

```text
Active -> Quiescing -> Reconfiguring -> Active
   |           |             |
   |           +-------------+-- rollback failure --> Broken
   +-------------------------------- disconnect ----> Disconnected
```

`Quiescing` 和 `Reconfiguring` 是一次持有 `&mut InterfaceSession` 和 `&mut Device` 的事务阶段，不对外暴露可并发写入口。阶段开始时所有旧 handle 已变成 `Revoked`。成功提交后 session 发布新 handle；普通失败恢复旧 handle；任何无法确认硬件/设备一致性的恢复失败进入 `Broken`。

`Broken` 和 `Disconnected` 均为终止状态。`Broken` 返回 `USBError::InterfaceBroken`，禁止继续猜测硬件状态；`Disconnected` 返回 `TransferError::Disconnected`。

## alternate setting 事务

所有后端实现同一顺序，但 HCD prepare/teardown 的物理边界不同：

1. 从 descriptor 校验目标 alternate，预先分配新 endpoint/HCD 资源。资源不足时旧 session 完全不受影响。
2. 将旧 handle 设为 `Revoked`，阻止新提交。
3. 请求取消旧 URB，并同步等待所有请求离开 endpoint 队列。
4. 等待 HCD 特定的硬件停用边界。
5. 准备 controller 配置，但不发布 pending endpoint，不允许 pending ring/QH/QTD 接收 completion 或提交 TD。
6. 发送 USB `SET_INTERFACE`。
7. 成功后提交 controller 配置、发布新 completion route/endpoint handle，并更新 session alternate。
8. 失败时恢复旧 HCD 配置和旧 alternate，然后将旧 handle 恢复为 `Active`。
9. 任一恢复步骤失败时保留仍可能被硬件引用的对象，标记 `Broken`，不释放或重用其 DMA。

`set_configuration()`、`release()` 和 `disconnect()` 使用相同的 revoke、flush 和 HCD teardown 原语，不直接清空 endpoint map。

## 取消与终态

取消分为两个不同事件：

- cancellation request：软件记录取消意图，并要求 HCD unlink/stop/halt；
- terminal completion：HCD 已证明硬件不会再读取或写入该请求的 TD、QH、ring 或 DMA。

只有 terminal completion 可以：

- 从 endpoint queue 删除请求；
- 唤醒最终 waiter；
- 释放 transfer DMA；
- 释放或复用 controller 队列资源。

共享 flush 会为每个在途请求发出取消，然后同步等待 terminal completion。重复 completion 不会重复完成同一 request。

## xHCI

### ring 与 completion route

- active ring 和 pending ring 是不同对象。
- pending ring 在 `Configure Endpoint` 和 `SET_INTERFACE` 成功前不注册 route，也不接受 TD。
- `(SlotId, Dci)` 最多存在一个 active route。注册重复 route 返回错误，禁止静默覆盖。
- Transfer Event 除了查 `(SlotId, Dci)`，还校验 TRB DMA 地址属于该 active ring。
- 缺失 route 或 DMA 不属于 active ring 被提升为 controller fault；event handler 返回 `Event::Stopped`，不只打印 warning。

### 停止和恢复

旧 endpoint 执行：

1. 提交 `Stop Endpoint`。
2. event handler 按 event ring 顺序消费旧 Transfer Event 和 Stop Command Completion。
3. Stop Completion 后把旧请求完成为 cancelled，并释放 request DMA；旧 ring 对象仍保留用于回滚。
4. `Configure Endpoint` 使用 Drop/Add context 切换 controller endpoint context。
5. `SET_INTERFACE` 成功后，原子地撤销旧 route 并注册 pending route。

控制请求或 Configure 失败时，旧 context 被重新配置，`Set TR Dequeue Pointer` 恢复旧 ring cursor，再重新启用旧 handle。恢复失败时旧和 pending endpoint 进入 quarantine，设备进入 `Broken`。

物理断连使用 `Disable Slot` 作为 controller 停用边界；成功后才注销 route 和回收请求。失败对象进入 quarantine。

### 并发所有权

- command scheduler、IRQ completion sink 和 transfer producer 分离。
- `Ring` cursor、event ring segment table、DCBAA 和 scratchpad DMA 字段均为私有。
- `Ring`、`EventRing`、`DeviceContextList`、`Xhci` 不再无条件实现 `unsafe Send/Sync`。
- event ring 和 event-side registers 由 `EventHandlerState` 的单一 `SpinLock` 串行化；不再通过 `UnsafeCell` 从 `&self` 制造多个 `&mut`。

## EHCI

### schedule 模型

- control/bulk endpoint 使用真正的 async QH 链表，允许多个 QH 同时链接。
- interrupt endpoint 使用 1024 项 periodic frame list。
- isochronous endpoint 明确返回 `NotSupported`。

每个 endpoint 拥有一个 QH，在途 request 拥有 qTD 和 transfer DMA。pending alternate 的 QH 完整构造但不进入 schedule；只有 `SET_INTERFACE` 成功后的新提交才链接。

### unlink 安全边界

async QH 从 hardware chain 摘除后：

1. 执行写屏障并敲响 IAAD；
2. 第一次 IAA 只表示 controller 已观察到 unlink；不能回收 QH，因为部分 controller 仍可能回写 overlay；
3. 再发起一次 IAA；每个 unlink boundary 在第二次 IAA 后才完成；
4. boundary 完成后才清理 software overlay、完成 qTD 并释放 QH/DMA。

多个 unlink 由 `requested/completed/in_progress` scheduler 串行推进，不能让一次 IAA 把尚未经历双周期的所有 QH 一次性完成。IAAD 写入后回读 `USBCMD`，确保 posted write 到达 controller。

periodic QH 从 frame list 摘除后等待至少 9 个 microframe，跨过 EHCI 允许的预取窗口，之后才能回收。

## DWC2

### QH/QTD 与 channel

DWC2 不再以 endpoint number 静态绑定 host channel。`HostChannelPool` 动态分配 channel，lease owner 包含：

- endpoint address（包括方向位）；
- transfer direction；
- 当前 request id。

因此相同 endpoint number 的 IN/OUT request 不会共享或串扰 channel。

### halt 边界

- 尚未进入硬件的 queued request 可以直接 unlink。
- active request 的 cancel 只设置取消状态并请求 `CHDIS`。
- cancel 路径不伪造 `CHHLTD`。
- IRQ 读取真实 `HCINT.CHHLTD` 后才发布 terminal completion、释放 channel lease 和 transfer DMA。
- `XFERCOMP` 到达但 channel 尚未 halt 时，先请求 halt，仍等待 `CHHLTD`。

disconnect IRQ 在全局 lifecycle gate 内将 controller 标记为 disconnected、屏蔽 channel IRQ，并为所有 active channel 发布 `Disconnected` 终态。channel lease 的取得与寄存器启动不是同一个事实：即使任务已取得 lease，`start_stage()` 仍须在同一 gate 内重新校验 connected，避免 disconnect 与 `HCDMA`/`HCCHAR.CHENA` 写入交错。此后 endpoint cancel/reclaim 不再访问 host channel 寄存器；waiting、active 和 completed request 都能被拓扑断连流程排空。重新枚举时先恢复 IRQ 配置，再发布 connected。

## umod/libusb

umod 将共享事务映射到 libusb：

- 首次 claim 时处理 kernel driver detach 和 `libusb_claim_interface()`；
- alternate 切换前 cancel 全部 transfer，并持续处理 libusb event，直到真实 completion；
- 使用 `libusb_set_interface_alt_setting()` 提交切换；
- release 时调用 `libusb_release_interface()` 并按需重新 attach kernel driver；
- `LIBUSB_ERROR_NO_DEVICE` 映射为 `Disconnected`。

libusb backend 维护已发现 device identity 集合，只报告增量 connect/disconnect。USB core 再为拓扑实体分配单调递增的逻辑 device id，避免物理 slot/address 重用让旧打开文件指向新设备。

## Hub 与 disconnect

root hub 和外部 hub 都产生显式 `PortEvent::Connected/Disconnected`。USB core 保存 `(HubId, port) -> device/child hub` 拓扑并使用稳定、单调递增的 `HubId` 和 logical device id。

断开一个端口时，core 按叶到根顺序：

1. 移除并 disconnect 所有后代 hub；
2. 对未打开的 `DeviceOp` 执行 HCD disconnect；
3. 向 usbfs 报告全部逻辑 device id；
4. usbfs 将 record 标为 absent，并对仍打开的 live `Device` 执行 disconnect；
5. 旧 session 和 handle 永久返回 `Disconnected`。

断连不是“本轮 probe 没有看到设备”。probe 是增量事件流，只有明确的 port disconnect 才移除拓扑实体。

## usbfs

usbfs 的 claimed-interface map 只保存 `InterfaceSession`，endpoint 始终通过当前 session 获取。

`USBDEVFS_DISCARDURB` 保持 Linux 用户态语义：

1. 立即从 submitted lookup 中取出 URB；
2. 请求底层 cancel；
3. 立即排入一个 `ENOENT` reap result；
4. 将已标记 `discarded` 的 transfer 放回 terminal-reclaim 队列；
5. HCD terminal completion 到达后只回收底层资源，不再发布第二个 reap result。

因此用户只观察一次 `REAPURB`，但立即报告取消不会缩短 DMA 的硬件所有权期限。

## 测试与证据

最低层确定性回归包括：

| 层次 | 回归 | 证明的错误边界 |
|---|---|---|
| shared core | alternate commit/failure、wrong-device session、disconnect | 旧 handle revoke/rollback/disconnect 与 session ownership |
| xHCI | `pending_ring_registration_does_not_replace_active_completion_route` | pending ring 不能覆盖 active completion route |
| xHCI | `stopped_iso_request_publishes_cancelled_exactly_once` | Stop 后取消只完成一次 |
| EHCI | `async_qh_waits_for_two_iaa_cycles_before_reclaim` | 第一次 IAA 后不能回收 QH |
| EHCI | `periodic_qh_waits_nine_microframes_before_reclaim` | periodic 安全窗口前不能回收 |
| EHCI | `async_schedule_accepts_multiple_endpoint_queue_heads` | async schedule 不再限制单 QH |
| DWC2 | `cancelled_endpoint_waits_for_real_channel_halt_before_reclaiming` | 真实 `CHHLTD` 前不能释放 DMA/channel |
| DWC2 | `opposite_direction_endpoints_do_not_share_a_host_channel` | 同 endpoint number 的 IN/OUT 不串扰 |
| DWC2 | `disconnect_completes_active_request_without_more_channel_writes` | disconnect 后不再写 channel 寄存器 |
| DWC2 | `acquired_channel_cannot_start_after_disconnect` | 已取得 lease 的任务也不能越过 disconnect 重启 DMA |
| usbfs | `discard_reports_enoent_once_then_reclaims_terminal_transfer` | DISCARD 立即报告且真实终态不重复 reap |

Starry UVC 板卡应用在同一个 libuvc device handle 上执行三轮 streaming start/stop。每轮必须收到非零 frame/byte。libuvc 的 `uvc_stop_streaming()` 会对全部异步 libusb transfer 发出 cancel，并等待每个 terminal callback 回收 transfer；在 Starry usbfs 后端，这一返回边界对应 `DISCARDURB` 后的终态 `REAPURB` 已完成。应用只在该同步边界返回后记录 `async_iso_cancel_completion=ok`，再暂停并重启。成功标记为：

```text
uvc-fps: lifecycle PASS rounds=3 active_alt=streaming->0->streaming pause_resume=ok async_iso_cancel_completion=ok
```

最终交付还要求：

- `cargo fmt`；
- `crab-usb` 单测与 kmod/umod 构建；
- `cargo xtask clippy --package crab-usb`、`crab-uvc`、`usb-keyboard`、`starry-kernel`；
- 现有 xHCI、EHCI、DWC2 QEMU/平台用例；
- rebase 最新 `origin/dev` 后 OrangePi-5-Plus 原生 Starry robot 和 Axvisor `run_host` 各连续三轮；
- 完整 push CI 终态全绿。
