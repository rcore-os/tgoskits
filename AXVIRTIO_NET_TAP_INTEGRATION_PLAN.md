# AxVisor virtio-net 完整 TAP 上行集成计划

> 本文承接 [`AXVIRTIO_NET_AXVISOR_INTEGRATION_PLAN.md`](./AXVIRTIO_NET_AXVISOR_INTEGRATION_PLAN.md)，
> 依据当前实现评审和实际 QEMU 烟测结果制定。前一份计划解决“客户机到 AxVisor
> 确定性虚拟 peer”的闭环；本文解决“客户机经 AxVisor、QEMU TAP 接入真实二层网络并
> 访问外部网络”的完整路径。

## 1. 目标、范围与完成标准

目标拓扑是：

```text
ArceOS guest
  virtio-mmio frontend
    -> AxVisor axvirtio-net emulated device
      -> AxVisor raw Ethernet bridge runtime
        -> AxVisor host virtio-net frontend
          -> QEMU -netdev tap
            -> Linux TAP / bridge / DHCP / NAT / physical uplink
```

这里的 TAP 由外层 Linux 和 QEMU 创建、持有。AxVisor 是运行在 QEMU 中的裸机
hypervisor，不能按 Linux 进程模型直接打开 `/dev/net/tun`。因此 AxVisor 内部实现应命名
为 `RawUplinkBackend`、`EthernetBridgeBackend` 或 `VirtioNetBridgeRuntime`，不应把一个
从未打开 TAP fd 的组件称为 `TapBackend`。

完整验收必须同时满足：

1. AxVisor 能按明确身份独占 QEMU 暴露的上行 virtio-net，不把它交给自身 `ax_net`；
2. guest TX 帧能经上行口到达 Linux TAP，TAP RX 帧能进入 guest RX virtqueue；
3. guest 能通过 DHCP 获得地址、默认路由和 DNS，或在网络初始化前应用一份完整静态配置；
4. guest 能完成 ARP、DNS 查询、TCP 建连和一次 HTTP/HTTPS 请求；
5. VM stop、reset、remove、stopped-start 和失败回滚不会遗留 worker、IRQ handler、DMA
   buffer 或上一代帧；
6. 队列拥塞、无 guest RX buffer、链路断开和畸形超长帧都可观测且不会忙等或无限增长内存。

访问 `https://www.baidu.com/` 可作为人工外网验收项，但不能作为唯一 CI 门禁。CI 应使用
可控的 DHCP/DNS/HTTP 服务或仓库内测试服务，以区分实现回归和第三方网络波动。

首版不要求多队列、VLAN trunk、网卡 offload、零拷贝、客户机到客户机交换或 AxVisor
自身与 guest 共享同一上行 IP 栈。首版必须实现可靠的单队列双向二层转发。

## 2. 当前实现评审结论与 TAP 前置门槛

当前实现已经能枚举模拟 virtio-net、注册 IRQ 和读取正确 MAC，但实际运行停在
`eth0: DHCP enabled`，没有出现 `UDP_ECHO_PASS`，因此尚未完成上一阶段，更没有外网
能力。进入 TAP 开发前先逐项修复以下问题；每个逻辑缺陷都先添加在旧实现上必然失败的
确定性回归测试，再实现修复。

### 2.1 guest memory 连续长度仍可能被高估

`AddrSpace::translate_and_get_limit` 对 alloc-backed area 返回到 area 末尾的长度，但
area 内物理页可能不连续。正确上限必须为：

```text
min(area_remaining, current_translation_block_or_page_remaining)
```

增加跨页且下一物理页不连续的读写测试，确认 virtqueue descriptor copy 不会把当前 HPA
当作整段连续 HVA 使用。此项未修复前禁止转发来自真实网络的任意长度帧。

### 2.2 确定性 UDP smoke 必须先跑通

当前 guest app 在 `axruntime` 已按默认 DHCP 初始化网络之后才设置静态 IP，配置时序无效。
应选择一种明确方式：

- 确定性 peer 测试通过构建配置或 `axruntime` 初始化输入，在 `init_network` 前提供静态
  IP、前缀和路由；
- TAP 测试保持 DHCP，并由外层网络提供 DHCP/DNS。

