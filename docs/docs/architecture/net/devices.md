---
sidebar_position: 5
sidebar_label: "多设备实现"
---

# 多设备实现

`ax-net` 采用 **single smoltcp Interface + Router as Device + queue-level poll
runtime**。smoltcp 只看到一个 IP medium `Router`；每个物理设备在启动时消费一次，
拆成 queue/control/IRQ parts。独立 IRQ affinity domain 可以在不同 CPU 上并行处理
DMA，协议状态仍由一个固定 CPU executor 串行推进。

完整状态机、Linux v7.1 对照和失败回滚见[队列级 NAPI 运行时](queue-napi-runtime.md)。

## 1. 源码与所有权

| 源码 | 所有权 |
| --- | --- |
| `drivers/interface/rdif-eth` | portable `NetDeviceParts`、queue/IRQ/control trait、move-only `DmaBuffer` |
| `drivers/net/rd-net` | 校验 parts、建立 DMA pool、生成 `PreparedNetDevice` |
| `queue_runtime/` | affinity domain、fixed-CPU executor、SPSC、budget、backpressure、IRQ lease |
| `poll_runtime.rs` | protocol `requested/completed` generation |
| `device/ethernet.rs` | Ethernet frame、ARP、neighbor、pending frame |
| `router.rs` | 多接口 route dispatch、loopback、smoltcp `Device` |
| `service.rs` | 唯一 smoltcp `Interface`、DHCP、控制面提交 |

运行期不保存可再次拆分的完整 driver handle。IRQ callback 不持有 RX/TX queue；
protocol executor 不借用硬件 queue；调用者也不能直接借用 Wi-Fi SDIO control。

## 2. Consumable device parts

portable driver 实现唯一入口：

```rust
pub trait NetDevice: DriverGeneric + Send + 'static {
    fn into_parts(self: Box<Self>) -> Result<NetDeviceParts, NetError>;
}

pub struct NetDeviceParts {
    pub info: NetDeviceInfo,
    pub control: Box<dyn NetControlEndpoint>,
    pub wifi_control: Option<Box<dyn WifiControl>>,
    pub poll_groups: Vec<NetPollGroupParts>,
}
```

`NetPollGroupParts` 表示一个不可拆分的 IRQ mask/rearm 域：

- typed `NetPollGroupId`；
- 一个或多个 `NetQueuePairParts`；
- owner-task 使用的 `NetPollIrqControl`；
- 可选的 move-only `NetOwnerStartup`；
- 一个或多个带 `NetIrqSourceId` 的 move-only `NetHardIrqEndpoint`。

只有具有独立 physical source 和独立 rearm 的硬件 queue 才能形成不同 group。当前
生产 backend 都发布 queue-0 group；接口已经允许多 group，但不虚构 virtio/fxmac
硬件多队列支持。

## 3. DMA token 与 queue contract

`DmaBuffer` 不实现 `Clone`/`Copy`。它从 pool 分配后只能处于以下一个位置：

```text
RX pool/spares -> hardware RX -> RxCompletion
        -> submit replacement -> RX-ready SPSC -> ProtocolRxFrame
        -> EthernetDevice -> Router -> smoltcp RxToken::consume
        -> RxRecycler -> recycle SPSC/overflow -> queue-local spares

TX pool -> TX-free SPSC -> protocol fills DMA frame -> TX-ready SPSC
        -> submit_with_options -> batch flush -> hardware TX
        -> completion reclaim -> TX-free SPSC
```

`ITxQueue::submit()` 和 `IRxQueue::submit()/recycle()` 失败时，typed
`SubmitError` 必须归还原 token。runtime 对 `Retry`/`LinkDown` 保留 token；若本轮没有
RX reclaim 等可观察进展，则完成本轮 poll、rearm IRQ 并等待未来硬件或 task 事件，不能
在 IRQ 保持关闭时立即自调度。只有 reclaim 已释放 descriptor 时，RX refill 才立即重试。
其它错误可以归还 free ring 或使 group 失败，但不能丢失 DMA ownership。

`TxSubmitOptions` 携带逐包 checksum 和通知方式。`TxNotify::Deferred` 允许驱动延迟
写 doorbell；queue executor 在完成发送批次以及提交提前返回前调用 `flush()`，已发布
的 descriptor 不会因为 RX backpressure 或下一次 submit 返回 `Retry` 而滞留。
默认 queue 只支持软件 checksum；不支持的 offload 请求必须返回原 DMA token。

RTL8125 在 descriptor 发布与 doorbell 之间使用 Release 顺序，支持满足其格式约束的
IPv4/IPv6 TCP/UDP checksum-v2；短 padding frame 走软件 checksum。Router 只公布所有
物理出口共同支持的能力，loopback 补齐 offload 路径留下的 checksum。driver 支持 IPv6
checksum 不代表物理 Ethernet IPv6 协议已完整接入。

