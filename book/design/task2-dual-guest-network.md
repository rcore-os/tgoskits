# Task-2 双 Guest 网络数据面与可靠协议设计

## 问题与成功标准

任务二需要在 AxVisor 管理的 Linux Guest 与 RTOS Guest 之间建立可复算的
UDP/IP 数据面，并证明控制协议在丢包、重复、乱序、非法参数和链路故障下
不会把错误状态当作成功。当前 `apps/starry/qemu/dual-net` 只验证一个 Guest
内部的两张网卡，不能证明跨 Guest 的设备、DMA、IRQ 和协议边界。

本设计的成功标准分为三个层级：

| 层级 | 成功标准 | 证据 |
| --- | --- | --- |
| P1 单 Guest | 一个 `virtio-net-pci` 完成驱动 attach，静态 IP 可配置，UDP 探针能发收 | Guest 日志、MAC/IP、IRQ 计数、单侧 pcap |
| P2 双 Guest | Linux 与 RTOS 各自只看到被分配的网卡，双向 UDP 形成双侧镜像 | 设备集合对账、双侧 pcap、manifest、序号对账 |
| P3 可靠协议 | ACK、重传、去重、乱序拒绝、非法参数、heartbeat 和安全状态可复现 | 协议 crate 单测、端到端日志、QMP link down/up 记录 |

本次实现不把 PCI controller 粒度透传误认为 PCI function 隔离。当前配置选择器
只支持 FDT path；因此 P2 的运行命令必须先通过设备集合不相交检查。若 host FDT
无法提供每个 endpoint 的稳定身份，应停止在 P2 之前，转为实现 PCI BDF selector
或 AxVisor 虚拟网卡，而不能继续使用共享 `/pcie@...` 的配置。

## 方案比较

| 方案 | 优点 | 主要缺点 | 决策 |
| --- | --- | --- | --- |
| QEMU user socket + `virtio-net-pci` passthrough | 与 ArceOS PCI 驱动路径一致，pcap/QMP 证据直接 | 当前 FDT selector 可能只能选整棵 PCI controller；需要先证明 endpoint 隔离 | P1 单 Guest 已通过；P2 暂不采用 |
| `virtio-mmio` passthrough | FDT 节点可独立选择，设备图简单；当前 ArceOS FDT probe 和直接 QEMU 数据面已验证 | AxVisor passthrough 的 IRQ/DMA/GPA-HPA 仍需端到端证明 | 当前 P2 探路路线 |
| AxVisor emulated virtio-net | Guest 隔离和 DMA 语义可完全由 AxVisor 控制 | 新增虚拟设备模型、后端队列和 host socket，工作量最大 | 若 PCI function 隔离不可实现，再进入该方案 |

## 协议边界

`components/task2-net-protocol` 是不依赖 socket、时钟或 Guest runtime 的 `no_std`
协议核心。网络适配层只负责把它产生的字节发送到 UDP，把收到的字节交回状态机。

### Frame 格式

所有整数使用网络字节序。固定头为 28 字节：

| 偏移 | 长度 | 字段 |
| ---: | ---: | --- |
| 0 | 4 | magic `T2N1` |
| 4 | 1 | version `1` |
| 5 | 1 | message kind |
| 6 | 2 | flags；当前仅 `RELIABLE=1` |
| 8 | 4 | session id |
| 12 | 4 | sequence；可靠帧非零 |
| 16 | 4 | acknowledgement；ACK/ERROR 使用 |
| 20 | 2 | payload length，最大 1200 |
| 22 | 2 | error code |
| 24 | 4 | CRC32-IEEE，计算时该字段视为零 |
| 28 | N | payload |

可靠消息使用 stop-and-wait：同一 endpoint 同时最多一个待确认帧。接收端只接受
`expected_seq`，重复的上一帧只重发 ACK，不再次交付应用；提前到达的序号回复
`OUT_OF_ORDER` 并进入安全状态。

### Typed payload

- `CONTROL`：12 字节，动作、参数和 request id；`SetOutput` 参数范围为 0–1000，
  `Stop`/`Reset` 参数必须为零。