不得让应用在网络栈启动后修改首个接口来掩盖配置问题。先取得可重复的
`UDP_ECHO_PASS <token>`，再开始真实上行转发。

### 2.3 先恢复基础测试可编译状态

修复 `virtualization/axvm/src/boot/fdt/core/tree.rs` 中 `Option<Phandle>` 与整数比较造成的
`cargo test -p axvm` 编译失败。TAP 分支不能建立在基础单测无法运行的状态上。

### 2.4 worker 生命周期必须闭合

- stopped VM 再次 `start_vm` 时要按新的 generation 重建 worker；不能只启动 VM 而不
  恢复 RX 路径；
- worker registry 保存真正的 task/join handle，不能只依赖 worker 尾部设置
  `finished`；worker panic 也必须被 join 或转换成终止状态，避免 stop 永久等待；
- stop/reset/remove/prepare 失败按“停止接收新工作 -> cancel -> wake -> join -> 丢弃
  queue/device”顺序清理；旧 generation 的 worker 和 IRQ sink 必须拒绝继续工作。

### 2.5 FDT 生成、补丁和冲突检测必须一致

- 资源冲突检查不仅比较本次新增节点，还要解析实际 guest DTB 的现有 `reg` 和 IRQ；
- generated-DTB 和 supplied-DTB 两条路径都必须注入或验证 virtio-mmio 节点；
- 不使用 `passthrough_devices = [["/"]]` 作为烟测捷径。它会让外层 host NIC、客户机
  模拟 NIC 和其他 host 设备的身份及隔离关系不明确；
- 显式保留 guest 所需的 CPU、memory、timer、GIC、console 等节点，显式排除 AxVisor
  独占的上行 NIC；
- guest 镜像和 DTB 使用 `${workspace}` 或构建产物解析，不写开发机绝对路径。

### 2.6 virtio 通知语义必须正确

当前 RX worker 在每次 `RxOutcome::Delivered` 后无条件 pulse IRQ。应由设备层根据 used
ring 更新、guest notification suppression、event index 和 interrupt status 决定是否
需要通知。runtime 只能消费“需要注入 IRQ”的显式结果，不能自行推断。

### 2.7 确定性 backend 仍需补齐协议验证

在把它保留为回归 oracle 前，补齐目标 MAC、guest 源 IP、IPv4 fragment、UDP length 和
UDP checksum 校验，并增加 backend 单测。测试配置按 guest MAC 选择接口，不使用“第一
个 Ethernet 接口”。完成后保留 echo backend 作为无宿主网络依赖的最低层测试模式。

以上门槛全部通过的退出条件是：`axaddrspace`/`axvm` 测试通过，确定性 UDP 首次启动和
reset 后各通过一次，worker registry 为空，且 IRQ 抑制测试证明不会多注入中断。

## 3. 边界设计

### 3.1 保持四层职责分离

- **Driver Core**：`drivers/ax-driver` 的 host virtio-net frontend 继续负责寄存器、DMA、
  descriptor 和 queue completion；不依赖 AxVisor VM 类型。
- **Capability Boundary**：复用 `rd-net`/`rdif-eth` 的 `Net`、`TxQueue`、`RxQueue` 和
  owned `IrqHandler`。如现有 API 缺少错误可见性或 readiness 能力，只做最小、通用的
  capability 扩展，不加入 VM、TAP 或线程概念。
- **AxVisor OS Glue**：按配置从 `PlatformNetDevice` 取得指定 NIC，解析 IRQ，构造 DMA
  queue，并把 owned IRQ handler 移入 IRQ callback。
- **AxVisor Runtime**：拥有双向 bridge worker、bounded frame queue、cancel/join、
  generation 和统计信息；它连接 host queue endpoint 与模拟 guest device endpoint。

透明桥接数据帧不经过 AxVisor 自身 TCP/IP 栈。把帧送入 `ax_net` 再路由会改变二层语义，
还会引入 IP、ARP、DHCP 和端口所有权冲突。

### 3.2 上行 NIC 的选择与所有权

新增显式配置，例如 `host_uplink`，至少包含稳定的 device selector、预期 MAC、MTU 和
queue 数。QEMU 配置为外层 virtio-net 设置固定且与 guest MAC 不同的 MAC。初始化时：

