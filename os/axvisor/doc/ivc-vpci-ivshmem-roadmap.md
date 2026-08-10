# AxVisor IVC 向 vPCI/ivshmem 设备演进的工作分析

本文记录当前 AxVisor IVC 如果要演进成类似 Jailhouse 或 Rust-Shyper 风格、Linux 可标准发现和使用的 vPCI/ivshmem 设备，需要完成的主要工作。

## 当前状态

当前 IVC 已经接入统一模拟设备框架，但它不是一个传统 MMIO/PCI 设备。

当前路径是：

```text
VM config
  -> EmulatedDeviceType::IVCChannel
  -> IvcChannelFactory
  -> DeviceRuntime services
  -> HVC publish/subscribe/notify
```

`ivc-channel` 现在主要贡献两个能力：

- `GuestRangeAllocatorKey`：为 IVC channel 分配 guest physical address 范围。
- `IvcNotifyIrqKey`：记录可选的 VM-local notify IRQ。

它不注册 MMIO `Device`，也不通过 MMIO read/write 访问路径处理 guest 请求。guest 仍主要通过 HVC 完成 publish、subscribe、unsubscribe 和 notify。

## 目标形态

如果目标是 Jailhouse 风格的 ivshmem，Linux guest 应该通过 PCI 枚举发现设备，而不是依赖 HVC 得到共享内存信息。

目标路径应类似：

```text
VM config
  -> vPCI ivshmem device
  -> Linux PCI probe
  -> BAR mmap shared memory
  -> doorbell/MSI-X or INTx notify
  -> upper-layer shared memory protocol
```

这样 Linux 侧可以通过标准 PCI 设备模型发现：

- device/vendor ID；
- peer ID；
- peer 数量；
- BAR register 区；
- shared memory BAR；
- MSI-X/INTx 中断能力；
- state table 和 doorbell register。

## 需要完成的工作

### 1. 明确设备模型

需要先决定 AxVisor IVC 是做成：

- 尽量兼容 Jailhouse/QEMU ivshmem 的标准 PCI ivshmem 设备；
- 还是先做 AxVisor 自定义 PCI IVC 设备；
- 或者先保留 HVC 控制面，同时新增 PCI 数据面。

建议短期不要直接删除现有 HVC API。HVC 对 ArceOS 和裸机 guest 仍然简单直接，可以作为兼容路径保留。

### 2. 实现 vPCI/ivshmem 模拟设备

当前 `ivc-channel` 只贡献 service。要做成标准 Linux 设备，需要新增真正的 vPCI device。

该设备至少需要提供：

- PCI config space；
- BAR0：ivshmem register 区；
- BAR1：MSI-X table/PBA，如果支持 MSI-X；
- BAR2：shared memory region；
- 可选 INTx fallback。

如果 AxVisor 当前 vPCI 框架还不完整，还需要补齐：

- vPCI host bridge；
- ECAM/config space 访问；
- BAR 分配；
- PCI capability 链；
- MSI/MSI-X 中断路由；
- AArch64 下通过 FDT 暴露 PCI host bridge 给 Linux。

这部分很可能是最大工作量。

### 3. 实现 ivshmem 寄存器语义

Jailhouse/ivshmem 的关键寄存器包括：

- ID register：当前 peer ID。
- max peers register：peer 总数。
- interrupt control：是否允许接收中断。
- doorbell register：写入 target peer ID 和 vector，触发目标 peer 中断。
- state register：写本 peer 状态，并更新 state table。

AxVisor 需要在 doorbell 写入时执行：

```text
source VM writes doorbell
  -> decode target peer ID/vector
  -> find target VM endpoint
  -> inject MSI-X/INTx/virtual IRQ to target VM
```

这会替代或补充当前的：

```text
HIVCNotify
  -> target_vm.pulse_interrupt(irq)
```

### 4. 重新设计共享内存布局

当前 IVC region 是 header 加双向 ring 的简单布局。

Jailhouse ivshmem 风格通常需要分区：

```text
State Table
Common RW section
Peer 0 output section
Peer 1 output section
...
Peer N output section
```

关键要求：

- State Table 对所有 peer 只读。
- Common RW section 可选，所有 peer 可读写。
- 每个 peer 的 output section 只有 owner 可写，其他 peer 只读。
- 所有 section 页对齐，方便 stage-2/NPT 设置权限。

这比当前双方共享同一块可写内存更规范，也更适合多 peer 场景。

### 5. 做权限隔离

标准 ivshmem 的一个重要价值是可以通过页表权限减少写乱风险。

需要在 stage-2/NPT 做权限控制：

