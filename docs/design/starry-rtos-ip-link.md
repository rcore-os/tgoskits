# Starry/Linux 与 RTOS 客户机 IP 通信设计

## 1. 目标与范围

本设计在已有 AxVM FDT/PSCI 修复、AxVisor 双客户机 VirtIO-net 交换机和
`axvirtio-common`/VirtIO-MMIO 基础上，建立 Starry/Linux 客户机与 RTOS 客户机
之间可复现的双向 IP 通信链路。

主数据通道必须是 TCP/UDP/IP 或等价的 IP 传输。共享内存、HyperCall、裸
MMIO 和 vsock 不得承载业务数据；如将来使用 vsock，只能用于控制、调试或
对比实验，并在运行文档中明确隔离。

本设计的非目标包括：重写已有 VirtIO-net 设备、把 VirtIO-blk 变成通信通道、
实现物理网卡直通、提供通用服务发现，以及在一个变更中引入多个相互独立的
应用协议。

## 2. 既有基线和复用边界

后续变更以以下已有提交为共同基线：

| 基线 | 复用内容 |
| --- | --- |
| `f56b496fe` | AxVM 生成 guest FDT 时保留 PSCI conduit，保证客户机启动和中断路径稳定 |
| `547f3266a` | AxVisor `virtio-net` DeviceModel、MMIO transport、内部二层交换机、DMA/IRQ 路径 |
| `4c4c55cd2` | workspace 中的 VirtIO 公共基础和设备图集成；块设备不作为本通信链路 |

新实现不复制 VirtIO 队列、MMIO、DMA grant 或交换机逻辑。Starry/Linux 和
RTOS 端只通过标准网卡、IP、UDP/TCP socket 使用这些能力。

## 3. PR 分层

四个 PR 必须依次基于同一条包含上述基线的分支。若某一层只有少量改动，
将它并入相邻 PR，而不制造只有配置文件的独立 PR。

### PR1：客户机网络接入和拓扑

建议标题：`feat(axvisor): connect Starry and RTOS guests over existing virtio-net`

复用已有 `virtio-net` 模型，为 Starry/Linux 和一个 RTOS 客户机增加 VM 配置、
FDT 网卡节点、MAC/IP/路由初始化以及最小 echo 验证。默认采用进程内二层交换
机；不使用宿主桥接、NAT 或物理上联。拓扑表和地址分配必须随 PR 提交。

### PR2：应用层协议和两端程序

建议标题：`feat(net-protocol): add Starry and RTOS guest control protocol`

协议采用一个固定版本的二进制帧。帧头字段如下：

| 字段 | 语义 |
| --- | --- |
| magic/version | 识别协议并支持显式拒绝未知版本 |
| message_type/flags | 区分控制、状态、错误、心跳和确认 |
| header_len/payload_len | 防止截断、粘包和过长载荷 |
| sequence/timestamp | 请求关联、重复检测和延迟统计 |
| error_code | 传达协议或业务错误 |
| checksum | 检测帧损坏 |

两端必须实现 `CONTROL`、`STATUS`、`ERROR` 和 `HEARTBEAT`，并完成至少一次
Linux/Starry → RTOS 控制请求及 RTOS → Linux/Starry 状态响应。

### PR3：可靠性和异常恢复

建议标题：`feat(net-reliability): add guest link recovery and fault handling`

本设计首选 UDP，以便显式验证可靠性协议。`CONTROL` 和需要响应的 `STATUS`
使用序号、ACK、有限重传和超时；重复帧只确认一次且不能重复执行，乱序帧返回
错误。心跳负责检测链路状态，链路恢复后重新建立会话。

如果实现改用 TCP，必须使用同一协议头进行分帧，并实现连接/读写超时、断连检测、
自动重连、未完成请求处置和异常恢复。不能因为 TCP 自带可靠字节流而省略这些
应用层状态。

### PR4：端到端验证、指标和运行文档

建议标题：`test(net): validate Starry and RTOS guest communication`

该 PR 提供可复制的 QEMU/板卡流程、丢包和断链故障注入、pcap 或日志校验，
并输出以下指标：请求成功率、应用层错误、超时、重传/重连次数、恢复成功率、
请求-响应延迟（至少 P50/P95）和有效应用吞吐量。失败必须以非零退出码传播到
测试运行器。

## 4. 网络拓扑

默认 QEMU 拓扑为两个互不共享宿主网络的 VirtIO-MMIO 端点：

```text
Starry/Linux guest                         RTOS guest
  virtio-net0                                virtio-net0
  MAC 52:54:00:42:00:01                      MAC 52:54:00:42:00:02
  10.0.42.1/24                                10.0.42.2/24
          \                                  /
           AxVisor in-process L2 switch
```

业务端口由协议配置明确指定（默认 UDP `4242`）。两个 guest 在同一子网内直接
通信，不配置默认网关、NAT 或宿主桥；访问控制只允许对端 MAC/IP 和业务端口。
任何使用桥接、NAT、TAP 或物理网口的变体必须另列拓扑和安全边界，不能隐式改变
主测试路径。

## 5. 所有权、错误和安全边界

- AxVisor 继续拥有 VirtIO 设备、交换机端口和 guest DMA 授权。
- 客户机应用只拥有自己的 socket、协议会话和业务状态。
- 协议解码在执行控制动作前完成 magic、版本、长度、序号、错误码和 checksum
  校验。
- 资源耗尽、非法帧、超时、断连和不支持的消息必须显式报告，不能伪造成功或
  静默降级到共享内存/HyperCall。
- 重复控制请求必须可识别；状态机必须保证一次执行语义或明确返回重复错误。

## 6. 验收矩阵

| 能力 | 必需证据 |
| --- | --- |
| IP 主通道 | 两端网卡、ARP/IPv4/UDP 或 TCP 报文、非零业务收发 |
| 协议 | 编解码单测、版本/长度/校验错误测试、两端真实调用 |
| 双向业务 | 控制请求、状态响应、错误通知和心跳均有报文 |
| 可靠性 | 丢包/丢 ACK、重复、乱序、超时、断链和恢复测试 |
| 拓扑安全 | MAC/IP/路由/端口/桥接或 NAT/访问控制文档 |
| 指标 | 成功率、错误、超时、恢复、延迟和有效吞吐量报告 |

## 7. 运行与回滚

每个 PR 都应有最小可运行命令，并在当前提交记录目标架构、镜像准备、启动
命令和成功标志。PR1 可回滚到已有双 ArceOS VirtIO-net 测试；PR2/PR3 的协议
能力通过独立 feature 或应用入口接入，不修改已有 VirtIO-net 公共 ABI；PR4
只增加测试和文档时可以单独回滚，不影响客户机网络设备。