- `STATUS`：12 字节，状态、标志、值和最后处理的 request id。
- `HEARTBEAT`：8 字节 monotonic uptime。
- `ERROR`：error code 加可选诊断字节，不伪造成功。

### 状态和计时

- 重传计时器到期时最多重传 `max_retries` 次；耗尽后清除 pending 并进入 `Safe`。
- `peer_timeout` 超过最后有效接收时间后进入 `Safe`。
- 合法 heartbeat 或合法应用帧恢复 `Active`；解析失败的 datagram 不刷新 liveness。
- 非法 typed payload 发送 `ERROR/INVALID_PARAMETER`，保持原序号未交付，并进入 `Safe`。

## 运行时适配和证据

P1/P2 脚本放在 `scripts/test/net-dual-guest/`，不修改任务一 QEMU 配置。QEMU 使用
两个 virtio-MMIO socket netdev 形成点到点链路、每侧一个 `filter-dump`，并通过独立 QMP UNIX
socket 执行 `set_link ... off/on`。所有路径从 `${workspace}` 展开，产物目录、命令、
镜像哈希、MAC/IP/端口和设备选择写入 manifest。

QMP 控制连接断开和 `set_link` 链路 down 是两个不同实验：前者只验证控制面可恢复，
后者才验证 heartbeat 超时、协议安全状态和重新建立数据面。

最终 P2 拓扑使用静态 IPv4 `/24`：Linux `10.0.42.15:4242`、RTOS
`10.0.42.2:4242`，固定 MAC 分别为 `52:54:00:12:34:01` 和
`52:54:00:12:34:02`。两个独立 virtio-net MMIO endpoint 通过 QEMU socket
`127.0.0.1:12781` 点到点连接；不使用桥接、NAT、hostfwd 或 vsock，也没有额外的
防火墙规则。访问边界由独占的点到点 endpoint、UDP 4242、session ID 和 CRC/字段
校验共同形成；应用发送目标固定为 peer IP，当前接收路径不单独拒绝其他源 IP。
QMP UNIX socket 仅用于故障注入和退出，不承载应用数据。

路由采用最小的 on-link connected route：Linux 在 `eth0` 上配置
`10.0.42.15/24`，Zephyr 在 VirtIO 网络接口上配置 `10.0.42.2/24`；因此两端的
`10.0.42.0/24` 路由由接口前缀直接生成，不设置网关，也不依赖默认路由。Linux 的
init 脚本使用 `ifconfig eth0 10.0.42.15 netmask 255.255.255.0 up`，Zephyr 使用
同样的 `/24` netmask 和静态 peer 配置。

### 双向传输验收表

下表来自 clean worktree 的一小时 P2 运行；两侧 pcap 是同一条链路的镜像捕获，
因此每一行都能在 Linux 和 RTOS 两份 pcap 中交叉复算。

| 方向 | T2N1 类型 | 期望语义 | clean 长稳观测 | 证据 |
|---|---|---|---:|---|
| Linux → RTOS | CONTROL | 控制请求 `seq=1` | 1 | Guest 日志 `TASK2_CONTROL_SENT`；pcap kind=1 |
| RTOS → Linux | ACK | 确认 CONTROL | 1 | `TASK2_ACK seq=1`；pcap kind=4 |
| RTOS → Linux | STATUS | 控制执行状态回传 | 1 | `TASK2_STATUS_RECEIVED`；pcap kind=2 |
| Linux → RTOS | ACK | 确认 STATUS | 1 | `TASK2_ACK seq=1`；pcap kind=4 |
| Linux → RTOS | HEARTBEAT | 双向存活检测 | 17,926 | pcap kind=5、序号账本一致 |
| RTOS → Linux | HEARTBEAT | 双向存活检测 | 17,923 | pcap kind=5、序号账本一致 |

合计每份 pcap 为 36,123 个 Ethernet 包、35,853 个 T2N1 UDP 帧；两份 pcap 的
方向/类型/序号签名完全相同。CONTROL→ACK 延迟为 18.542 ms，CONTROL→STATUS
延迟为 21.034 ms；运行期间没有 `TASK2_SAFE`、协议错误或发送错误事件。