- 本 peer output section：RW；
- 其他 peer output section：RO；
- state table：RO；
- common section：RW，如果启用；
- register BAR：trap 到 AxVisor；
- shared memory BAR：按 section 权限映射。

当前 IVC 更多依赖协议约束双方不要写乱。演进为 ivshmem 后，应把这部分约束下沉到内存权限。

### 6. Linux 侧驱动

Linux 侧有两条路线。

第一条是兼容现有 ivshmem 生态：

- 尽量匹配 ivshmem vendor/device ID；
- 尽量匹配 ivshmem v2 register layout；
- 尝试复用 `uio_ivshmem` 或现有 ivshmem 用户态工具。

优点是更标准，缺点是 AxVisor 设备语义必须更贴近既有规范。

第二条是写 AxVisor 自定义 PCI IVC driver：

- PCI probe；
- ioremap register BAR；
- mmap shared memory BAR 给 userspace；
- 注册 MSI-X/INTx handler；
- 提供 `/dev/axivcX`；
- userspace publisher/subscriber 直接读写 mmap ring 或标准 ivshmem section。

优点是可控，缺点是生态兼容性弱。

### 7. 处理 HVC API 的兼容关系

当前 IVC HVC API 包括：

- `HIVCPublish`
- `HIVCSubscribe`
- `HIVCUnpublish`
- `HIVCUnsubscribe`
- `HIVCNotify`

PCI 化后建议分阶段处理：

1. 保留 HVC API，避免 ArceOS/裸机 guest 立刻断裂。
2. Linux guest 新增 PCI ivshmem 路径。
3. ArceOS 后续也实现 PCI ivshmem driver。
4. 最后再决定是否弱化 HVC API。

### 8. 扩展 VM 配置

当前配置示例：

```toml
["ivc-channel", base, size, ..., [notify_irq]]
```

如果变成 vPCI/ivshmem，需要能描述 link 和 peer：

```toml
name = "ivshmem0"
link_id = 1
peer_id = 0
peers = 2
bdf = "00:05.0"
state_table_size = 0x1000
rw_section_size = 0x1000
output_section_size = 0x10000
msix_vectors = 2
```

还需要明确：

- 哪些 VM 属于同一个 link；
- 每个 VM 的 peer ID；
- BAR 地址由 vPCI 自动分配还是配置固定；
- shared memory backing 由谁分配、何时释放；
- VM reset/remove 时如何清理 peer state。

### 9. 测试矩阵

至少需要覆盖：

- Linux 能枚举 PCI ivshmem 设备。
- Linux driver probe 成功。
- BAR mmap 后能读写共享内存。
- doorbell 能触发目标 VM 中断。
- A 到 B、B 到 A 双向通信。
- 三个及以上 peer 时 target ID 不串。
- 权限测试：peer 不能写其他 peer 的 output section。
- VM reset/unsubscribe 后 state table 清理正确。
- 现有 HVC IVC 测例不回归。

## 推荐演进路线

建议分三步走：

1. 保留当前 HVC IVC，继续作为可用基线。
2. 在统一模拟设备框架中新增真正的 MMIO/PCI register 设备，先跑通 register + shared memory + IRQ。
3. 再向 Jailhouse ivshmem v2 register/layout 靠拢，最终支持 Linux 标准 PCI 枚举和 MSI-X/doorbell。

如果当前 vPCI 基础设施还不成熟，优先级应是：

```text
vPCI host bridge
  -> PCI config space
  -> BAR mapping
  -> MSI-X/INTx
  -> ivshmem register/device
  -> Linux driver/userspace
```

不要一开始就把 HVC 路径删除。否则一旦 vPCI/MSI-X 基础设施卡住，IVC 会失去当前已经能跑通的测试基线。

## 当前 IVC 与目标形态的差距

当前 IVC 已经具备：

- VM config 驱动的 IVC channel 注册；
- 共享内存 GPA 分配；
- publish/subscribe 生命周期；
- notify IRQ 配置；
- HVC notify；
- ArceOS/Linux demo 和 CI 测例基础。

仍缺少：

- Linux 标准 PCI 枚举；
- ivshmem BAR/register 模型；
- doorbell register；
- state table；
- per-peer output section；
- section 级内存权限隔离；
- MSI-X/INTx 标准中断路径；
- 多 peer 标准化协议；
- 可复用的 Linux ivshmem/uio 驱动路径。

因此，当前 IVC 更像是 AxVisor 自定义 HVC-based shared-memory channel；目标形态则是标准 vPCI/ivshmem shared-memory device。两者可以并存一段时间，逐步迁移。
