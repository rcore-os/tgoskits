# axvirtio-net 实现与测试计划

## 1. 背景与目标

当前 feature 分支已经将 `axvirtio-common` 和 `axvirtio-blk` 引入
`virtualization/`。这两个 crate 面向 hypervisor 的 VirtIO 设备模拟：guest
驱动通过 VirtIO MMIO 寄存器和 guest 内存中的 split virtqueue 访问设备，设备
模型再把请求交给宿主 backend。它们不是运行在 guest 内的 VirtIO 前端驱动。

本任务目标是在 `axvirtio-common` 之上新增 `axvirtio-net`，提供一个可复用、
`no_std`、不耦合具体 VMM 网络运行时的 VirtIO 1.x MMIO 网络设备模型，并通过
确定性的协议级测试验证 feature 协商、队列处理、收发数据、异常输入、复位和
中断状态等行为。

目标交付物包括：

1. 补齐 `axvirtio-common` 中网络设备所需的通用 split virtqueue 能力。
2. 尽可能收敛设备无关的 VirtIO MMIO 状态机，避免 `axvirtio-net` 复制
   `axvirtio-blk` 的大段寄存器处理代码。
3. 新增 `virtualization/axvirtio-net` crate。
4. 实现单 RX/TX 队列对的 VirtIO-net 数据路径。
5. 添加 common、net 和既有 blk 的回归测试。
6. 完成格式化、测试和目标 crate clippy 验证。

## 2. 首版功能边界

### 2.1 首版支持

- VirtIO 1.x MMIO transport，设备 ID 为 `1`。
- split virtqueue。
- 一个 RX/TX 队列对：
  - queue 0：RX，设备向 guest 写入网络帧。
  - queue 1：TX，设备读取 guest 提交的网络帧。
- 固定或由构造参数提供的 6 字节 MAC 地址。
- 链路状态配置。
- 普通 descriptor chain 和跨多个 descriptor 的 scatter-gather 数据。
- MMIO feature selector、driver feature negotiation、device status、queue
  配置、queue notify、interrupt status/ack、device config generation 和 reset。
- guest 内存访问失败、非法 descriptor、非法 feature 和队列配置错误的显式处理。
- 宿主到 guest 的显式 push RX API。

### 2.2 首版协商 feature

首版只声明实际完整支持的 feature：

- `VIRTIO_F_VERSION_1`
- `VIRTIO_NET_F_MAC`
- `VIRTIO_NET_F_STATUS`

`VIRTIO_NET_F_MTU` 可在实现 MAC/status 后作为小型增量加入，但必须同时完成
配置空间字段、feature gating 和测试，不能只声明 feature bit。

### 2.3 首版不支持

- `VIRTIO_NET_F_CTRL_VQ` 及 control queue。
- `VIRTIO_NET_F_MQ` 及多队列对。
- `VIRTIO_NET_F_MRG_RXBUF`。
- checksum、TSO、UFO、GSO 和 guest offload。
- packed virtqueue。
- indirect descriptor。
- `VIRTIO_F_RING_EVENT_IDX`。
- RSS、hash report、standby、speed/duplex 等扩展。

未支持能力不得出现在 device feature bits 中。guest 写入未支持的 driver feature
后设置 `FEATURES_OK` 时，设备必须拒绝协商，并按 VirtIO 状态机保留可诊断状态。

## 3. 当前代码基线与前置问题

实现前应重新检查 feature worktree 中以下文件，因为计划文档位于主仓库根目录，
而代码基线当前位于 `feature-virtio-net` worktree：

- `virtualization/axvirtio-common/src/queue/mod.rs`
- `virtualization/axvirtio-common/src/queue/descriptor.rs`
- `virtualization/axvirtio-common/src/queue/available.rs`
- `virtualization/axvirtio-common/src/queue/used.rs`
- `virtualization/axvirtio-common/src/mmio/transport.rs`
- `virtualization/axvirtio-blk/src/mmio/device.rs`

已识别的前置问题如下。

### 3.1 `pop_avail` 尚未实现

`VirtioQueue::pop_avail()` 当前固定返回 `None`。net 数据路径不能依赖该占位
行为。common 层必须提供一次只消费一个 available head 的可靠 API，并正确处理
16 位 wrapping index。

