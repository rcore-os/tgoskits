# AxVisor virtio-net 多 Guest 虚拟交换机设计

## 1. 背景与目标

当前 AxVisor virtio-net raw uplink 已完成单 guest 数据面：

```text
ArceOS guest
  -> AxVisor emulated virtio-net
  -> RawUplinkBackend
  -> AxVisor host virtio-net frontend
  -> QEMU TAP
  -> Linux bridge / DHCP / DNS / NAT
  -> Internet
```

该实现只有一个 guest 时可以正常完成 DHCP、DNS、TCP、HTTP 和公网访问，但不能通过简单
复制 VM worker 的方式支持多个 guest。当前每个 VM worker 都可能调用
`HostUplink::progress_rx` 消费同一个 host RX queue；两个 worker 会竞争帧，先运行的 worker
可能把属于另一个 guest 的帧送入自己的 backend。共享 `wake_one` 也无法保证唤醒拥有目标
端口的 worker。

本设计把 host uplink 从“一个 guest 的转发附件”提升为 AxVisor 内部共享的二层虚拟交换机：

```text
ArceOS VM1 virtio-net --+
                        |
ArceOS VM2 virtio-net --+--> AxVisor Virtual Ethernet Switch
                        |              |
ArceOS VMn virtio-net --+       one host virtio-net
                                       |
                                    QEMU TAP
                                       |
                             Linux bridge / NAT
                                       |
                                    Internet
```

首版目标：

1. 多个 ArceOS VM 共享一个 AxVisor host virtio-net 和一个 Linux TAP；
2. 每个 VM 使用唯一 guest MAC，并通过同一 DHCP server 获得独立地址；
3. 外部单播按目标 MAC 精确投递，广播/组播复制到所有活动端口；
4. VM 间流量在 AxVisor 内部直接交换，不依赖 Linux TAP hairpin；
5. host virtio-net queue 只有一个运行时 owner，不允许 VM worker 竞争消费；
6. 每个端口拥有独立的容量、背压、统计、唤醒和 generation 生命周期；
7. 一个 VM 的拥塞、停止或 reset 不影响其他 VM；
8. 保持 `axvirtio-net` 设备模型和 `rd-net` host driver 与多 VM 策略解耦。

首版不实现 VLAN、STP、动态 trunk、多 host uplink 聚合、virtio-net 多队列、零拷贝、端口
镜像或可配置 ACL。这些能力必须在核心单 uplink、多端口语义稳定后扩展。

## 2. 设计原则

### 2.1 单一 host queue owner

host TX/RX queue 和 IRQ handler 属于一个 `HostUplinkRuntime`。只有它的 uplink worker 可以：

- reclaim host TX completion；
- 从各端口选择待发送帧并提交 host TX；
- reclaim host RX buffer；
- 把 host RX frame 交给交换机分发。

per-VM worker 不再直接访问 `rd_net::TxQueue` 或 `rd_net::RxQueue`。这消除 queue 竞争，
也让 IRQ、DMA buffer 和 queue completion 的所有权保持单一。

### 2.2 静态端口身份优先于动态学习

AxVisor 已从 VM 配置知道每个 emulated virtio-net 的 guest MAC，因此首版不需要传统 bridge
的动态 FDB 学习。端口注册时建立：

```text
guest MAC -> SwitchPortId -> active generation endpoint
```

静态注册能直接拒绝重复 MAC，并避免恶意或错误 guest 通过伪造源 MAC 污染学习表。后续若
支持 guest 修改 MAC，应增加显式控制面和策略，不能无条件学习任意源地址。

### 2.3 数据面只做二层交换

交换机不解析或修改 ARP、IPv4、IPv6、TCP、UDP、DHCP 和 DNS 内容，不做 NAT，不重写
MAC，也不计算 L3/L4 checksum。外层 Linux bridge 提供网关、DHCP/DNS 和 NAT。

交换机只读取 Ethernet 目标/源 MAC，并执行长度、端口身份和队列容量检查。

## 3. 模块与所有权

建议继续把实现放在 `os/axvisor/src/virtio_net/`，按以下职责拆分：

```text
virtio_net/
  switch.rs       VirtualSwitch、端口表、二层转发策略、统计
  raw_uplink.rs   host NIC claim、IRQ、DMA queues、uplink worker
  backend.rs      每个 guest 端口的 bounded ingress/egress endpoint
  worker.rs       guest RX delivery worker 和 VM 生命周期
  factory.rs      从 VM 配置创建端口注册、device、adapter
  adapter.rs      emulated device 与端口注册的 RAII ownership
  config.rs       guest MAC、uplink selector 和参数校验
```