1. 完成设备 probe，但在 `axruntime::init_net` 消费设备列表之前选择上行 NIC；
2. selector 和预期 MAC 必须唯一匹配，零个或多个匹配都返回错误；
3. 通过现有 `take_rd_net_device` 取得 `rd_net::Net` 的唯一所有权；
4. 创建一个 TX queue、一个 RX queue并取得 owned IRQ handler；
5. 已被 bridge 取得的设备不得再次进入 `ax_net::init_network`。

若 AxVisor 不需要自身 TCP/IP，优先在该构建 profile 中关闭 `ax-net` 消费路径并显式 claim
上行设备；若仍需管理网络，则给 `collect_net_devices` 增加明确的 reservation/selection
策略，不能依赖枚举顺序。

### 3.3 backend 与 runtime 解耦

解除 `VirtioNetDeviceAdapter`、factory 和 worker 对
`DeterministicUdpEchoBackend` 具体类型的绑定。设备侧仍只依赖 `NetworkBackend` 的同步、
非阻塞发送语义；模式由 AxVisor 配置选择：

- `DeterministicPeer`：保留当前有界 echo backend，用于 CI 和协议回归；
- `RawUplink`：guest TX 只写入 bounded outbound queue 并唤醒 host TX worker；
- 后续模式必须通过新的配置 enum 和独立 adapter 加入，不扩展布尔参数集合。

`NetworkBackend::transmit` 可能在 guest virtqueue lock 内被调用，禁止在其中等待 host TX、
调用 `receive_frame`、注入 IRQ或取得 VM 生命周期锁。

## 4. 双向数据面

### 4.1 guest TX 到 Linux TAP

```text
guest TX notify
  -> axvirtio-net validates descriptor chain and copies Ethernet frame
  -> RawUplinkBackend::transmit enqueues bounded frame
  -> host TX worker wakes
  -> TxQueue::prepare_send copies into DMA buffer
  -> TxPending::try_submit
  -> QEMU virtio-net
  -> Linux TAP
```

- 首版允许一次有界 copy，先保证所有权和 backpressure 正确；零拷贝不属于首版；
- 帧长必须在 Ethernet header 下限和双方 buffer/MTU 上限内，FCS 不包含在帧数据中；
- outbound queue 满时采用明确的 drop/backpressure policy并计数，不能无限分配；
- `NetError::Retry` 只在 TX completion/readiness 到来后重试，不使用无界 `yield_now` 循环；
- worker 定期 reclaim completed TX buffer，link-down 和永久错误进入可诊断状态。

### 4.2 Linux TAP 到 guest RX

```text
Linux TAP
  -> QEMU virtio-net
  -> host NIC IRQ publishes queue event
  -> host RX worker calls RxQueue::try_receive/consume
  -> bounded inbound queue or direct deferred delivery
  -> emulated device receive_frame
  -> device reports whether guest IRQ is required
  -> VM-local queued IRQ sink wakes target vCPU
```

- IRQ top half 只调用 owned `IrqHandler::handle_irq` 并发布 queue-local readiness；packet copy、
  DMA reclaim、descriptor refill、waker 和 guest 注入都在 task/deferred context；
- RX DMA buffer 在 `consume` 完成后及时回填 host ring；不得让 guest 无 RX buffer 长期耗尽
  host RX ring；
- `NoGuestBuffer` 时把帧放入有界 pending queue，等待 guest RX kick/readiness 后重试；超过
  数量或时间预算后 drop 并计数；
- 广播、ARP、IPv4/IPv6 multicast 默认透明传递。首版不解析 L3/L4，也不修改 checksum；
- QEMU host NIC 和 guest NIC 使用不同 MAC 时，Linux TAP 侧实际看到的是 guest MAC。
  QEMU virtio-net 必须允许这些源/目的帧；若设备过滤导致丢包，应显式启用合适的
  promiscuous/all-multicast 能力或配置，不得静默改写 MAC。

### 4.3 并发和锁顺序

运行时拆分为 guest TX producer、host TX worker、host IRQ endpoint、host RX worker 和
guest RX endpoint。各方向使用独立 bounded queue 和 event。约束如下：