### 3.2 队列地址 LOW/HIGH 写入模型不完整

当前 descriptor/available/used 地址 setter 在地址非零后拒绝再次设置，而 VirtIO
MMIO 使用 LOW 和 HIGH 两次 32 位写入组合 64 位地址。第一次 LOW 写入可能使第二次
HIGH 写入被拒绝，并且上层目前忽略 setter 错误。需要改为显式的低/高半部更新，
或者让 MMIO transport 保存未提交的地址寄存器，在 queue ready 前统一校验和提交。

### 3.3 descriptor API 混入设备类型判断

`get_data_buffers(head, VirtioDeviceID)` 在 common 层根据 Block/其他设备选择
descriptor 子区间。该接口把 block 请求布局泄漏到了通用队列层。应改为返回完整、
带方向信息的 descriptor chain，由 block/net 各自在设备层解释协议布局。

### 3.4 通用 MMIO 状态机存在复制风险

`axvirtio-blk::VirtioMmioBlockDevice` 同时持有 feature selectors、driver
features、status、queue selector、queues、interrupt status 和 config generation，
并直接处理绝大多数标准 MMIO 寄存器。若 net 再复制一份，后续修复会发生漂移。

建议在 common 中抽取小而清晰的 MMIO transport/state 对象，但不要一次制造庞大的
设备 trait。设备特有配置空间和 queue notify 应继续由具体设备处理。

### 3.5 错误类型需要设备无关化

common 的错误类型当前包含 `InvalidSector` 等 block 专用语义。此次不要求大规模
重写全部错误，但新增 API 应区分：

- transport/feature/status 错误；
- queue/descriptor/guest memory 错误；
- net backend/RX capacity 错误。

非平凡公共错误类型应遵循项目规范使用 workspace `thiserror`；若 common 的
`no_std`/依赖约束阻止采用，应在实现前记录原因，并至少实现 `Display` 和
`core::error::Error`。

## 4. 设计原则

### 4.1 分层

```text
VMM / network runtime
  |  push RX frame / consume interrupt event
  v
axvirtio-net device orchestration
  |  net header, RX/TX semantics, backend calls
  v
axvirtio-common MMIO state + split virtqueue
  |  GuestMemoryAccessor
  v
guest address space
```

- `axvirtio-common`：只拥有 VirtIO transport、queue 和 guest memory 协议逻辑。
- `axvirtio-net`：只拥有 VirtIO-net wire format、配置和 RX/TX 行为。
- VMM glue：拥有 TAP、虚拟交换机、IRQ controller 注入和任务调度。
- backend/runtime 相关 OS crate 不进入 `axvirtio-net` 普通依赖。

### 4.2 并发与所有权

- 队列消费和完成需要独占推进 index，API 优先使用 `&mut self` 表达。
- 不要求调用方把 OS lock 传入 portable crate。
- 不在持有 queue/global state lock 时调用外部 backend。
- backend 调用失败不能留下半写 guest ring 状态。
- RX 帧是否排队由 API 明确表达，不隐藏无限队列。
- 首版可以让设备对象内部使用短临界区，但应避免一个覆盖 MMIO、backend 和帧复制
  的大锁。

### 4.3 guest 输入不可信

所有 guest 提供的地址、长度、index、flags 和 feature bits 都必须视为不可信：

- 地址加长度必须 checked arithmetic，禁止溢出。
- descriptor index 必须小于 queue size。
- chain 最多访问 queue size 个 descriptor。
- 遇到环、重复 index 或非法 `next` 必须停止并返回错误。
- 未协商 indirect descriptor 时必须拒绝 `VIRTQ_DESC_F_INDIRECT`。
- TX descriptor 必须为 device-readable。
- RX descriptor 必须为 device-writable。
- 复制前先验证完整 chain 容量，避免部分写入 guest memory。
- 任何错误路径均不得 panic。

## 5. 建议的 common 层 API

具体命名可按实现时的代码结构调整，但能力边界应保持如下形状。

### 5.1 descriptor chain

```rust
pub struct DescriptorChain {
    head: u16,
    descriptors: Vec<Descriptor>,
}

impl DescriptorChain {
    pub fn head(&self) -> u16;
    pub fn descriptors(&self) -> &[Descriptor];
    pub fn readable_len(&self) -> VirtioResult<usize>;
    pub fn writable_len(&self) -> VirtioResult<usize>;
}
```