### 网络实现细节总表

| 层次 | Linux/controller Guest | RTOS/managed Guest | 连接或隔离含义 | 实际证据 |
|---|---|---|---|---|
| Guest 软件 | Linux kernel + `/bin/task2-net` | Zephyr + `zephyr-task2` | 两端运行独立协议端点 | 两侧均输出 `TASK2_READY` |
| Guest VM | VM[1] | VM[2] | 独立 vCPU、内存和 stage-2 页表 | AxVisor 创建 VM[1]/VM[2] 成功 |
| Guest 网卡身份 | MAC `52:54:00:12:34:01` | MAC `52:54:00:12:34:02` | 二层身份不重复 | manifest + 双侧 pcap |
| Guest IPv4 | `10.0.42.15/24` | `10.0.42.2/24` | `10.0.42.0/24` 为 on-link connected route | Linux `ifconfig`、Zephyr netmask 配置 |
| 应用 UDP | 绑定 `0.0.0.0:4242`，发送到 `10.0.42.2:4242` | 绑定 `10.0.42.2:4242`，发送到 `10.0.42.15:4242` | 应用主通道是 UDP/IPv4 | pcap UDP port=4242 |
| Guest FDT 设备 | `/virtio_mmio@a003e00` | `/virtio_mmio@a003c00` | 每个 Guest 只选择一个 VirtIO-MMIO endpoint | `verify_fdt_devices.py` PASS |
| Host MMIO | HPA `0x0a003e00`, size `0x200` | HPA `0x0a003c00`, size `0x200` | 两个设备寄存器窗口不重叠 | manifest + host DTB |
| Guest MMIO | GPA `0x0a003e00` | GPA `0x0a003c00` | GPA=HPA identity passthrough；两个地址位于同一数值页 `0x0a003000..0x0a004000`，但分别安装在 VM[1]/VM[2] 的独立 stage-2 页表中 | `verify_isolation.py` 按 VM ID 分别验证 |
| Host/Guest IRQ | FDT SPI cell/hwirq `47`；host INTID `79` → guest INTID `79` | FDT SPI cell/hwirq `46`；host INTID `78` → guest INTID `78` | route 属于对应 VM，非共享 IRQ | route registration 日志 + isolation verifier |
| Guest 内存 | GPA/HPA `0x80000000..0xa0000000` | GPA/HPA `0xa0000000..0xc0000000` | 两个 512 MiB identity carveout 不相交 | AxVisor memory-map 日志 |
| QEMU 数据链路 | `net-linux` listen `127.0.0.1:12721`（短跑）/ `12781`（长稳） | `net-rtos` connect 到同一 socket | 仅作为 Ethernet backend 连接，不是应用协议 | QEMU TOML + 双侧 pcap |
| 管理通道 | QMP UNIX socket | QMP UNIX socket | 仅执行 `set_link`/`quit`，不承载 T2N1 | `qmp_link.py` 返回值 |
| 桥接/NAT/防火墙 | 无 | 无 | 点到点 socket 不依赖 host bridge、NAT 或 hostfwd | 设计文档和 QEMU 参数审计 |

### 双向数据路径表