不把 `VirtualSwitch` 放入 `axvirtio-net`。后者是可复用的 virtio-net device model，不应知道
VM、host NIC、TAP 或交换策略。

### 3.1 核心类型

建议的领域类型如下，具体字段可按实现调整：

```rust
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SwitchPortId {
    vm_id: usize,
    generation: usize,
    device_index: u16,
}

pub struct SwitchPortEndpoint {
    id: SwitchPortId,
    guest_mac: [u8; 6],
    backend: RawUplinkBackend,
    active: AtomicBool,
}

pub struct VirtualSwitch {
    registry: spin::Mutex<SwitchRegistry>,
    stats: SwitchStats,
}

pub struct HostUplinkRuntime {
    host_mac: [u8; 6],
    queues: spin::Mutex<UplinkQueues>,
    switch: Arc<VirtualSwitch>,
    work: Arc<UplinkWorkSignal>,
    task: WorkerTask,
    irq_registration: ax_runtime::irq::Registration,
}

pub struct SwitchPortRegistration {
    switch: Arc<VirtualSwitch>,
    endpoint: Arc<SwitchPortEndpoint>,
}
```

`SwitchPortRegistration` 是 RAII capability。adapter 持有 registration，worker 只持有端口
endpoint。registration drop 或显式 `deactivate` 时从两个索引移除端口；旧 generation 的
endpoint 即使仍被临时引用，也因 `active == false` 拒绝继续收发。

端口表不能用裸 `usize` 表示身份。`vm_id + generation + device_index` 可以区分同一 VM
reset 前后的设备，也为未来每 VM 多 NIC 留出空间。

### 3.2 全局 uplink registry

保留按 host NIC MAC 索引的 registry：

```text
host uplink MAC -> Arc<HostUplinkRuntime>
```

同一个 host MAC 只 claim 一次 `PlatformNetDevice`、创建一次 host queue、注册一次 IRQ、
启动一次 uplink worker。多个 VM 通过 `claim_or_get` 取得同一个 runtime，再注册独立端口。

uplink worker closure 只能持有 runtime 的 `Weak` 或拆出的 worker core，不能与
`HostUplinkRuntime::task` 形成强 `Arc` 引用环。

registry 不按 guest MAC 保存 uplink。guest MAC 只属于对应 `VirtualSwitch` 的端口表。

## 4. 配置与校验

现有 raw-uplink 配置形态可以兼容使用：

```text
[guest MAC 6 bytes, mode=1, host uplink MAC 6 bytes]
```

例如两个 VM：

```toml
# VM1
[0x52, 0x54, 0x00, 0x12, 0x34, 0x56, 1,
 0x52, 0x54, 0x00, 0xaa, 0xbb, 0x01]

# VM2
[0x52, 0x54, 0x00, 0x12, 0x34, 0x57, 1,
 0x52, 0x54, 0x00, 0xaa, 0xbb, 0x01]
```

新增校验：

- guest MAC 必须是单播地址，不能为全零或广播地址；
- 同一个 switch 上活动端口的 guest MAC 必须唯一；
- host uplink MAC 必须唯一匹配一个 `PlatformNetDevice`；
- guest MAC 不得等于 host uplink MAC；
- frame 长度必须在 Ethernet header 下限和端口 MTU/buffer 上限内；
- 重复 `SwitchPortId` 或重复 guest MAC 返回结构化配置错误，不覆盖已有端口；
- 不依赖设备枚举顺序或“第一块网卡”。

首版不把 queue capacity、MTU、anti-spoof 等继续编码为 `cfg_list` 中的位置参数。需要开放
这些配置时，应在 VM 配置模型中增加 typed struct，而不是增长布尔值和裸整数列表。

## 5. 数据面

### 5.1 Guest TX

完整路径：

```text
guest virtqueue TX kick
  -> axvirtio-net validates descriptor chain
  -> RawUplinkBackend::transmit
  -> port egress bounded queue
  -> signal uplink worker
  -> VirtualSwitch classifies destination
  -> local delivery and/or host TX
```

交换决策：

| 目标 MAC | 本地端口投递 | host uplink |
|---|---:|---:|
| 本地已注册单播 | 仅目标端口 | 否 |
| 广播 | 除源端口外所有活动端口 | 是 |
| 组播 | 除源端口外所有活动端口 | 是 |
| 未知单播 | 否 | 是 |

发送前验证 Ethernet 源 MAC 等于端口注册 MAC。首版采用 anti-spoof 策略：不匹配时丢弃并
增加 `source_mac_violation`，不自动学习，也不把伪造帧发到 uplink。