若希望避免 common 层分配，可实现受 queue size 限制的迭代器；但迭代器必须防止
环和越界，并且 net RX 仍需在写入前完成总容量验证。首版优先选择容易审计和测试
的实现，不为消除小型临时 `Vec` 引入复杂 unsafe。

### 5.2 available queue 消费

```rust
pub fn pop_available(&mut self) -> VirtioResult<Option<DescriptorChain>>;
```

语义：

1. queue 未 ready 或地址不完整时返回错误。
2. Acquire/guest-memory ordering 由 accessor/transport 边界明确保证。
3. 比较 guest `avail.idx` 和设备 `last_avail_idx`。
4. 无新 entry 返回 `Ok(None)`。
5. 读取 `ring[last_avail_idx % queue_size]`。
6. 完整验证 chain 后推进 `last_avail_idx`。

需要明确验证失败时是否消费 malformed head。建议设备能够完成或隔离坏请求，避免
同一个 head 永久阻塞队列；具体策略通过返回包含 head 的错误或独立 reject API
表达，不要仅返回丢失上下文的 `InvalidDescriptor`。

### 5.3 used queue 完成

```rust
pub fn complete(&mut self, head: u16, written_len: u32) -> VirtioResult<NotifyDriver>;
```

该操作顺序应为：

1. 设备完成所有 guest buffer 写入。
2. 写入 used element。
3. 使用正确的 publish ordering 更新 `used.idx`。
4. 根据已协商 feature/flags 计算是否通知 driver。

首版未启用 event index，因此只需尊重 `VIRTQ_AVAIL_F_NO_INTERRUPT`。现有
`UsedRing::should_notify()` 读取 used flags 的方向需要对照 VirtIO 规范重新核实，
测试必须覆盖通知抑制语义。

### 5.4 MMIO transport state

建议抽取类似：

```rust
pub struct VirtioMmioState<T> {
    config: VirtioConfig,
    status: u32,
    driver_features: u64,
    device_features_sel: u32,
    driver_features_sel: u32,
    queue_sel: u16,
    queues: Vec<VirtioQueue<T>>,
    interrupt_status: u32,
    config_generation: u32,
}

pub enum MmioWriteAction {
    None,
    QueueNotified(QueueIndex),
    DeviceReset,
}
```

通用对象处理标准寄存器；遇到 device config offset 则交回具体设备。queue notify
返回 action，由 block/net 在 common lock 之外推进数据路径。

验收点：`axvirtio-blk` 能迁移到该对象或至少共享同一组 helper，且现有 blk 行为
测试不回归。若迁移会显著扩大首版风险，可分成两个小提交：先补 queue 正确性，
net 可用后再抽 MMIO 状态；但最终不得留下两份新增的 net/blk MMIO 复制代码。

## 6. axvirtio-net crate 结构

```text
virtualization/axvirtio-net/
  Cargo.toml
  README.md
  src/
    lib.rs
    backend.rs
    config.rs
    constants.rs
    device.rs
    error.rs
    header.rs
  tests/
    net_tests.rs
```

职责划分：

- `lib.rs`：crate 文档和稳定 API re-export，不承载大段实现。
- `backend.rs`：宿主发送能力边界。
- `config.rs`：MAC、link status、可选 MTU 及 config generation 更新。
- `constants.rs`：仅 net 专用 feature bits、queue index 和 status 常量。
- `header.rs`：`virtio_net_hdr` wire format 与验证。
- `device.rs`：构造、MMIO device config 分派、TX notify、RX 注入和 reset。
- `error.rs`：net 专用、可匹配的错误。

crate 应保持 `#![no_std]`，仅在需要临时帧聚合或测试时使用 `alloc`。依赖优先为：

```toml
[dependencies]
axaddrspace.workspace = true
axvirtio-common.workspace = true
log.workspace = true # 仅在 workspace 已统一声明后使用
thiserror.workspace = true
```

如果仓库 workspace 尚未声明对应依赖，应先按仓库约定补入
`[workspace.dependencies]`，避免新 crate 写独立版本。

## 7. 公共 API 草案

