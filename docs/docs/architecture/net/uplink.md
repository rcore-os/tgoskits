---
sidebar_label: "虚拟交换与物理上联"
---

# 虚拟交换与物理上联

AxVisor 的 virtio-net 设备通过 `VirtualSwitch` 连接到一个由宿主网络栈管理的物理接口。上联只借用该接口已经存在的 queue owner，不复制驱动或 DMA 队列；`net/ax-net/src/uplink.rs` 负责有界的异步交接，`virtualization/axvm/src/configured/devices/virtio_net.rs` 负责虚拟交换和宿主接口之间的能力边界。

## 1. 运行时边界

### 1.1 数据路径

guest TX 在 vCPU 的 virtio 写路径中进入 `PhysicalUplink::submit_guest_egress`，随后复制到 `UplinkRuntime` 的固定容量 ring。只有物理接口所属的 protocol executor 才能调用 `drain_egress` 并提交现有 TX queue；RX 则在 host MAC 过滤前由 `deliver_ingress` 复制到 `IngressSink`，再进入 `VirtualSwitch`。这条路径保证虚拟设备不会直接触碰物理 DMA 所有权。

```mermaid
flowchart LR
    G[guest virtio-net] --> S[VirtualSwitch]
    S -->|known guest MAC| G
    S -->|uplink egress| P[PhysicalUplink]
    P --> R[UplinkRuntime bounded ring]
    R --> Q[interface protocol executor]
    Q --> H[existing NIC TX queue]
    H --> I[host RX before MAC filter]
    I --> R2[IngressSink]
    R2 --> S
```

`UplinkRuntime` 的 egress ring 默认由 `DEFAULT_EGRESS_DEPTH` 个预分配 slot 组成，单次 `drain_egress` 受 `EGRESS_DRAIN_BUDGET` 限制。vCPU 路径遇到 `UplinkEgressError::Full` 或 `InvalidFrame` 会得到 `PhysicalUplinkError`，virtio backend 将其报告为传输失败；因此 ring 背压不会伪装成成功发送。没有安装物理上联时，交换机仍可完成 guest 端口之间的转发，发往未知物理目的地的帧在交换机边界结束。

### 1.2 所有权与生命周期

`NetPollGroup`、队列 control endpoint 和 DMA queue 仍由 `ax-net` 的 queue runtime 按设备实例拥有。`UplinkRuntime::bind_interface` 绑定稳定的 `InterfaceId`，`EthernetDevice` 只为该 id 查询上联，因此接口改名或探测顺序变化不会把 guest 流量导向另一张网卡。上联注册发生在 guest device 创建之前；`axvm::install_physical_uplink` 只接受一个 adapter，重复注册失败并恢复候选接口的 `NetRxMode::Normal`。

## 2. 接口能力

### 2.1 接收模式

`rdif_eth::NetRxMode` 描述接收覆盖范围，而不是某个厂商寄存器位。`NetControlEndpoint::set_rx_mode` 由每个驱动实例实现，调用者通过 `ax_net::set_interface_rx_mode` 将请求路由到对应 `InterfaceId` 的 control endpoint。无法提供覆盖范围的设备返回 `NetError::NotSupported`，AxVisor 会跳过该接口并继续尝试其他 Ethernet 接口。

| 模式 | 语义 | 上联用途 |
| --- | --- | --- |
| `Normal` | 设备地址、广播和设备默认允许的组播 | 普通 host stack |
| `AllUnicast` | 在普通流量之外接收所有单播目的地址 | 接收 guest MAC |
| `Promiscuous` | 接收设备能够交付的所有 Ethernet 帧 | 当前上联不要求 |

RTL8125 当前以硬件的 all-unicast 能力实现 `Normal`/`AllUnicast`，并对 `Promiscuous` 明确返回不支持。其他驱动不应复制这个寄存器实现；应在各自 control endpoint 中根据硬件过滤器提供同样的语义，不能为了接入上联而把固定控制端点强行宣称为可用。

### 2.2 接口选择

`os/axvisor/src/net_uplink.rs` 先筛选带 MAC 的 `InterfaceKind::Ethernet`，再逐接口请求 `NetRxMode::AllUnicast`。请求成功的接口才会被 `UplinkRuntime` 绑定并登记宿主 MAC；TAP、TUN、loopback 以及没有该能力的物理设备不会被选为 L2 下层。

## 3. 启动与错误处理

### 3.1 启动顺序

启动顺序把设备能力探测放在虚拟设备发布之前，避免 guest 能够观察到一个尚未准备好的上联。`net_uplink::start` 的关键阶段如下：

1. 预留所有已发布 host MAC，防止 guest 端口复用宿主地址。
2. 以 `InterfaceId` 选择并切换一个支持 `AllUnicast` 的 Ethernet 接口。
3. 在本地创建并绑定 `UplinkRuntime`，登记 ingress sink，先安装 `PhysicalUplink` adapter，再发布 runtime 指针。
4. 完成后才调用 `AxvmManager::init_default_vms`，让 guest virtio-net 端口加入交换机。

候选接口不支持所需模式时不会修改其他接口；AxVM 上联注册竞争失败时会把候选接口恢复为 `Normal` 并终止本次安装。runtime 只有在 adapter 注册成功后才发布，避免竞争失败留下可见的半初始化对象。物理接口启动、停止和 queue rearm 仍由 `ax-net` 的既有 owner-CPU 生命周期负责，本层不模拟 Linux `phylink` 或 `ethtool` 控制面。

### 3.2 当前边界

当前 AxVisor 启动路径发布一个进程级上联和一个内部交换机，因此一次启动只选择一个 host interface；`InterfaceId` 和 per-device capability API 已经允许后续把上联对象下沉到 VM 或 switch 实例。`ax-net` 的 `ETHERNET_FRAME_CAPACITY` 当前为 2048 字节，而 virtio-net 接收路径的 `MAX_FRAME_SIZE` 为 65535 字节；超过前者的 guest TX 会得到 `PhysicalUplinkError::InvalidFrame` 并映射为 `TransmitFailed`，后续应通过 MTU 协商或按端口配置的 slab 消除这两个边界的重复定义。精确 MAC 地址表、promiscuous lease、多个独立交换机以及运行期热插拔仍不属于本接口，新增这些能力时应扩展 lease/address-set 语义，而不是重新引入厂商名称或接口名称匹配。