- IRQ callback 不取得 guest device/VM/host queue 的广锁；
- 持有 guest virtqueue lock 时不调用可能阻塞、唤醒或重入设备的代码；
- 持有 host queue endpoint 时不调用 VM stop/reset 编排；
- cancel 和 generation 使用 Acquire/Release 发布与观察；纯统计计数才使用 Relaxed；
- 文档化唯一锁顺序，并用 fake endpoints 做“callback 不在锁内重入”的测试。

## 5. 生命周期、错误与可观测性

每个 VM generation 拥有一个 `VirtioNetBridgeRuntime`，内部至少保存：

- guest device/runtime endpoint；
- host TX/RX queue endpoint；
- cancel token、readiness event 和 task handles；
- inbound/outbound queue capacity；
- generation、link state 和 typed terminal error；
- RX/TX packet、byte、drop、retry、oversize、no-buffer、IRQ 等 counters。

启动顺序为“claim uplink -> 建 queue/IRQ -> prepare/register VM device -> 注册 host IRQ ->
启动 worker -> enable IRQ”。任一步失败都逆序回滚。终止顺序先阻止新帧和屏蔽 IRQ，再
cancel/wake/join worker，最后释放 queues/device。panic 必须由 task handle 观察并转为错误，
不能依赖 worker 正常执行尾部标志。

日志只记录状态转换和聚合计数，不逐包刷屏。诊断输出至少能区分：host RX 未到达、guest
无 buffer、guest IRQ 被抑制、TX ring 满、link down、generation 过期和 host 配置错误。

## 6. 外层 QEMU 与 Linux TAP

### 6.1 QEMU 专用配置

新增 AxVisor AArch64 TAP 专用 QEMU config，不修改默认无网络配置。核心参数形态为：

```text
-device virtio-net-device,netdev=axv_uplink,mac=<host-uplink-mac>
-netdev tap,id=axv_uplink,ifname=<tap-name>,script=no,downscript=no
```

具体 machine bus 和 virtio transport 以 AxVisor 当前 AArch64 probe 能力为准；MMIO
`virtio-net-device` 优先于尚未验证的 PCI 路径。TAP 名称通过脚本参数或环境生成，不把开发
机接口名写死在 TOML。另提供同一 host virtio-net 数据面的 `-netdev user` 配置，用于不需
`CAP_NET_ADMIN` 的 CI/开发烟测；它验证 bridge runtime，但不替代真实 TAP 验收。

### 6.2 推荐的 NAT 拓扑

默认采用隔离的私有 bridge + NAT，而不是把 guest 直接接入管理 LAN：

```text
guest DHCP address: 10.88.0.0/24
bridge gateway:      10.88.0.1
TAP:                 enslaved to dedicated bridge
DHCP/DNS:            dedicated dnsmasq instance
uplink:              caller显式传入的 Linux interface
NAT:                 dedicated nftables table/chain with masquerade
```

bridge 本身不提供互联网连接。setup 必须同时配置 IP forwarding、DHCP/DNS 和 NAT；否则
guest 最多只能访问同一 bridge。不要复用 QEMU user networking 常见的 `10.0.2.0/24`，
避免路由和预期混淆。

host tooling 要求：

- 检查 `/dev/net/tun`、`ip`、`nft`（或受控的 iptables fallback）、`dnsmasq` 以及
  root/`CAP_NET_ADMIN`；
- setup/teardown 幂等，资源名带测试前缀，重复执行不会叠加规则；
- 保存并只恢复自己修改的 `ip_forward` 状态和 nftables table，不 flush host firewall；
- 不猜默认物理网卡，要求参数传入并验证其存在、UP 且有默认路由；
- 对信号和 QEMU 异常退出设置 cleanup trap；
- 支持 `--dry-run` 或状态检查，打印最终 TAP、bridge、subnet、uplink 和规则句柄；
- 复用现有脚本中的有效思路，但不要沿用删除同名 bridge、全局关闭 forwarding 或
  `brctl`/`ifconfig` 的破坏性行为，统一使用 `ip` 和 scoped firewall rules。