### 7.1 backend

首版 backend 只处理 guest TX 到宿主网络的发送：

```rust
pub trait NetworkBackend: Send + Sync {
    fn transmit(&self, frame: &[u8]) -> Result<(), NetworkBackendError>;
}
```

宿主 RX 不建议设计成在 queue notify 中阻塞调用 `backend.receive()`。TAP、虚拟
交换机和异步 runtime 的到包模型不同，因此由 VMM glue 在收到帧后调用设备的 RX
入口更通用。

backend 错误不应被压缩成字符串。若不同 backend 需要自定义错误，可采用关联错误
类型，或由 adapter 映射到稳定的 `NetworkBackendError`。

### 7.2 配置

```rust
pub struct VirtioNetConfig {
    mac_address: [u8; 6],
    link_status: LinkStatus,
    mtu: Option<u16>,
}

pub enum LinkStatus {
    Down,
    Up,
}
```

避免用裸 `bool` 表示链路状态。只有协商 `VIRTIO_NET_F_STATUS` 后，guest 才能依赖
status 字段；只有协商 MTU feature 后才暴露 MTU 配置。

### 7.3 设备

```rust
pub struct VirtioMmioNetDevice<B, T> { /* private fields */ }

impl<B, T> VirtioMmioNetDevice<B, T> {
    pub fn new(
        mmio: MmioRegion,
        backend: B,
        net_config: VirtioNetConfig,
        guest_memory: T,
    ) -> VirtioResult<Self>;

    pub fn mmio_read(
        &self,
        address: GuestPhysAddr,
        width: AccessWidth,
    ) -> VirtioResult<usize>;

    pub fn mmio_write(
        &self,
        address: GuestPhysAddr,
        width: AccessWidth,
        value: usize,
    ) -> VirtioResult<DeviceEvent>;

    pub fn receive_frame(&self, frame: &[u8]) -> Result<RxOutcome, NetError>;
}
```

`MmioRegion` 应替代多个含义相近的裸参数，至少校验长度覆盖标准寄存器和 device
config。`mmio_write` 是否直接处理 TX 或返回 `QueueNotified` 由 common 状态对象的
最终设计决定，但 VMM 必须能获得稳定事件，以便在锁外注入 IRQ。

建议结果类型：

```rust
pub enum DeviceEvent {
    None,
    InterruptPending,
    Reset,
}

pub enum RxOutcome {
    Delivered { frame_len: usize },
    NoGuestBuffer,
}
```

`NoGuestBuffer` 是正常流控结果，不应和 malformed descriptor 或 guest memory fault
混为同一错误。是否由上层缓存/丢弃帧应由 VMM runtime 决定。

## 8. VirtIO-net wire format

首版不启用 mergeable buffers，header 应对应基础 `virtio_net_hdr`：

```text
u8  flags
u8  gso_type
le16 hdr_len
le16 gso_size
le16 csum_start
le16 csum_offset
le16 num_buffers or padding depending negotiated layout/spec version
```

实现前必须以仓库采用的 VirtIO 规范版本为准核对精确 layout、大小和
`num_buffers` 条件，不能依赖 Rust 默认 struct layout。推荐显式字节编解码，或者
使用 `#[repr(C)]` 加 compile-time size/alignment 断言，并确保所有多字节字段按
little-endian 处理。

未协商 offload 时：

- TX 只接受无 checksum/GSO 请求的 header。
- RX 写入全零 header。
- 非零、表示未支持 offload 的字段返回明确错误，不静默发送可能损坏的帧。

## 9. TX 数据路径

queue 1 notify 后按以下步骤处理，编排函数应保持这一阅读顺序：

1. 检查 `DRIVER_OK`、feature negotiation 和 TX queue ready。
2. 循环消费当前可见的 available heads，但设置合理上限，最多处理当前快照中
   的 available count。
3. 读取并验证 descriptor chain。
4. 验证所有 TX buffer 均为 device-readable。
5. 从 chain 开头读取完整 `virtio_net_hdr`，允许 header 跨 descriptor。
6. 验证未请求未协商的 offload。
7. checked-sum 计算 payload 总长度，并限制最大帧长度。
8. 将 header 后的 payload 聚合为连续帧，或通过 backend scatter-gather 扩展发送。
9. 在不持有 queue/global lock 的情况下调用 backend。
10. backend 成功后完成 used element。
11. 根据通知抑制状态设置 interrupt pending，并返回事件给 VMM glue。