本地单播不经过 host TX，因此两个 VM 的通信不依赖 TAP hairpin。广播/组播既复制给本地
端口，也发送到 uplink，使 DHCP、ARP 和 IPv6 邻居发现仍能到达 Linux bridge。

### 5.2 Host RX

完整路径：

```text
QEMU TAP
  -> host virtio-net RX used ring
  -> host IRQ publishes RX readiness
  -> uplink worker reclaims frame and refills buffer
  -> VirtualSwitch classifies destination
  -> target port ingress bounded queue
  -> target guest delivery worker
  -> axvirtio-net::receive_frame
  -> optional guest IRQ pulse
```

分发规则：

| 目标 MAC | 分发行为 |
|---|---|
| 本地已注册单播 | 仅投递对应活动端口 |
| 广播 | 复制给所有活动端口 |
| 组播 | 首版复制给所有活动端口 |
| 未知单播 | 丢弃并计数 |

从外部进入的未知单播默认不 flood，避免把不属于任何 guest 的 host traffic 暴露给所有 VM。
将来如需 promiscuous port，应作为显式 capability 配置。

交换机在持有端口表锁时只创建目标 endpoint 的 `Arc` 快照；释放端口表锁后才调用
`push_ingress`。不能在端口表锁内复制大帧、唤醒 worker 或调用 guest device。

### 5.3 公平性与预算

uplink worker 每轮使用有界预算：

- host RX 总预算，例如 64 帧；
- host TX 总预算，例如 64 帧；
- 每端口 TX quantum，例如 8 帧；
- 从上次停止的端口继续 round-robin，不能每轮都从最小 `PortId` 开始；
- `NetError::Retry` 时保留该端口帧并转向其他端口，等待 TX completion 再重试；
- 单端口持续发包不能阻止其他端口 DHCP、ARP 或 TCP ACK。

复制广播/组播时，每个目标端口独立执行容量检查。一个端口 ingress 满只丢弃该端口的
副本，不阻止其他端口和 uplink 收到帧。

## 6. Worker 与唤醒协议

### 6.1 Uplink worker

每个 host uplink 只有一个 uplink worker，负责 host queues 和交换决策。工作来源包括：

- host IRQ 发布 RX/TX readiness；
- 任一端口 egress 从空变为非空；
- 端口注册或恢复后存在待处理工作；
- shutdown/cancel。

不要继续让所有 backend 共享一个 `wake_one`。建议使用不会丢边沿的 work epoch：

```rust
pub struct UplinkWorkSignal {
    epoch: AtomicU64,
    wake: WorkerWaitQueue,
}
```

producer 按以下顺序发布：

```text
写入 queue/event state
  -> epoch.fetch_add(1, Release)
  -> wake_one uplink worker
```

worker 保存 `observed_epoch`，先排空有界工作，再以
`epoch.load(Acquire) != observed_epoch || cancel` 为条件等待。进入睡眠前必须重新读取 epoch，
避免“检查为空”和“真正睡眠”之间丢失通知。

IRQ top half 只 ACK host interrupt、发布 queue readiness/epoch 并唤醒 uplink worker。它不取
端口表锁、host queue 锁或 guest device 锁，也不复制 frame。

### 6.2 Guest delivery worker

每个活动端口保留一个 guest delivery worker。它只负责：

- 消费该端口 ingress queue；
- 调用对应 generation 的 `receive_frame`；
- 根据 `RxOutcome::Delivered { notify }` 决定是否 pulse guest IRQ；
- `NoGuestBuffer` 时把帧放回本端口队首，等待 guest RX kick；
- cancel 后退出并由 VM manager join。

该 worker 不访问 host queues，也不遍历其他端口。

端口的 ingress 和 egress 使用独立 readiness 状态，不能用一个全局布尔值同时代表 host IRQ、
本端口 TX 和本端口 RX。

## 7. 锁与并发规则

运行时涉及以下状态：

- host TX/RX queue owner；
- switch port registry；
- 每端口 ingress queue；
- 每端口 egress queue；
- guest virtqueue/device；
- VM lifecycle registry。

约束：

1. IRQ context 不取得上述 mutex；
2. host queue 锁内不调用 switch 分发、端口 wake 或 guest device；
3. port registry 锁内不调用 backend queue 或 worker wake；
4. backend queue 锁内不调用 host queue、guest device 或 VM manager；
5. guest virtqueue lock 内的 `NetworkBackend::transmit` 只能做有界 enqueue 和 signal；
6. VM stop/reset 不在持有 worker registry 锁时 join task；
7. 所有跨上下文状态使用 Acquire/Release；仅独立统计计数使用 Relaxed；
8. 不依赖嵌套锁顺序，跨边界操作使用 `Arc` 快照后释放原锁。