| 层级/步骤 | Linux → RTOS：CONTROL 路径 | RTOS → Linux：STATUS 路径 | 需要保持的边界 | 验证方式 |
|---:|---|---|---|---|
| 1. 应用编码 | Linux 编码 CONTROL：kind=1、seq=1、request=1、value=100 | RTOS 执行 CONTROL 后编码 STATUS：kind=2、seq=1、request=1、state=Active | 两端使用同一 T2N1 v1 wire format | frame/payload 单元测试 |
| 2. UDP/IP | `10.0.42.15:4242` → `10.0.42.2:4242` | `10.0.42.2:4242` → `10.0.42.15:4242` | 应用数据必须封装在 UDP/IPv4 中 | pcap IPv4 protocol=17、UDP port=4242 |
| 3. Guest VirtIO | Linux 驱动提交 VM[1] VirtIO TX descriptor | Zephyr 驱动提交 VM[2] VirtIO TX descriptor | DMA 仅引用本 Guest carveout | identity GPA/HPA evidence |
| 4. MMIO/IRQ | VM[1] 使用 `0x0a003e00` / INTID 79 | VM[2] 使用 `0x0a003c00` / INTID 78 | MMIO endpoint 和 IRQ route 按 VM 独立拥有 | stage-2 + route registration 日志 |
| 5. QEMU backend | `net-linux` 把 Ethernet frame 写入 host-local socket | `net-rtos` 沿同一 socket 反向写回 Ethernet frame | socket backend 只搬运 Ethernet frame，不绕过 Guest IP 栈 | 两个 `filter-dump` pcap |
| 6. 接收校验 | RTOS 校验 magic/version/length/session/seq/CRC 后交付 CONTROL | Linux 做相同校验后交付 STATUS | 重复不重复交付；乱序或非法 payload 显式报错 | protocol tests + Guest event logs |
| 7. 可靠确认 | RTOS → Linux 发送 ACK，acknowledgement=CONTROL seq=1 | Linux → RTOS 发送 ACK，acknowledgement=STATUS seq=1 | CONTROL、STATUS 各自完成一次 stop-and-wait | pcap 中 kind=4 各方向 1 个 |
| 8. 存活检测 | Linux → RTOS heartbeat 17,926 个 | RTOS → Linux heartbeat 17,923 个 | heartbeat 不覆盖 pending 重传计时器 | 一小时 pcap + 0 unexpected timeout |

### 协议帧与运行证据对照表

| T2N1 kind | 业务含义 | 可靠性 | 关键字段 | clean 长稳观测 | 失败时的可观察结果 |
|---:|---|---|---|---:|---|
| 1 | CONTROL / `SetOutput(100, request=1)` | ACK + 超时重传 | seq=1、payload length=12、CRC32 | 1 | `TASK2_RETRANSMIT` 或 `TASK2_SAFE RetryExhausted` |
| 2 | STATUS / `Active` | ACK + 超时重传 | seq=1、last request=1 | 1 | 远端 `TASK2_REMOTE_ERROR` 或 Safe |
| 3 | ERROR / 协议或参数错误通知 | 由触发帧关联 | error code、acknowledgement、CRC32 | 0（正常长稳） | `TASK2_PROTOCOL_ERROR` / `TASK2_REMOTE_ERROR` |
| 4 | ACK | 无需再次确认 | acknowledgement=被确认序号 | 2 | ACK-drop proxy 可观察重传和 duplicate ACK |
| 5 | HEARTBEAT | 周期发送、peer timeout | uptime timestamp、CRC32 | 35,849 | `TASK2_SAFE HeartbeatTimeout`，恢复后 `TASK2_RECOVERED` |

### 细节表对应的验收命令

| 验收目标 | 命令 | 通过条件 |
|---|---|---|
| 协议字段和状态机 | `cargo test -p task2-net-protocol` | 19 个测试通过 |
| Python 解析器 | `python3 -m unittest discover -s scripts/test/net-dual-guest -p 'test_*.py'` | 10 个测试通过 |
| 设备集合 | `verify_fdt_devices.py manifest.toml qemu-p2-host-carved.dtb` | 两个 endpoint 不相交 |
| 双向序号账本 | `verify_pcap.py linux-p2-stability-1h.pcap rtos-p2-stability-1h.pcap --require-task2` | 两侧签名完全一致，ACK 率达标 |
| GPA/HPA、MMIO、IRQ、DMA | `verify_isolation.py manifest.toml task2-clean-p2-stability-1h.log` | identity map、stage-2、SPI route 全部可解析 |
| 长稳指标 | 一小时 QEMU/QMP 运行记录 | ≥3600 s，错误/意外超时为 0，延迟和吞吐量可计算 |

## P2 阻塞根因与当前状态（2026-08-11）