首版建议使用有明确最大长度的临时 `Vec<u8>` 聚合 TX frame，先保证跨 descriptor
header/payload 边界正确。后续 profiling 证明复制是瓶颈后再引入 scatter-gather
backend API。

需要提前确定错误语义：VirtIO-net TX 没有每请求 status byte。对于 malformed
descriptor，设备不能无限重复消费同一 head。建议：

- 消费并以 used length 0 完成该 head；
- 记录可诊断错误；
- 对严重 queue corruption 设置 `DEVICE_NEEDS_RESET`；
- 不调用 backend。

最终行为必须逐项对照 VirtIO 规范，而不是仅模仿 block 设备。

## 10. RX 数据路径

`receive_frame(frame)` 按以下步骤处理：

1. 校验链路、设备状态、协商状态和 RX queue ready。
2. 拒绝超过配置上限的帧；最大长度应包含明确的 L2/FCS 语义。
3. 尝试消费一个 RX available head。
4. 没有 head 时返回 `RxOutcome::NoGuestBuffer`，不修改 ring。
5. 验证整个 chain 均为 device-writable。
6. 计算 header 加 frame 的总长度，先验证 chain 总容量。
7. 写入零初始化 header，然后按 descriptor 边界写入 frame。
8. 所有 guest memory 写入成功后，写 used element；used length 为 header 长度加帧
   长度。
9. 根据通知抑制设置 interrupt pending，并返回 `Delivered`。

首版不协商 `MRG_RXBUF`，因此一个 packet 只使用一个 descriptor chain，但该 chain
内部可以包含多个 descriptor。不得跨多个 available head 拼接一个 RX packet。

guest memory accessor 可能在中途失败。为了避免可观察的部分写入，至少必须先对
所有 descriptor 地址和长度做完整可访问性验证。如果 accessor 无法提供纯验证
API，需要决定使用 staging/预探测，或者接受 buffer 内容可能部分更新但 used ring
不完成，并在 API 文档中说明。优先扩展 accessor/common 边界实现预验证。

## 11. 配置空间与状态变更

设备配置空间至少包括：

- offset 0：`mac[0..6]`
- 随后按规范布局提供 `status`
- 可选 `max_virtqueue_pairs` 和 `mtu` 仅在对应 feature 支持时出现/有效

需要支持 byte/word/dword 等 device config 合法访问。标准 MMIO 寄存器仍要求规范
规定的 32 位访问，不能把 block 设备当前“config 一律 dword”的假设直接复制到 net
MAC 字段读取。

链路状态变化 API 应：

1. 更新 config 数据。
2. 增加 `config_generation`。
3. 设置 config-change interrupt bit。
4. 返回事件，由 VMM glue 注入实际 IRQ。

设备 status 写入必须验证合法状态转换和 feature subset。写入 0 应完整 reset：

- 清 driver features/selectors。
- 清 interrupt status。
- 重置 queue ready、地址、size 和 indexes。
- 清运行期 RX/TX pending 状态。
- 保留构造时的 device config，如 MAC 地址。

## 12. 中断边界

portable device 只维护稳定的 interrupt status/event，不直接依赖具体 GIC/APIC 或
VMM IRQ API。

- used buffer 完成设置 `VIRTIO_MMIO_INT_VRING`。
- 配置变化设置 `VIRTIO_MMIO_INT_CONFIG`。
- guest 写 interrupt ACK 时只清除指定 bit。
- 多个事件到达时 bit 必须合并，不能覆盖。
- 是否实际拉高/注入中断由 VMM glue 根据 `DeviceEvent` 决定。

如果后续接入 axvisor，应在 adapter 层把 `InterruptPending` 转为虚拟 IRQ 注入；不得
让 `axvirtio-net` 直接依赖 axvisor 的 vCPU 或 interrupt controller 类型。

## 13. 测试计划

所有 bug 修复和 common 前置问题遵循“先写失败测试、确认旧实现失败、再修复并确认
通过”。测试使用内存型 `GuestMemoryAccessor`，不依赖真实 TAP、网络权限或物理板。

