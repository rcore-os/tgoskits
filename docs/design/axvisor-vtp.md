# VTP：StarryOS 与 RTOS 客户机之间的 IP 应用层协议

## Problem and success criteria

Axvisor 已能在两个 ArceOS 客户机之间提供 VirtIO MMIO 网络设备并通过内部
L2 switch 转发 Ethernet 帧（见 [axvisor-virtio-net.md](./axvisor-virtio-net.md)）。
但仓库中没有跨 **StarryOS/Linux 宏内核客户机** 与 **RTOS 客户机** 的 IP
链路演示，也没有定义在其之上的应用层协议。本设计补齐两条：

1. **链路**：StarryOS 客户机与 FreeRTOS 客户机各自持有一块
   `[[devices.virtual]] model = "virtio-net"` 虚拟网卡，经 Axvisor 内部
   `VirtualSwitch` 直连；两端配置私有 IP（10.0.2.15 / 10.0.2.16），构成
   基于 IP 协议栈的双向网络链路。**共享内存、HyperCall、裸 MMIO 不作为主数据
   通道**；本设计的以太网帧承载数据，路径为
   `guest NIC → virtio-net device → L2 switch → peer virtio-net device → peer NIC`。
2. **协议**：定义一个运行在 UDP 之上的应用层协议 **VTP（Virtual Transport
   Protocol）**，报头包含版本、消息类型、载荷长度、序号、时间戳、校验字段；
   支持控制指令、状态回传、错误通知三类消息。协议码编为两端共用的纯 C 实现。

成功标准：

- StarryOS 与 FreeRTOS 客户机在 QEMU aarch64 / Axvisor 下同时启动，各自发现
  `eth0` 并配置私有 IP。
- VTP 双向数据收发成功（控制请求→ACK、状态回传、数据载荷、错误通知）。
- QEMU 控制台出现两端独立打印的 `STARRY_VTP_PASS` 与 `FREERTOS_VTP_PASS`
  marker（以及失败 marker 与 panic 互斥）。

非目标：host uplink / 物理网口直通、VirtIO 多队列与 offload、TCP 可靠传输、
MTU 分片重组、虚拟网卡热插拔与 live migration。

## 链路设计（复用现有能力）

数据通道完全复用仓库已有实现，不新增 hypervisor 侧代码：

| 组件 | 位置 | 角色 |
| --- | --- | --- |
| `virtio-net` DeviceModel | `os/axvisor/src/virtio_net.rs` | 每客户机一块虚拟网卡，固定 MMIO `0x0a00_0000` / SPI 48，按 VM 配置 `guest_mac` |
| `VirtualSwitch` | `virtualization/axvirtio-net/src/switch.rs` | L2 转发：已知单播直达目标端口，广播/组播扇出，源 MAC 反欺骗，有界 ingress 队列 |
| VirtIO MMIO + split-ring | `axvirtio-common` | 设备核心的 MMIO 状态与描述符环机制 |
| guest 侧驱动（Starry） | `drivers/ax-driver/src/virtio/net.rs` | `rd_net::Interface`，经 `ax-driver/virtio-net` feature 复用 |
| guest 侧 IP 栈（Starry） | `net/ax-net` | smoltcp，POSIX socket 系统调用暴露 |

FreeRTOS 侧是本设计的主要新增工作：仓库内没有 FreeRTOS virtio-net 驱动与
lwIP 移植。RTOS 侧需要：

1. **virtio-mmio split-ring 驱动**（C，`guests/freertos-vtp/`）：MMIO 传输
   寄存器（magic/version/features/queue/notify/status/config）、描述符表 +
   available ring + used ring、TX/RX 队列、中断状态查询/应答。逻辑移植自
   `axvirtio-common`，与 VirtIO v1.1 spec 对齐。
2. **lwIP netif 接入**：`low_level_output` 经 TX 队列发送；`low_level_input`
   由 RX 收包线程轮询 used ring 取帧；netif 注册 MAC 与 MTU（1500）。
3. **VTP 应用**（lwIP socket + 同一份 `vtp.c`）。

### 配置要点

- 两份 guest VM 配置均 **不设置 `dtb_path`**：`virtualized` 客户机的 DTB 由
  Axvisor 在运行时从 host FDT + machine profile + 虚拟设备生成
  （`virtualization/axvm/src/boot/fdt/core/create.rs`），配置含
  `model="virtio-net"` 时会自动注入 `/virtio_mmio@a000000` 节点（reg
  `0x0a00_0000 0x200`、SPI 48、level、`dma-coherent`）。若客户机改用自带
  DTB，该节点不会被注入（自带 DTB 仅 patch CPU 节点）。
- FreeRTOS 如果启动时不读运行时 DTB，则 virtio_mmio 地址/IRQ 必须以固定值
  编入其板级配置，与 Axvisor 的 fixed binding（`0x0a00_0000` / SPI 48）一致。
- 两个端口 MAC 必须不同（`52:54:00:12:34:56` / `52:54:00:12:34:57`），否则
  `VirtualSwitch` 拒绝重复 MAC。

## VTP 协议设计

### 报头（18 字节固定 + 载荷，网络字节序）