早期 Linux + Zephyr 双 Guest 运行中，Zephyr overlay 使用随机 MAC，后来已固定为
`52:54:00:12:34:02`。固定 MAC 后仍只有 Linux 的 SPI 79 能完成 gate/ack/queue，
Zephyr 的 SPI 78 没有进入 host delivery，导致 RX descriptors 不回收。

根因不是 AxVisor 的物理 SPI 状态机，而是 Zephyr 使用了 Secure GIC 视图：`qemu_cortex_a53`
构建缺少 `CONFIG_ARMV8_A_NS=y`，因此只启用 `GICD_CTLR.Group 1 Secure`（bit2）；
AxVisor 的 passthrough VGIC 按 Non-secure 视图只识别 Group 1 Non-secure（bit1）。
在 `scripts/test/net-dual-guest/zephyr-task2/prj.conf` 加入该配置后，SPI 78 已出现
`acknowledged → queue → loaded into LR`，并完成双向 CONTROL/ACK/STATUS/HEARTBEAT。

当前已验证：

- 基线双 Guest 短跑双侧 pcap 各有 260 个 T2N1 UDP 帧，`verify_pcap.py --require-task2`
  通过；最终长稳结果见下文；
- host DTB 中两个 VirtIO-MMIO endpoint 不相交；
- AxVisor debug 日志确认 VM[1]/VM[2] 的 GPA→HPA identity memory、独立 stage-2
  MMIO 映射和 SPI 79/78 route；
- `verify_isolation.py` 的 `dma_evidence = "identity-map"` 模式通过。Linux+Zephyr
  不提供 ArceOS `TASK2_DMA` 日志，因此该模式要求运行时 identity mapping、MMIO
  stage-2 和 IRQ route 全部可见，不把缺少 descriptor 日志默认为成功。

长稳期间还发现并修复了两个独立的计时问题：协议核心的可靠重传路径错误刷新
heartbeat 发送计时器，导致 pending STATUS 时心跳被压制；Zephyr 端点原先以 5 秒
周期发送 heartbeat，恰好触及 Linux 的 5 秒 peer timeout。前者在
`components/task2-net-protocol/src/session.rs` 增加了确定性回归测试，后者将
`scripts/test/net-dual-guest/zephyr-task2/src/main.c` 的发送周期对齐到 200ms（接收
超时仍为 10 秒）。

2026-08-11 的最终长稳使用 Debug AxVisor 日志、独立 QMP socket 和双侧 pcap，实际
运行 3849.75 秒（约 64 分钟）：

- 两侧各 37,450 个 Ethernet 包、37,220 个 UDP/T2N1 帧；其中 37,216 个 heartbeat，
  CONTROL/ACK/STATUS 初始交换完整；双向帧分别为 18,611 / 18,609，pcap 序号账本
  完全一致，`verify_pcap.py --min-ack-rate 99 --require-task2` 通过；
- Debug 日志中 `TASK2_SAFE`、`TASK2_RECOVERED`、`TASK2_PROTOCOL_ERROR` 和
  `TASK2_SEND_ERROR` 均为 0；
- `verify_isolation.py` 通过，确认两 VM 的 identity GPA/HPA、独立 stage-2 MMIO、
  SPI route 和 DMA 证据均成立；
- 产物哈希：AxVisor 日志
  `375a22204541d94be9ddd6db611bc1b2c4686844011c9aaf4a18f2008631111a`，Linux pcap
  `1deabda7c5d163f2090240826c82fdfafb31334274373134af73f87ed5892c0e`，RTOS pcap
  `82867eb77487e01e17e86c18e3f3a6bb654fde72f1d037882e5f88204089bfaf`。

P2 长稳验收已完成；P3 的丢 ACK、重复、乱序和链路 down/up 证据仍以各自独立运行
记录为准。

## 失败和回滚

设备集合不相交检查失败时，运行器必须在启动 Guest 前失败；不得使用 timeout、
共享 PCI controller 或完整 host DTB 来掩盖隔离缺失。协议不兼容、CRC 错误、非法
参数和重传耗尽都必须产生显式事件，不能静默丢弃后继续报告 PASS。