非一致 DMA 平台的 CPU/device sync 在 `DmaBuffer` 的 read/write 与 driver submit/reclaim
边界完成。跨 CPU 只 move token，不共享可变 payload reference。

## 4. IRQ affinity domain

builder 先把每个 endpoint source ID 解析为 physical `IrqId`。共享同一 IRQ 的 group
通过并查集合并为一个 affinity domain，再按稳定顺序分配在线 CPU。

硬性不变量：

```text
IRQ callback CPU == group poll CPU == owner_cpu
```

注册顺序：

1. 构造全部 group state、DMA pool、SPSC 和 per-CPU executor。
2. executor 设置 `AxCpuMask::one_shot(owner_cpu)`，yield 后回报 affinity-ready。
3. registrar 以 `NonReentrant + AutoEnable::No + Fixed(owner_cpu)` 注册 action。
4. owner worker 执行 one-shot startup；AIC 固件/FDRV 初始化只允许发生在这里。
5. owner worker initial refill 并执行第一次 `rearm_and_check()`。
6. enable 全部 registration；startup Wi-Fi transaction 成功后才发布 service。

shared action affinity 冲突、fixed route 不支持、worker pin 失败或 registration 返回的
CPU 不一致都会使整个物理网络初始化失败。没有 `Any` affinity、远程 IRQ continuation、
无 IRQ 路径或周期 poll fallback。

## 5. Hard IRQ 与 group 状态机

hard endpoint 只能做 bounded mask/ack/status snapshot，返回：

- `Spurious`：共享 IRQ 不属于本 endpoint；
- `Schedule(snapshot)`：有真实 queue work；
- `ProbeDeferred`：例如 virtio transport gate 正被 task owner 使用，需要同 CPU 稍后探测。

IRQ 不能分配、访问 DMA payload、调用 smoltcp、调用任意 waker或在 transport gate 上
自旋。

每个 group 的原子状态为：

```text
IDLE -> SCHEDULED -> POLLING -> IDLE
                    |    ^
                    + MISSED
any state -> DISABLED
```

首次 publish 才通知 executor；scheduled/polling 中的新事件只置 `MISSED`。poll owner
结束时用 CAS：观察到 `MISSED` 则回到 `SCHEDULED`；否则先变 `IDLE`，再执行硬件
`rearm_and_check()`。rearm 窗口发现 pending 会立即重新 schedule。

`DISABLED` 是吸收态。poll completion 使用 CAS，不能把 concurrent disable 重新写成
scheduled。

## 6. Budget 与 backpressure

一个 group poll 按顺序处理：

1. TX completion；
2. TX submission 与批次 `flush()`；
3. 已消费 RX token 回收到 queue-local spare cache；
4. RX reclaim、replacement refill 与完成项发布。

各类使用 64 项子预算，每 CPU executor round 使用 256 项总预算。任一子预算用尽、CPU round
用尽、`MISSED` 或硬件仍有工作时，group 保持 IRQ 关闭并重新排队。

四条 SPSC ring 都预分配且有界：

- RX-ready 满：queue owner 保留 `pending_rx`；
- replacement submit retry：在有界 `pending_rx_refill` 中同时保留 completion 和 replacement；继续 reclaim，避免软件队列因 completion ring 满而无法推进；
- TX-free 满：保留 `pending_tx_free`；
- TX submit retry：保留 `pending_tx`。

blocked group 不 busy-wait，也不等待 timer。protocol owner 消费/释放空间时调用
group-local task schedule；只有目标 group 被精准激活。

## 7. Protocol frame port 与 Router

`QueueFramePort` 位于 protocol owner 一侧。`receive_owned()` 从 RX-ready ring 取
completion，返回带回收器的 `ProtocolRxFrame`。它保留 token 到 smoltcp 消费结束；
ARP 和未知二层帧在原 token 回收前处理。回收 ring 满时由 `RxRecycler` 暂存 token，
queue owner 再将其转入 spare cache，不提前释放 DMA mapping。

TX 优先从 spare/free ring 取 token，在 DMA buffer 中写 Ethernet header、IP packet
及 `ETH_ZLEN` padding。`NoQueue` 在忙时返回 `Again`；`Fifo` 只在积压时保留有界
inline frame 和原提交选项，按序重试。各 RX 接口都会推进待发送 FIFO，避免仅使用
owned RX 的生产路径遗漏发送重试。Router peek 待发 packet，仅在接受提交后 dequeue。
非 DMA 端口沿用 `ProtocolEthernetFrame` 的兼容复制接口。

`EthernetDevice` 在 frame port 上完成：

- Ethernet header 解析/封装；
- IPv4 与 ARP 过滤；
- neighbor cache 与 300 秒 TTL；
- ARP retry 与有界 pending packet；
- L2 byte/packet/error/drop 统计。