### 13.1 common 单元测试

#### Queue 配置

- queue size 为 0 时拒绝。
- queue size 非 2 的幂时拒绝。
- queue size 超过 max 时拒绝。
- LOW 后 HIGH 写入得到正确 64 位 descriptor GPA。
- HIGH 后 LOW 写入同样得到正确地址。
- queue ready 前地址不完整时拒绝 ready 或在使用时返回明确错误。
- reset 清除全部地址、ready 和运行 index。

#### Available ring

- 空 ring 返回 `None`。
- 单 entry 返回正确 head。
- 多 entry 按顺序消费。
- `avail.idx` 从 `u16::MAX` 回绕到 0 时仍正确消费。
- ring entry 的 head 越界时返回包含 head 上下文的错误。
- guest `avail.idx - last_avail_idx` 大于 queue size 时识别为 queue corruption。

#### Descriptor chain

- 单 descriptor chain。
- 多 descriptor chain。
- `next` 越界。
- descriptor 自环。
- 多节点环。
- chain 长度超过 queue size。
- 未协商 indirect descriptor。
- 地址加长度溢出。
- guest memory 跨边界。
- readable/writable 总长度 checked-sum 溢出。

#### Used ring 和通知

- used element 的 id/len 写入正确 slot。
- used index 正确递增和回绕。
- payload 写入先于 used index publish。
- guest 设置 `VIRTQ_AVAIL_F_NO_INTERRUPT` 时不请求通知。
- 未设置抑制时请求通知。
- interrupt ACK 只清指定 bit。

### 13.2 MMIO transport 测试

- magic、version、device ID、vendor ID。
- feature selector 0/1 返回低高 32 位。
- 无效 selector 返回 0。
- driver features 低高 32 位组合正确。
- driver features 不是 device features 子集时拒绝 `FEATURES_OK`。
- queue selector 越界行为符合规范。
- queue notify 返回正确 queue index action。
- 标准寄存器非 32 位访问被拒绝。
- MMIO 区域外读取/写入行为一致且有测试锁定。
- status 写 0 完整 reset。
- config generation 读取正确。

### 13.3 axvirtio-net 配置测试

- 构造后 device ID 为 Network。
- device features 只包含首版能力。
- MAC 逐字节、word/dword 跨字段读取符合 little-endian 和访问边界。
- link up/down status 正确。
- 修改 link status 增加 config generation 并设置 config interrupt。
- 未协商 feature 时配置字段的行为符合规范。

### 13.4 TX 测试

- header 和 payload 位于同一 descriptor。
- header 与 payload 分属不同 descriptor。
- header 跨 descriptor 边界。
- payload 跨多个 descriptor，backend 收到完全相同的帧。
- VirtIO header 不传给 backend。
- 空 payload 的处理符合最终约定。
- descriptor 错误标记为 writable 时拒绝。
- header 过短。
- 非零 unsupported GSO/checksum 字段。
- frame 超过最大长度。
- guest memory 地址无效。
- descriptor chain 成环。
- backend 返回错误时 used ring、中断和错误状态符合约定。
- 成功 TX 后 used id、len、index 和 interrupt status 正确。
- guest 抑制中断时完成请求但不返回 interrupt event。
- 一个 notify 批量处理当前可见多个 TX 请求。
- queue index 回绕后继续发送。

记录型 backend 应保存收到的帧和调用次数，使测试能证明异常路径没有调用 backend。

### 13.5 RX 测试

- 单 writable descriptor 容纳 header 和 frame。
- header 与 frame 分散到多个 descriptor。
- 精确容量边界成功。
- 容量少 1 字节时完整拒绝，used ring 不推进。
- 无 available buffer 返回 `NoGuestBuffer`。
- readable descriptor 被拒绝。
- frame 超过上限被拒绝。
- guest memory 地址无效。
- descriptor chain 成环。
- 写入 header 全零且长度正确。
- used length 等于 header 加 frame，而不是只计算 payload。
- 成功 RX 设置 vring interrupt。
- guest 抑制中断时仍完成 RX，但不请求 IRQ。
- 连续多个 RX frame 按 available 顺序写入。
- index 回绕。
- 未启用 mergeable buffers 时不跨 available head 分割一个 packet。