直连物理 bridge 可作为显式高级模式，但必须说明会把 guest 二层暴露到真实 LAN，并要求
上游允许多个源 MAC、DHCP 和安全策略；它不是默认验收拓扑。

## 7. guest 网络与外网验收应用

TAP/NAT 模式默认让 ArceOS 在 `axruntime` 初始化阶段启用 DHCP。验收应用在创建 socket
前等待有限时间，并验证：

1. 指定 guest MAC 对应接口处于 UP；
2. 获得非零 IPv4、正确前缀、默认路由和 DNS server；
3. DHCP 超时会输出明确失败原因并退出，而不是永久等待；
4. DNS 查询返回至少一个地址；
5. TCP connect 和 HTTP request 有独立超时；
6. HTTPS 验收同时验证系统时间来源、SNI、证书链和响应状态，不能为“能访问”而关闭证书
   校验。

若当前 ArceOS 网络栈尚无 DNS、TLS 或可信时间能力，分开记录为上层缺口：先用 host
可控的明文 HTTP 服务证明完整 TAP/TCP 数据面，再补 DNS/TLS/time 后宣称完成外网 HTTPS。
不得把 TCP 连接失败一律归因于 TAP。

建议结果标记：

```text
TAP_DHCP_PASS <ip> <gateway> <dns>
TAP_DNS_PASS <name> <address>
TAP_TCP_PASS <address> <port>
TAP_HTTP_PASS <status> <token>
TAP_HTTPS_PASS <host> <status>
```

人工百度验收使用域名而非固定 IP，记录 DNS、TCP/TLS 和 HTTP status 各阶段结果。若站点
策略拒绝自动客户端，能正确完成 DNS/TLS 且收到明确 HTTP 响应仍可证明网络路径，不能
通过放宽 TLS 校验换取 PASS。

## 8. 分阶段实施与退出条件

### 阶段 0：清偿当前实现缺陷

完成第 2 节全部项目，恢复 `axvm` 测试，确定性 UDP 首启和 reset 均通过。

### 阶段 1：冻结 host uplink 配置与设备所有权

定义 typed config/selector，给 QEMU host NIC 固定 MAC，实现 AxVisor 显式 claim，并证明
同一设备不会进入 `ax_net`。用两个 fake NIC 覆盖零匹配、多匹配和选择正确设备。

### 阶段 2：建立 raw Ethernet capability adapter

基于 `rd_net::Net` 建立单 TX/RX queue，owned IRQ handler 移入 callback。fake queue 测试
覆盖 DMA buffer 回收、RX refill、`Retry`、link down、oversize 和 IRQ readiness。

### 阶段 3：解除具体 echo backend 绑定

引入显式 backend mode 和 bridge runtime endpoint。验证 guest TX callback 只入 bounded
queue、不阻塞、不重入；保留确定性 peer 测试全部通过。

### 阶段 4：完成双向 bridge worker

实现 guest TX -> host TX 与 host RX -> guest RX，加入 backpressure、guest RX readiness、
正确 IRQ decision、task handle、generation 和 counters。fake raw port 端到端交换带 token
帧，覆盖队列满与 cancel/join。

### 阶段 5：修正 DTB 和 QEMU 双网卡身份

外层 DTB 的 host NIC 只归 AxVisor，guest DTB 只暴露 emulated NIC。移除根节点广泛透传，
generated/supplied DTB 测试均通过。先用 QEMU `-netdev user` 证明 DHCP/DNS/TCP 数据面。

### 阶段 6：实现隔离 TAP/NAT tooling

新增幂等 setup/status/teardown 和专用 QEMU config。用 `tcpdump -i <tap>` 验证双向 ARP、
DHCP 和 TCP；异常退出后确认无遗留 TAP、dnsmasq 进程和 firewall table。

### 阶段 7：完整功能和生命周期验收

依次通过 DHCP、gateway、DNS、TCP、HTTP/HTTPS。随后执行 VM stop/start 和 reset，各重复
一轮；最终 worker/queue/counter generation 正确，Linux host 资源清理干净。

任何阶段只看到 probe、IRQ 或单向包都不能越过对应退出条件。

## 9. 验证矩阵

### 9.1 host 单元与集成测试