`Router` 对 smoltcp 暴露 `Medium::Ip`。RX packet 携带 ingress `InterfaceId`，用于
DHCP/TCP SYN snoop 与 cmsg metadata；TX 根据目标、source address、最长前缀和 metric
选择 frame port。loopback 直接注入 protocol RX buffer，不经过物理 queue。

## 8. 唯一 protocol executor

queue executor 只在 frame/token 到达时调用 `request_poll()`。`ProtocolPollRuntime`
增加 requested generation 并唤醒固定 CPU `net-protocol` task；只有该任务可以持有
`Service`/`SocketSet` 并调用 smoltcp poll。

同步 egress flush 同样只 request 并等待自己的 completed generation，调用线程不会
成为第二个 poll owner。request 与 completion 竞争时，worker 清除 scheduled 后再次
比较 generation，确保新请求至少再运行一轮。

## 9. Wi-Fi/SDIO specialization

AIC probe 只识别 chip variant 并把 SDHCI controller source move 到 hard endpoint，
不执行固件/FDRV I/O。owner startup 在固定 CPU 创建 bus；top half 只识别并 mask
CARD_INT，owner CPU 负责 FIFO drain、TX、firmware command completion、AP event 与
rearm。SDHCI task rearm 在 unmask 后立即读 CARD_INT status，若 level 已挂起则重新
mask 并返回 pending。

Wi-Fi startup/reconfigure 使用有界 owner control queue。executor quiesce group 后才
执行 SDIO/MMIO control，完成后 rearm，再让 protocol owner更新 STA DHCP 或 SoftAP
static/DHCP-server state。

当前 variant policy：

- AIC8800D80：V3 queue encoding、software IRQ bit 清源、Function 1 command/data
  FIFO；
- AIC8800DC：V1 queue/status，Function 1 data FIFO、Function 2 firmware mailbox；
- AIC8801、AIC8800D80X2、AIC8800DW 和未知变体：缺少经验证的 profile 或固件，
  probe fail-closed。

任何 variant 都没有 OOB callback、独立 RX/TX task 或 kicker。

## 10. Driver group mapping

| Driver | 当前 group | hard endpoint | task owner/rearm |
| --- | --- | --- | --- |
| virtio-net | queue 0 RX/TX | 仅真实 `QUEUE_INTERRUPT`；gate 竞争为 `ProbeDeferred` | callback disable + 非消费 `peek_used` double-check；reclaim 才消费 completion |
| E1000 | queue 0 RX/TX | ICR snapshot/mask | descriptor drain 后恢复 IMS 并复查 |
| RTL8125 | queue 0 RX/TX | status gate/ack/mask | deferred refill、completion、pending status 复查 |
| FXMAC | queue 0 | status snapshot；gate 竞争 deferred | owner drain/rearm |
| Loongson GMAC | queue 0 | DMA status snapshot/mask | RX restart、ACK/rearm |
| AIC/SDHCI | queue 0 | nested CARD_INT mask/status | FIFO/command/TX + controller/chip rearm/shutdown |

## 11. Lifecycle 与回滚

builder 的失败路径按发布顺序反向执行：拒绝新 Wi-Fi request、disable/synchronize IRQ
lease。同步成功时通知 executor stop，由 owner CPU quiesce 并执行 driver `shutdown()`，
然后 join；同步失败时发送 `QUARANTINE`，保留完整 callback、executor、control 与
backing graph，不再并发触碰驱动硬件。
只有 shutdown 证明硬件已不能访问 backing 时才 drop queue token 与 device parts；无法
证明时隔离完整 executor ownership graph。`NetworkQueueRuntime::Drop` 保持同一顺序。
service 只有在 builder 完整成功后进入全局 `OnceLock`，因此不存在半发布接口。

物理设备要求完整 IRQ、fixed affinity、mask/rearm 和 worker pin。无物理 NIC 时可以
启动 loopback-only；发现物理设备但能力不完整时必须失败，不能把 loopback 成功当作
物理网络成功。

## 12. 可观测不变量

每个 group 暴露测试态 `NetQueueStats`：IRQ、schedule、MISSED、poll batch、budget
exhaustion、spurious、probe deferred、rearm race、owner CPU、last IRQ CPU、last poll
CPU 与 remote wake。

验收断言：

- `last_irq_cpu == last_poll_cpu == owner_cpu`；
- `irq_to_poll_remote_wake == 0`；
- 一个 IRQ 不改变无关 group 的 schedule/wake；
- idle queue executor 没有周期唤醒；
- burst 在 IRQ mask 期间按预算合并，drain/rearm 后才重开；
- stop 后 group 拒绝新 schedule，DMA token 不重复、不泄漏。

CPU hotplug、运行时新增/删除 NIC、RPS/RFS/GRO、busy-poll、协议状态分片和真实
virtio/fxmac 多硬件队列不在当前范围。