推荐把端口表的两个索引放在同一个小型 `SwitchRegistry` mutex 中，避免两个锁之间产生
一致性窗口：

```rust
struct SwitchRegistry {
    by_id: BTreeMap<SwitchPortId, Arc<SwitchPortEndpoint>>,
    by_mac: BTreeMap<[u8; 6], SwitchPortId>,
    tx_cursor: Option<SwitchPortId>,
}
```

## 8. 生命周期

### 8.1 首次启动

顺序：

```text
claim_or_get host uplink
  -> ensure uplink IRQ/queues/worker exist
  -> validate unique SwitchPortId and guest MAC
  -> create inactive port endpoint
  -> construct emulated virtio-net device and adapter
  -> activate SwitchPortRegistration
  -> start guest delivery worker
  -> start VM
```

构造中途失败时，RAII registration 必须逆序注销端口，不能在 MAC 表留下半初始化 endpoint。

### 8.2 Stop/remove

顺序：

```text
mark port inactive
  -> remove it from by_id/by_mac
  -> reject new egress enqueue
  -> cancel and wake guest delivery worker
  -> join worker
  -> clear ingress/egress queues
  -> stop/drop guest device generation
```

先从分发表移除端口，保证 stop 开始后 host RX 不会再向该 VM 加帧。join 不能在 switch
registry 锁或 VM manager 锁内执行。

### 8.3 Reset 与 stopped-start

reset 前注销旧 `SwitchPortId`。新 prepare generation 创建新 ID 和新 endpoint；旧 endpoint
即使仍被某个临时 `Arc` 引用，也因 inactive/generation mismatch 丢弃工作。

`VirtioNetPrepareProfile::build(generation)` 必须把 `generation` 传给
`VirtioNetDeviceFactory`；factory 不得在内部重新读取或猜测当前 generation。

guest MAC 可以在旧 generation 完全注销后由新 generation 复用。注册逻辑不能静默替换
仍活动的同 MAC 端口，否则会掩盖两个 VM 的配置冲突。

### 8.4 Host uplink 生命周期

host uplink runtime 首版可在 AxVisor 生命周期内常驻。最后一个端口注销后 worker 继续休眠，
不释放 host NIC。这样 reset 不需要反复解绑 IRQ 和重建 DMA ring，也避免 QEMU
`RING_EVENT_IDX` 启动期中断窗口再次出现。

若未来需要动态释放 uplink，应增加明确的“禁止新端口 -> 停 IRQ -> cancel/join uplink
worker -> drain/drop queues -> unregister IRQ -> drop NIC”状态机，不能只依赖 `Arc` drop。

## 9. 错误、背压与统计

### 9.1 Port counters

每端口至少记录：

- `tx_packets` / `tx_bytes`；
- `rx_packets` / `rx_bytes`；
- `local_tx_packets`；
- `uplink_tx_packets`；
- `ingress_queue_full` / `egress_queue_full`；
- `no_guest_buffer`；
- `oversize_drop` / `undersize_drop`；
- `source_mac_violation`；
- `inactive_generation_drop`。

### 9.2 Switch/uplink counters

至少记录：

- `host_rx_packets` / `host_tx_packets`；
- `host_tx_retry` / `host_tx_error`；
- `unknown_unicast_drop`；
- `broadcast_copies` / `multicast_copies`；
- `local_unicast_forwarded`；
- `irq_events`；
- `worker_wakes`；
- `duplicate_mac_rejected`。

日志只记录端口注册/注销、uplink 状态变化、配置错误和聚合统计，不逐包输出。队列满等高频
错误使用计数和受限日志，不能刷屏。

## 10. 实施步骤

### 阶段 1：提取纯交换核心

- 新增 `switch.rs` 和 typed `SwitchPortId`；
- 实现端口注册、重复 MAC 拒绝、注销和 generation 隔离；
- 实现单播、广播、组播和未知单播决策；
- 使用 fake endpoints 添加确定性测试，不接触 QEMU 或 host driver。

退出条件：端口表和 frame 分类测试全部通过，交换核心不依赖 AxVM、IRQ、DMA 或 QEMU。

### 阶段 2：改造 backend 为端口 endpoint

- 将 raw backend 的 ingress/egress readiness 分离；
- `transmit` 只入本端口 egress；
- inbound delivery 只唤醒本端口 guest worker；
- 增加 active/generation 检查和 bounded queue counters。