### 13.6 端到端协议测试

在一个集成测试中完整模拟 guest：

1. 通过 MMIO 读取设备身份。
2. 写 ACKNOWLEDGE 和 DRIVER。
3. 读取并协商 features。
4. 设置 FEATURES_OK，并确认设备保留该 bit。
5. 配置 RX/TX descriptor、available 和 used ring GPA。
6. 设置 queue ready。
7. 写 DRIVER_OK。
8. guest 提交 TX frame 并 notify queue 1。
9. 验证 backend、used ring 和 interrupt status。
10. ACK TX interrupt。
11. guest 提交 RX buffer，宿主调用 `receive_frame`。
12. 验证 guest memory 中的 header/frame、used ring 和 interrupt。
13. 写 status 0，验证设备和两条 queue 完整 reset。

该测试是首版最重要的验收用例，应只通过公共 API 使用设备，避免依赖私有字段。

### 13.7 axvirtio-blk 回归

common 修改后运行现有全部 block tests，重点新增或确认：

- 64 位 queue 地址 LOW/HIGH 配置。
- available queue 实际消费。
- used ring 通知语义。
- reset 后可重新配置 queue。
- common MMIO state 迁移前后寄存器行为一致。

## 14. 分阶段实施与提交建议

### 阶段 A：锁定协议与测试基线

任务：

- 确认采用的 VirtIO 规范版本。
- 将首版 features、header layout、queue index 和错误策略写入 crate 文档。
- 运行并记录 `axvirtio-common`/`axvirtio-blk` 当前测试和 clippy 基线。
- 为 `pop_avail` 和 LOW/HIGH 地址问题添加失败回归测试。

验收：测试能稳定暴露两个已知问题，没有通过放宽断言掩盖缺陷。

### 阶段 B：补齐 common queue

任务：

- 实现 queue 地址组合与 ready 校验。
- 实现 available head 消费。
- 引入设备无关 descriptor chain。
- 完善环、越界、方向和长度检查。
- 修正 used completion 和通知判定。

验收：common 新增测试全部通过；blk tests 不回归。

### 阶段 C：收敛 common MMIO transport

任务：

- 抽取标准寄存器状态。
- 以 action/event 形式交出 queue notify/reset。
- 将 device config 访问保留给具体设备。
- 迁移 `axvirtio-blk` 或建立共享 helper，删除可避免的复制。

验收：blk 的 MMIO 公共行为保持一致；标准寄存器测试集中在 common。

### 阶段 D：搭建 axvirtio-net

任务：

- 新增 crate、workspace dependency 和 README。
- 实现 net config、feature bits、header 和 typed errors。
- 创建两条 queue。
- 实现 MMIO device config 读取和 reset。

验收：身份、feature negotiation、MAC/status、queue 配置测试通过。

### 阶段 E：实现 TX

任务：

- descriptor chain 解析。
- header 跨 descriptor 读取与验证。
- payload 聚合。
- backend 调用。
- used completion、interrupt 和错误处理。

验收：TX 测试矩阵全部通过，异常路径不调用 backend、不 panic。

### 阶段 F：实现 RX

任务：

- `receive_frame` 流控 API。
- 完整容量预检。
- header/payload scatter 写入。
- used completion 和 interrupt。

验收：RX 测试矩阵全部通过，无 buffer 和 malformed buffer 可明确区分。

### 阶段 G：端到端与质量验证

任务：

- 完整 guest 初始化和收发集成测试。
- fmt、test、clippy。
- 检查 std/clippy 测试白名单是否需要新增 crate。
- 更新 README，记录支持/不支持 features 和 VMM 集成方式。

验收：本计划第 15 节命令全部通过，无新增 warning。

建议每个阶段形成独立、可测试的提交，避免把 common 重构、blk 迁移、net TX/RX 和
测试一次性压入单个提交。

## 15. 本地验证命令

代码修改后至少执行：

```bash
cargo fmt --all
cargo test -p axvirtio-common
cargo test -p axvirtio-net
cargo test -p axvirtio-blk
cargo xtask clippy --package axvirtio-common
cargo xtask clippy --package axvirtio-net
cargo xtask clippy --package axvirtio-blk
```