- alloc-backed 不连续物理页 guest copy；
- FDT existing/new node 的 MMIO、IRQ 和 phandle 冲突；
- deterministic peer 的 MAC/IP/fragment/length/checksum；
- fake host port 双向 frame、broadcast/multicast、MTU 边界；
- inbound/outbound queue 满、host TX `Retry`、guest `NoGuestBuffer`；
- notification suppression、interrupt status/ACK；
- IRQ callback 只发布 readiness，不执行 packet path；
- worker panic、cancel/join、stop/start/reset/remove 和 stale generation；
- 两个 NIC 时严格按 selector/MAC claim。

### 9.2 QEMU 分级测试

1. 无 host NIC：原有 VM 启动基线；
2. deterministic peer：`UDP_ECHO_PASS` 首次和 reset 后通过；
3. QEMU user net：DHCP、DNS、TCP/HTTP，CI 可运行；
4. TAP + 本机可控服务：ARP、DHCP、DNS、TCP、HTTP，作为确定性 TAP 验收；
5. TAP + NAT 外网：HTTPS 和人工百度访问；
6. 重复运行至少两次，确认 host 和 AxVisor 无资源泄漏。

抓包点至少包括 Linux TAP；日志 counters 能与抓包数量解释一致。失败时按
“guest descriptor -> AxVisor outbound -> host TX -> TAP -> host RX -> AxVisor inbound ->
guest descriptor -> guest IRQ”逐点定位，不用增加无界逐包日志作为长期方案。

## 10. 代码阶段验证命令

按实际修改范围执行最低层回归测试，并至少运行：

```bash
cargo fmt --all
cargo xtask clippy --package axaddrspace
cargo xtask clippy --package axvm
cargo xtask clippy --package ax-driver
cargo xtask clippy --package axvisor
```

若 `cargo xtask clippy --package ax-driver` 无法表达仅启用 virtio-net 的 feature 组合，先
检查 xtask 流程，再补充与构建配置一致的 targeted Cargo clippy。ArceOS guest 和 AxVisor
AArch64 镜像均使用 `cargo xtask` 构建；QEMU 使用专用 `--qemu-config`，不把本机路径提交
进配置。

每次端到端运行保存：构建命令、QEMU 参数、host topology 状态、guest result markers、
AxVisor 聚合 counters，以及失败时的限时 TAP pcap。

## 11. 预计文件归属

- **现有缺陷修复**：`virtualization/axaddrspace`、`virtualization/axvm`、
  `os/axvisor/src/virtio_net`、VM manager 和确定性 guest app；
- **通用 host NIC capability**：优先复用 `drivers/net/rd-net` 和
  `drivers/interface/rdif-eth`；只有 fake endpoint 证明现有契约不足时才做小范围扩展；
- **AxVisor glue/runtime**：`os/axvisor/src/virtio_net/` 下新增 uplink claim、bridge runtime、
  lifecycle 和 stats，不把这些策略下沉到 `axvirtio-net`；
- **配置**：AxVisor 专用 VM TOML、AArch64 QEMU user/TAP configs 和 workspace-relative
  guest artifact；
- **host tooling**：AxVisor 网络脚本目录中的 TAP/NAT setup/status/teardown；不直接扩大
  旧 ArceOS 脚本的破坏性语义；
- **guest acceptance**：独立网络验收 app/build config，不让确定性 echo app 同时承担
  DHCP、DNS 和 TLS 测试。

## 12. 最终完成定义

只有同时具备以下证据，才能宣称“完整 TAP 已实现”：

1. 第 2 节所有已知缺陷有回归测试且已修复；
2. 同一套 AxVisor raw uplink 数据面通过 fake port、QEMU user net 和真实 TAP 三层测试；
3. TAP 抓包证明 guest MAC 的双向 Ethernet frame，guest 输出 DHCP/DNS/TCP/HTTP(S)
   对应 PASS marker；
4. reset 和 stopped-start 后再次通过，旧 generation worker 已退出；
5. host setup/teardown 幂等且不破坏既有 bridge、forwarding 或 firewall；
6. 配置无开发机绝对路径、无根节点广泛透传、无“第一块网卡”隐式选择；
7. 百度等公网访问仅作为外部可达性补充证据，确定性 CI 不依赖第三方站点。