| 偏移 | 大小 | 字段 | 说明 |
| --- | --- | --- | --- |
| 0 | 2 | `magic = 0xA5A5` | 帧同步 / 非法包检测 |
| 2 | 1 | `version = 0x01` | 版本，不匹配即拒绝 |
| 3 | 1 | `msg_type` | CONTROL=1 / STATUS=2 / DATA=3 / ERROR=4 / ACK=5 |
| 4 | 1 | `flags` | bit0 REQUEST、bit1 LAST_FRAGMENT、bit2 ACK_REQUESTED |
| 5 | 1 | `reserved` | 必须为 0 |
| 6 | 4 | `seq` | 发送端单调序号；ACK 回显请求的 seq |
| 10 | 4 | `timestamp_ms` | 发送端单调时钟（ms） |
| 14 | 2 | `payload_len` | ≤ 1400，避免 IP 分片 |
| 16 | 2 | `checksum` | CRC16-CCITT（poly 0x1021）覆盖 `header[0..16] + payload` |

### 消息类型与载荷

- **CONTROL**：`{ cmd: u8, data[0..255] }`。命令集 `PING=1`、`SET_STATE=2`、
  `REQ_STATUS=3`、`RESET=4`。带 `ACK_REQUESTED` 的请求收到 ACK 回显。
- **STATUS**：`{ state: u8, code: u8, uptime_ms: u32(be), extra[0..255] }`。
  状态枚举 `INIT/READY/RUNNING/DEGRADED/MAINTENANCE/ERROR`。
- **DATA**：原始字节载荷。
- **ERROR**：`{ error_code: u16(be), source: u8, detail[0..255] }`。
- **ACK**：`{ ack: u8, error_code: u16(be) }`，`seq` 回显被应答请求。

错误码：`OK=0`、`UNSUPPORTED_VERSION=1`、`BAD_CHECKSUM=2`、`UNKNOWN_CMD=3`、
`INVALID_PAYLOAD=4`、`SEQ_MISMATCH=5`、`TIMEOUT=6`、`NOT_READY=7`、
`RESOURCE_BUSY=8`、`BAD_MAGIC=9`。

### 可靠性

VTP 运行于 UDP 之上，可靠性由应用层承担：

- **序号**：发送端单调递增；接收端 `vtp_peer_t` 检测重复/乱序，重复包返回
  `SEQ_MISMATCH`（可触发错误通知）。
- **ACK + 重传**：带 `ACK_REQUESTED` 的控制请求在超时内未收到 ACK 时重传
  （上限 5 次，退避 20ms × 2^n）。
- **错误通知**：解码失败（坏校验、未知版本、非法载荷）时对端可主动发送
  `ERROR` 消息并附错误码——用于演示与日志。

## 数据流

```
StarryOS (VM1, 10.0.2.15, MAC 52:54:00:12:34:56)
   VTP server: bind UDP :6000, REQ_STATUS 轮询 RTOS, 双向 DATA, 错误注入路径
        │ eth0
   virtio-net device (0x0a00_0000 / SPI48)
        │ frames
   VirtualSwitch (MAC 学习 / 反欺骗 / 单播直达)
        │ frames
   virtio-net device (0x0a00_0000 / SPI48)
        │ eth0
FreeRTOS (VM2, 10.0.2.16, MAC 52:54:00:12:34:57)
   VTP agent: 周期性 STATUS 上报 + 响应 CONTROL + 回传 DATA
```

## 失败行为

- 对端未就绪：VTP 应用重试连接/等待，超时后打印失败 marker，不伪造成功。
- 解码失败：`vtp_decode` 返回负错误码，调用方转为 `ERROR` 消息或日志，不吞错。
- 校验不符：帧被丢弃并计数，可通过注入坏校验触发对端 `ERROR(BAD_CHECKSUM)`。
- 设备未注册 model / MAC 冲突：Axvisor 在 VM 创建时报错，不静默降级。

## Alternatives

| 方案 | 决定 |
| --- | --- |
| 用共享内存 / IVC / HyperCall 作主通道 | 拒绝：任务明确禁止非网络机制作主数据通道 |
| 宿主 TAP/桥接 + 用户态网络 | 拒绝：需要 host 侧新驱动/网络栈，且非本仓库能力；内部 L2 switch 已实现双端口直连 |
| RTOS 侧用 Zephyr（主线自带 ETH_VIRTIO） | 否决（用户选定 FreeRTOS）：需自写 virtio 驱动 + lwIP 移植，但 FreeRTOS+lwIP 生态成熟、可控 |
| RTOS 侧实现完整 TCP 栈 | 否决：lwIP 已提供；VTP 用 UDP + 应用层 ACK，满足协议字段要求且更轻量 |
| 协议码编各端独立实现 | 拒绝：重复知识；共享 `vtp.h/vtp.c` 保证两端语义一致 |

## Validation

- **码编单测**（host，容器/Linux）：`cc -std=c11 -Wall -Wextra -Werror vtp_test.c vtp.c -o vtp_test && ./vtp_test`，
  覆盖 round-trip、坏校验、坏 magic、坏版本、截断、超长载荷、序号去重、类型化消息。
  通过条件：`VTP_TEST_PASS`。
- **链路（Starry 单侧）**：QEMU 下 Starry guest 起网，`eth0` 有 IP、ARP 可达对端。
- **端到端**：`cargo xtask axvisor test qemu --arch aarch64 --test-group normal --test-case vtp`，
  通过条件：QEMU 输出同时含 `STARRY_VTP_PASS` 与 `FREERTOS_VTP_PASS`；任一
  `*_FAIL` 或 panic 即失败。
- **错误路径**：注入坏校验帧，对端回 `ERROR(BAD_CHECKSUM)`；验证日志与
  失败路径不破坏主数据流。

## 高风险边界

本功能属于 feature-development.md 的高风险类别（新增协议、跨系统集成、
RTOS 侧新驱动）。设计材料即本文档，可在实现 diff 之前独立评审。实现的每个
提交保持可构建；码编单测先行，再接入两端应用，最后做端到端用例。