如果 common MMIO API 被其他 virtualization crate 使用，还应对所有直接消费者运行
目标 clippy/test。若新增 crate 符合 std 测试条件，使用项目的 `update-std-tests`
流程审计 `scripts/test/std_crates.csv`，不要凭名称直接加入。

可选的高层集成验证：

- 将设备接入 axvisor 的 MMIO device 和虚拟 IRQ adapter。
- 使用 Linux guest 的 `virtio_net` 驱动完成设备枚举。
- 使用隔离的 TAP/用户态虚拟交换机进行 ARP、ICMP 和 UDP smoke test。

上述测试需要额外 VMM glue，不应替代确定性的 crate 内协议测试。

## 16. 主要风险与控制措施

### 16.1 common 基线尚不完整

风险：直接写 net 会复制 blk 的临时实现，并把 queue bug 固化到新 API。

控制：先以失败回归测试补齐 common；net 不自行维护第二套 ring index。

### 16.2 MMIO 抽取范围过大

风险：一次重构整个 blk 会扩大变更面并使 net 进度停滞。

控制：按 queue 正确性、标准 MMIO 状态、blk 迁移三个小步骤推进；每步保持 blk
测试通过。

### 16.3 RX 生命周期和背压不清晰

风险：设备内部无限缓存宿主帧导致内存失控，或者无 guest buffer 时静默丢包。

控制：首版 `receive_frame` 返回 `NoGuestBuffer`，缓存/重试/丢弃策略由 VMM runtime
显式决定。

### 16.4 guest memory 部分写入

风险：RX 写到一半遇到地址错误，guest 看到损坏 buffer 且 ring 未完成。

控制：写入前验证所有 ranges 和总容量；必要时扩展 accessor 提供预验证能力。

### 16.5 中断状态与实际 IRQ 注入混淆

风险：portable crate 直接依赖 VMM IRQ controller，失去复用性；或只设置 status
而上层不知道何时注入。

控制：设备返回稳定 `DeviceEvent`，VMM adapter 负责实际 IRQ 注入。

### 16.6 宣告未实现 feature

风险：Linux guest 根据 feature 改变 header/queue 行为，造成协议错位。

控制：feature allowlist；每新增一个 bit 必须同时新增成功、拒绝和 layout 测试。

### 16.7 锁与 backend 回调死锁

风险：持设备锁调用 backend，backend 回调 RX 或 VMM 路径后重入设备。

控制：解析/复制、backend 调用、queue completion 分阶段；外部回调期间不持 broad
lock，并记录并发所有权约束。

## 17. 完成定义

满足以下条件后，首版 `axvirtio-net` 才算完成：

- `axvirtio-common` 不再有固定返回 `None` 的 available queue 占位路径。
- 64 位 queue 地址能通过 MMIO LOW/HIGH 正确配置。
- `axvirtio-net` 为 `no_std`，普通依赖不包含具体 OS/VMM runtime。
- 只声明已实现且有测试的 features。
- 单 queue pair TX/RX 均支持多 descriptor chain。
- malformed guest 输入不会 panic、越界访问或调用错误 backend 路径。
- RX 无 buffer 有明确流控结果。
- used ring、interrupt status/ack、config interrupt 和 reset 有确定性测试。
- 完整 guest 初始化、TX、RX、ACK、reset 集成测试通过。
- `axvirtio-common`、`axvirtio-net`、`axvirtio-blk` 的测试和 clippy 通过。
- `cargo fmt --all` 后工作树无格式变化。
- README 记录架构、公共 API、支持 features、限制和 VMM 集成责任。

## 18. 后续演进

首版稳定后按实际需求逐项增加，每项单独协商 feature、实现和测试：

1. `VIRTIO_NET_F_MTU`。
2. 有界 RX backlog/runtime adapter。
3. `VIRTIO_F_RING_EVENT_IDX`。
4. indirect descriptor。
5. `VIRTIO_NET_F_MRG_RXBUF`。
6. checksum/offload。
7. control virtqueue。
8. multiqueue 和 queue-pair 局部同步。
9. axvisor/TAP adapter 与 Linux guest smoke test CI。

扩展时继续保持：中断只同步状态，任务或 VMM runtime 推进慢路径；portable 设备模型
不承担 OS 调度、TAP 生命周期和具体虚拟中断控制器职责。