退出条件：两个 fake port 同时入队不会互相消费或丢失通知。

### 阶段 3：建立单一 uplink worker

- 从 per-VM worker 移除 host `TxQueue`/`RxQueue` 访问；
- host IRQ 只唤醒 uplink worker；
- uplink worker 执行 host RX、round-robin TX 和 local switching；
- 验证快速 completion、IRQ/task 竞争和 TX `Retry` 不丢事件。

退出条件：一个 fake host port 与两个 fake guest port 可稳定双向交换，单端口 flood 不饿死
另一端口。

### 阶段 4：接入 VM generation 生命周期

- factory 创建 RAII port registration；
- adapter/worker 分离 registration 与 endpoint ownership；
- stop/reset/remove 先 deactivate，再 cancel/wake/join；
- 重复 MAC、prepare 失败和 stale generation 有确定性测试。

退出条件：反复 reset 一个 VM 时另一个 VM 流量不中断，registry 不保留旧 generation。

### 阶段 5：双 ArceOS QEMU 验收

- 增加第二份 VM 配置，使用不同 VM id、guest MAC 和必要的独立 console/log 通道；
- 两个 VM 共享相同 host uplink MAC；
- Linux 只创建一个 TAP 和一个 bridge；
- dnsmasq 给两个 MAC 分配不同地址；
- 并发运行 DHCP、DNS、TCP 和 HTTP；
- 增加 VM1 到 VM2 的 ARP/UDP 或 ICMP 本地交换验证；
- 分别 stop/reset VM1，确认 VM2 公网连接持续可用。

## 11. 验证矩阵

### 11.1 交换核心单测

- 两个端口注册成功；
- 重复 `PortId` 和重复 MAC 被拒绝；
- known unicast 只选中目标端口；
- guest-to-guest known unicast 不选择 uplink；
- broadcast/multicast 复制给其他活动端口并选择 uplink；
- host RX unknown unicast 丢弃；
- source MAC spoof 被拒绝；
- 端口注销后不再成为分发目标；
- stale generation 不能注销或覆盖新 generation；
- 某一 ingress 满时其他副本仍成功。

### 11.2 Worker/并发测试

- producer 在 worker 睡眠窗口发布事件不会丢 wake；
- host IRQ 与端口 TX 同时到达时两类工作都被处理；
- round-robin 在持续负载下服务所有端口；
- host TX `Retry` 不改变同一端口的 frame 顺序；
- cancel 能唤醒并 join uplink/guest worker；
- stop/reset 与 host RX 并发时不会投递到 inactive endpoint；
- callback 不在 registry/queue 锁内重入。

### 11.3 QEMU 验收标记

建议为两个 guest 输出带 VM 身份的结果：

```text
VM1_DHCP_PASS 10.88.0.75
VM2_DHCP_PASS 10.88.0.76
VM1_DNS_PASS www.baidu.com <address>
VM2_DNS_PASS www.baidu.com <address>
VM1_HTTP_PASS HTTP/1.1 200 OK <body-bytes>
VM2_HTTP_PASS HTTP/1.1 200 OK <body-bytes>
VM_LOCAL_SWITCH_PASS <vm1-ip> <vm2-ip>
VM2_SURVIVES_VM1_RESET_PASS
VM1_POST_RESET_HTTP_PASS
```

公网站点只作为补充验证。CI 使用本机可控 DHCP/DNS/HTTP 服务，避免第三方响应变化导致
不稳定。百度 HTTP/HTTPS 测试应与确定性本机 HTTP 测试分开。

## 12. 完成定义

只有同时满足以下条件，才能声明共享 uplink 的多 guest 网络完成：

1. host virtio-net RX/TX queue 只有一个 uplink worker owner；
2. 两个 VM 使用唯一 MAC，通过一个 TAP 同时取得不同 DHCP 地址；
3. 两个 VM 同时通过 DNS、TCP 和本机 HTTP；
4. VM 间已知单播在 AxVisor 内转发，抓包证明不经过 TAP；
5. 广播/组播正确复制，未知入站单播不泄漏到其他 VM；
6. 单 VM 拥塞不会阻塞另一 VM，TX 调度具有有界公平性；
7. stop/reset/remove 后无旧端口、旧 generation frame 或 worker；
8. reset 一个 VM 不打断另一 VM 的持续网络连接；
9. 单 guest TAP、QEMU user-net 和 deterministic echo 回归全部保持通过；
10. 格式化、相关单测、targeted clippy、AxVisor 构建和宿主资源清理全部通过。
