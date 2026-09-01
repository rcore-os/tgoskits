# 统一 SDIO 与 AIC8800 OS 无关驱动架构

## 问题与目标

项目原有多套 SDIO 协议：控制器、旧 `sdio-host` 和 AIC8800 分别编码
CMD5/CMD52/CMD53，并重复解析 CCCR/FBR/CIS 和维护 Function 状态。本次只
保留 `sdmmc-host` 与 `sdmmc-protocol`：前者定义非阻塞物理 host 能力，
后者独占 SD/MMC/SDIO 卡协议。Memory-only 与 IO-only 明确支持；Combo card
返回 `UnsupportedComboCard`。AKA 实板的 common CIS 为 `c8a1:c08d`，对应
AIC8800DC，而不是原先假定的 D80 `c8a1:0082`。当前支持 profile 因此限定为
AIC8800D80 与 AIC8800DC U02/H-U02/HBT-U02；缺少可信固件来源的 DC U01、
D80X2、DW、AIC8801 和未知变体直接失败，不共享猜测性 profile。

## 依据与方案选择

- 卡协议以 SD Association 的 SDIO Simplified Specification 3.00 为语义依据，
  覆盖 CMD5、CMD52、CMD53、CCCR、FBR、CIS、Function 生命周期和 Function IRQ。
- Linux 6.x 的 `sdio_func` 生命周期及 `drivers/mmc/core/sdio_irq.c` 只用于核对
  Function/IRQ 所有权，不复制其内核线程、claim/release 或等待模型。
- Linux `sdio_read_func_cis()` 允许 Function CIS 不携带 `CISTPL_MANFID`，此时
  `sdio_func.vendor/device` 继承 common CIS。portable 协议保留原始 CIS，AIC owner
  只在身份解析边界计算同样的 effective identity，不能把缺失 Function tuple 当成
  D80，也不能只凭 vendor ID 猜变体。
- 项目内部比较过“保留多套实现并加兼容层”“控制器继续暴露高级 SDIO API”与
  “统一卡协议”。兼容层仍会保留重复状态源，高级控制器 API 会把卡协议下沉到
  硬件层，因此选择唯一 `sdmmc-host` 能力接口加唯一 `sdmmc-protocol` 卡协议。
- AIC 比较过同步包装器、驱动自建线程和 owner 驱动状态机。同步包装器会隐藏
  取消/超时，驱动线程会绑定 OS；因此核心与 adapter 均只返回有限步骤进度，
  由外层 owner 决定等待策略。
- 主线 Linux v7.1（`8cd9520d35a6c38db6567e97dd93b1f11f185dc6`）和
  Linux 6.16（`038d61fd642278bab63ee8ef722c50d10ab01e8f`）都没有
  AIC8800 驱动。AIC LMAC 消息布局和状态语义以本机 Sipeed out-of-tree Linux
  驱动 `d4003f15b35d43ad4842f427050ab2bba0114fa5` 为依据；只借鉴
  `rwnx_msg_tx.c`、`rwnx_msg_rx.c`、`lmac_msg.h` 的协议，不复制 cfg80211、
  线程、锁和同步等待模型。
- DC 固件、patch、system config、双 Function SDIO 和 RF 配置以同一 vendor tree
  的 `aic_bsp_driver.c`、`aic8800dc_compat.c`、`aicwf_sdio.c` 为依据。历史 Rust
  实现只用于交叉核对 wire 数据与实板事实；其全局回调、轮询和 10 ms kicker 不迁移。

## 四层职责

| 层 | crate/位置 | 职责 |
| --- | --- | --- |
| Driver Core | `drivers/net/aic8800` 默认模块 | 寄存器语义、固件/命令/RX/TX/AP 状态机；只接收显式时间、熵、SDIO 完成和 IRQ 快照 |
| Capability Adapter | `drivers/net/aic8800/src/rdif`（`rdif` feature） | 转换 `SdioCard`、拥有型 DMA、RDIF token、IRQ 快照和核心事件；有界 SPSC、无执行线程 |
| OS Glue | `drivers/ax-driver` | FDT、MMIO、CV181x pinmux/reset/clock、DMA capability 和设备注册 |
| Runtime | `net/ax-net`、`axruntime` | 固定 CPU owner、定时唤醒、IRQ 注册/亲和性/同步和队列调度 |

`aic8800` 是唯一 AIC crate 且始终为 `no_std`。默认 feature 不编译 RDIF；
可选 adapter 不依赖 `ax-sync`、`ax-task`、`axruntime`，也不创建线程、休眠
或自旋。两层没有独立发布和生命周期需求，因此不增加第二个 crate。

具体芯片 feature 只由 `ax-driver/aic8800-wifi` 所有；AKA 和 LicheeRV 的板级
构建配置直接选择该 feature，并独立选择通用 `starry-kernel/sg2002` 平台能力。
`starry-kernel`、`axruntime` 和 `axstd` 不保留 `aic8800-wifi` 转发 alias；通用
网络执行能力仍由 `ax-runtime/net` 提供。这样与其他网卡一致，板级策略不会把
可移植驱动伪装成 Starry kernel 功能，也不会产生多层 feature 兼容入口。

## 事务与 IRQ 所有权

`sdmmc-host` 定义 move-only `HostParts`；`SdMmcIrqHost::into_parts()` 一次性
拆出 `HostParts { bus, irq, card_irq }`。
硬 IRQ 独占 `irq`，只读/确认/屏蔽状态并发布预分配快照。固定 CPU owner
独占 `bus`、`card_irq`、`SdioCard` 与 `AicDevice`，只有它能推进协议、完成
DMA、处理 FIFO 或修改 Function 状态。

通用 `SdMmcIrqHost` 只提供 SD/MMC 协议和块运行时共同需要的 endpoint、
completion enable/disable、DMA 与等待类型。AIC 的 owner 直接要求更窄的
`CompletionIrqRearmHost`：它在任务上下文恢复 completion delivery，同步采集
masked window 内已经 latch 的状态，并把状态发布到 hard IRQ 使用的同一 mailbox。
当前只有 `Sdhci` 和显式委托的 `Cv181xSdhci` 提供该能力；DWMMC、Phytium MCI
和 StarFive wrapper 不承担 AIC 的 rearm 契约。

命令完成、DMA、错误和 `CARD_INT` 可同时到达；adapter 先把 CARD_INT 事实
交给核心，再用同一 acknowledged snapshot 推进活动 host 事务。CMD53 使用
拥有型 `PreparedDma`，完成或 abort 后才恢复 `CompletedDma`。RDIF
`DmaBuffer` 只通过有界 SPSC 转移，提交失败原样归还 token。

协议侧 `QueueFramePort` 拥有设备级 `TxQueueDiscipline`。当前 `axruntime` 为 AIC 和
其它生产网卡显式选择 `Fifo { max_frames: 64 }`：短暂耗尽 TX token 时按顺序保留帧，
queue completion 触发下一轮 protocol poll 后继续 flush，达到设备自己的上限才返回
`Again`。FIFO 由 `VecDeque::new()` 开始，只在第一次真实入队时分配 payload；没有
busy backlog 的设备不再在启动时固定预留约 128.5 KiB。该策略属于 protocol frame
port，不改变 AIC 的 `aic,queue-size`、SPSC/DMA token 数量或 hardware queue 配置。

SDHCI 的 `*_INT_STATUS_ENABLE` 定义本驱动拥有并捕获的 latch，
`*_INT_SIGNAL_ENABLE` 只控制外部 IRQ line。因 CARD_INT 进入 hard IRQ 时，即使
completion signal 临时 masked，也必须按 status-enable 读取、确认并缓存同一代
command/data completion；CARD_INT 本身仍按 signal-enable 判断是否可见并立即 mask，
由 owner drain 后 `rearm_and_check()` 闭合电平竞态。owner 必须先调用
`CompletionIrqRearmHost::rearm_completion_irq_and_check()`，再恢复 CARD_INT，
确保同一窗口内的 completion 和 card 事实都进入 latch。task-context 的“清旧状态、发布
新 request generation、写 argument/command”与 hard-IRQ 的“采样、W1C、按 generation
缓存”必须具有明确 exclusion/handoff，不能假设固定 CPU 会阻止本地硬中断抢占。
与 Linux `sdhci.c` 一致，相邻 normal/error interrupt status、status-enable 和
signal-enable 字段始终通过单次 32-bit MMIO 访问；不能拆成两次
16-bit 交易，否则 DWC MSHC 集成可能丢失 command completion 而在卡识别阶段
超时。

## 启动、等待和回滚

1. ax-driver 映射 MMIO、执行 SDIO1 SoC 设置并注册 portable device；reset
   settle 以绝对 deadline 交给 owner，不在驱动中 sleep。
2. ax-net 固定 owner CPU，注册并启用硬 IRQ；poll group 仍保持 disabled，
   IRQ 只能唤醒启动状态机，不能开放网络队列。
3. owner 完成 IO-only 卡初始化、Function 生命周期、固件和 FDRV；每步只
   返回 Ready、WaitForInterrupt、RetryAt 或错误。
4. 成功后才 refill RX、rearm 并 publish 队列，再执行可选启动 transaction。
   Wi-Fi 控制同样使用 `start/advance/cancel`。
5. 失败先 disable+synchronize IRQ，再 cancel/abort；证明 host DMA 停止后
   释放队列，无法证明时隔离整个 ownership domain。

等待原因必须由类型区分，不能再用一个 `rearm_ready` 布尔值同时表示定时器和
设备中断：

- `RetryAt(deadline)` 只表示 timer deadline。reset settle、vendor settle、flow-control
  backoff 等纯定时等待保持 `CARD_INT` masked；到期由 runtime 在 owner CPU 继续。
- `WaitForInterrupt` 表示只等待下一次设备中断；
  `WaitForInterruptUntil(deadline)` 表示等待设备中断或绝对超时，mailbox
  confirmation 使用后者。只有这两种等待允许 task-context 执行 card-level
  `rearm_and_check()`。
- 活动 CMD52/CMD53 的 controller completion IRQ 独立于 card-level IRQ，始终按
  host transaction 生命周期 enable/ack；它不能因为核心正在 timer wait 而被关闭。

该区分也固定 AIC 启动时序：Function enable、block size 和 vendor register setup
可以在 card IRQ masked 时推进；只有 mailbox 已写入且进入 confirmation wait 后才
开放 card IRQ。FriendlyARM vendor Linux tree
`174d4e6989914651850b3ba52c7880a458aa3602` 的 `aicwf_sdio_bus_start()` 先为 DC 的
Function 1/2 安装 handler 再写两条 `intr_config_reg = 0x07`，而 RX handler 只在
实际 IRQ 后读取相应 Function 的 `block_cnt_reg`。本项目不复制 Linux 线程和
`sdio_claim_host()`，但保留同样的“已建立 IRQ consumer 后才允许设备 IRQ 驱动
FIFO drain”语义；固件 settle 的 timer 不能冒充这个 consumer-ready 状态。

卡初始化完成后，owner 以 Function 1 effective identity 选择唯一 `ChipProfile`，再
构造 `AicDevice`。FDT 和 `AicRdifOptions` 不携带芯片型号，不允许板级配置与 CIS
形成两个身份源。完整 `(VID,DID)` 必须匹配支持表；DC 的 Function 1 无 MANFID 时
继承 common `c8a1:c08d`，D80X2/DW/AIC8801 和未知 pair 均 fail closed。

固件应用启动后还必须完成 FDRV 配置事务，不能在 `MM_SET_STACK_START_CFM`
后直接发布网络设备：

- D80 顺序保持为 stack start、vendor TX-power profile、RF calibration、读取并校验
  非零单播 MAC、firmware reset、ME capability、信道表、`MM_ADD_IF_REQ`、启动
  MAC、RX filter 和 chip IRQ arm。vendor 默认关闭额外 TX-power offset/adjust
  请求，因此不发送两个 disabled 全零事务。
- DC 在 firmware 阶段先读取 `0x40500000` 和 `0x20`，分别保留 silicon revision、
  H 标志、MCU 标志、bit 26 的 BT capability 与 sub-id。sub-id 1 选择 U02；
  sub-id 2 且 H 标志有效时，根据 BT capability 严格选择 H-U02 或 HBT-U02。
  AKA 实板报告 `chip_id=0xc7, sub_id=2, btenable=1`，必须选择 HBT 资产，不能把
  HBT 当作 H 的兼容别名。sub-id 0 是 U01，由于 pinned firmware 源没有完整 U01
  blob，返回 `UnsupportedRevision`，不得套用 U02。
- DC U02/H-U02/HBT-U02 执行独立 system-config/masked-write profile，把对应 ROM patch
  上传到 `0x0018_0000`，从 `0x10164` 读取 wifisetting、LDPC、AGC、TX-gain
  配置指针，写入 vendor RF 表和自描述 patch table；H/HBT 分别使用与 BT capability
  匹配的 patch、patch table 和 calibration image。最后从 `0x0012_0000` 以
  `HOST_START_APP_DUMMY` 启动，不复用 D80 的
  main/patch 地址或 metadata。
- DC FDRV 使用 `MM_SET_STACK_START_REQ(start=1,is_5g=false)`，随后发送 DC
  24 GHz TX gain、20/40 MHz RX gain 和 RF calibration，再进入与 D80 共用且经
  wire-equivalence 测试证明的 MAC/reset/ME/channel/interface/start/filter 流程。

成功的 add-interface confirmation 返回的 `inst_nbr` 是唯一 firmware VIF 来源。

LMAC payload 按 vendor Linux C ABI 的自然对齐构造，包括尾部 padding；不能用字段
长度之和替代 `sizeof(struct ...)`。confirmation 的声明长度、消息 ID、精确 payload
长度和 status 都必须校验；截断、非零 status、非零 padding 或无效索引使启动失败。
host-to-firmware 使用 8-byte `struct lmac_msg`，firmware-to-host 则使用 12-byte
`struct ipc_e2a_msg`，后者在 `param_len` 后带独立的 32-bit `pattern`。这两个方向是
不同的 vendor ABI，不能共用“LMAC header size”常量。config RX 解析器要求外层
`pkt_len` 精确等于 12-byte E2A header 和声明 payload 之和，并跳过 `pattern` 后再把
声明区间交给 confirmation/indication 解析；`pattern` 不是 payload、padding 或
status。
只有取得 MAC 和 firmware VIF 后才发布 `Started`。

STA 连接由同一 owner 状态机串行推进：

1. `SM_CONNECT_CFM.status == 0` 只表示固件接受了连接流程，不能完成控制事务。
2. owner 等待 `SM_CONNECT_IND`；只有 `status_code == 0` 才保存其中的 `vif_idx`
   和 AP station entry `ap_idx`。普通 TX descriptor 分别使用这两个索引；`0xff`
   只允许表示未关联/未知 station，不能用于已连接的数据 TX。
   FULLMAC Ethernet TX descriptor 严格使用 vendor 28-byte `struct hostdesc`：VIF/STA
   位于偏移 24/25；Linux 路径先把目的/源地址和 EtherType 写入 descriptor，再
   `skb_pull(14)`，因此 descriptor 后只放 L3 payload，而不是重复完整 Ethernet
   frame。host-to-device packet type 使用 vendor TX 值 `0x01`，不能复用 RX
   aggregate 的 `0x00`；D80 CRC header
   声明 descriptor+payload 的未对齐长度，DC/V1 则按 vendor 路径声明 word-aligned
   aggregate 长度。
3. Linux WEXT 边界按 UAPI 的 `iwreq -> iw_point -> struct iw_encode_ext + key[]`
   原生布局接收 `IW_ENCODE_ALG_PMK` 的 32 字节 PMK。这与 Linux
   `wpa_supplicant` `driver_wext` 的 PMK-offload 调用相同，但不声称通用 mainline
   cfg80211 WEXT backend 会接受 PMK；它是本设备支持的 Linux UAPI 子集。旧的
   raw-passphrase pointer ABI 被直接删除，不提供双解析兼容层。产品构建把
   SSID/passphrase 编入 `ax-driver` 启动配置，OS Glue 在启动时使用 RustCrypto
   `pbkdf2`/`sha1` 派生 PMK；凭据不再通过 DTB、session sidecar 或 guest helper
   建立第二条配置通道。
4. WPA2-PSK/AES 使用调用方熵生成 SNonce。AIC core 的密码学原语集中复用
   `no_std` RustCrypto：`hmac`/`sha1` 计算 PTK PRF 与 MIC，`aes-kw` 执行 RFC 3394
   GTK unwrap，`subtle` 做常量时间比较，`zeroize` 清除密钥。本地纯 core 只实现
   802.11i 特有的 EAPOL-Key 编解码与 M1/M3 状态转换；M2/M4 经同一 SDIO owner TX
   路径发送，禁止在 mailbox、RDIF 或 OS glue 重复密码学逻辑。
   AP 以相同 replay counter 和 ANonce 重发 M1 时，supplicant 必须重发相同 M2，
   不能把合法丢包恢复误判为握手失败；不同 counter/ANonce 仍 fail closed。GTK KDE
   只接受单个 16-byte CCMP GTK、合法 key-info 和零 reserved byte，截断、超长或
   重复 KDE 都返回 typed error。
5. owner 依次确认 PTK、GTK 安装和 `ME_SET_CONTROL_PORT_REQ(open=true)`，最后才
   发布 `ControlComplete`，外层随后提交 STA 配置并启动 DHCP。
6. 主动断连等待空的 `SM_DISCONNECT_CFM`，异步 `SM_DISCONNECT_IND` 独立校验
   reason/VIF 并清理 link；二者不能互相冒充。disconnect、timeout、
   MIC/replay/RSN 错误或任何 firmware rejection 都清除 peer、key、VIF/STA link
   状态并返回 typed error，不发布假成功。

EAPOL 是驱动内部控制流，不作为普通 Ethernet RX 向协议栈发布。固件 indication
直接在本次有界 FIFO batch 中分派，不另设无界接收队列。QoS control 标记 A-MSDU
时，core 在同一有界 batch 内严格校验每个 subframe 的 DA/SA、big-endian length、
LLC/SNAP 和 4-byte padding，再分别发布 Ethernet frame；畸形聚合整体丢弃，不能把
subframe header 当作 LLC/SNAP 后静默丢包。

DC 使用 V1 SDIO profile 和两个 Function，但仍只有一个 owner：Function 1 承载
普通数据/管理帧，Function 2 承载 firmware mailbox。两者均由
`sdmmc-protocol::SdioCard` 设置 512-byte block size、enable 并维护生命周期；
Function 2 不产生第二个线程、锁或 executor。DC RX 以 V1 `block_cnt` 读取 512-byte
块数并分别 drain 两条 FIFO；D80 继续使用 V3 misc-status、byte/block mode 和 header
CRC。transport/profile 必须显式选择这些差异，未知变体不能落入 V1 默认分支。

硬件只向 host 暴露 card-level IRQ，不能把它压缩成“仅 Function 1 有数据”。core
在每次新 `CARD_INT` 上启动一次有界 RX scan：按 profile 的唯一 RX Function 集合
先检查 command Function、再检查 data Function，每个 Function 都重复读取
`block_cnt` 并 drain 到 0；scan 期间再次到达的 IRQ 只设置一次 rescan，不创建无界
事件队列。Function 2 的 config confirmation 直接完成唯一活动 mailbox，Function 1
的数据和异步 indication 进入统一 frame dispatcher。mailbox 发送完成后等待 IRQ
驱动的 confirmation 或绝对超时，不再自行每 1 ms 轮询 Function 2；缺少 IRQ、未知
消息、乱序 confirmation 或任一 Function 无法 drain 都显式失败。该模型对应 vendor
Linux 为 `func`/`func_msg` 分别注册 handler 并 drain 两个 FIFO 的所有权语义，同时
保留本项目单 owner、无线程和固定 CPU queue-NAPI 约束。

安全连接必须携带调用方拥有的 32 字节熵。熵缺失返回
`EntropyUnavailable`，禁止用时间戳代替随机源。

普通 WEXT 连接和编译期 station 启动事务都由 `ax-net` 在提交 owner transaction
前从运行时 CSPRNG 补齐熵；显式 `connect_wpa2_pmk_with_entropy` 保持优先。CSPRNG 只能由
`ax_hal::boot::boot_entropy()` 的
可信 UEFI RNG 或精确 32 字节 FDT `/chosen/rng-seed` 初始化。板级 runner 为每次
运行生成临时 DTB 副本并注入新 seed；静态 DTB、时间、计数器、地址和 MAC 都不是
随机源。缺少可信 seed 时安全连接 fail closed。

AKA board build 直接读取 `STARRY_WIFI_SSID` 与 `STARRY_WIFI_PASSWORD`。输入校验位于
`ax-driver` build support，因此半组变量、非法 SSID 或非法 WPA2 密码在申请板卡前
失败；AIC OS Glue 用 RustCrypto PBKDF2-SHA1 派生 PMK，并通过已有
`WifiTransaction` 发布 station 启动事务。portable AIC core 不读取环境变量、不解析
产品配置，也不增加第二套连接 API。

普通 ostool `session_files` 继续走现有 HTTP 传输；启动事务在内核网络初始化阶段完成
WPA2 和 DHCP 后，iperf 脚本才使用该通道下载。凭据不经过 sidecar、boot archive、
guest helper 或 Starry 专用内核文件协议。板级 runner 只在发现完整 Wi-Fi 环境变量时
生成带新 `rng-seed` 的临时 DTB 副本；仓库 DTB 不修改，临时副本随 board run 的
RAII guard 清理。

## FDT/ACPI 参数边界

板级物理地址、IRQ、clock/reset/power-domain、pinctrl、DMA 地址宽度、总线频率
和产品网络策略只允许出现在 OS Glue 输入层。当前 SG2002/CV181x 硬件资源
使用 FDT：SDIO consumer 节点通过 `reg-names` 提供 `sdio`、`syscon`、`crg`、
`rtcsys-ctrl`、`rtcsys-io`，或者分别用 `cvitek,syscon`、`cvitek,crg`、
`cvitek,rtcsys-ctrl`、`cvitek,rtcsys-io` phandle 引用 provider。缺少必要资源时
probe 显式失败，不回退到写死物理地址。

通用 SD/MMC 属性 `bus-width`、`min-frequency`、`max-frequency`、`no-1-8-v`、
`cd-gpios` 与 clock/reset/power-domain 由 rdrive 统一解析。AIC 附加策略使用
`dma-address-bits`、`post-power-on-delay-ms`、`aic,startup-timeout-ms`、
`aic,control-timeout-ms`、`aic,queue-size` 和 `aic,max-frame-size`。可选启动 AP
必须显式配置 `aic,startup-mode = "access-point"` 及 `aic,ap-ssid`、
`aic,ap-channel`、`aic,ap-ipv4`、`aic,ap-prefix-length`；未配置时只注册
`wlan0`。AKA 的 station 产品策略来自上述编译期环境变量；若同时配置 FDT 启动
策略则 probe 显式拒绝，不能按隐式优先级覆盖。

支持 ACPI 的平台使用 rdrive ACPI memory/IRQ/_CCA 资源构造同一 portable
配置；CV181x 当前只存在 FDT probe，因此本次不伪造 ACPI 节点或默认资源。
控制器寄存器 offset、bitfield、AIC Function 1 和固件规定的 512-byte block
属于 silicon/protocol 常量，仍保留在 portable crate。

芯片型号不属于板级策略：FDT 不接受 `aic,chip-variant`，OS Glue 不猜测型号；
身份只来自初始化完成的 CIS。DC/D80 profile 选择和 unsupported 语义完全位于
portable owner/core 边界。

## 固件供应与变体门禁

固件 blob 不提交仓库。`build.rs` 从固定 upstream commit 获取每个 profile 的精确
文件并校验 SHA-256，或校验 `AIC8800_FIRMWARE_DIR`/本地 cache 中同名文件；缺失、
hash 不符或 profile 文件不完整均在构建时失败。Cargo package 继续排除
`/firmware/`。DC U02/H-U02/HBT-U02 的 ROM patch、FMAC patch table 和 calibration
分别作为 manifest 项列出，RF 配置数组来自 vendor 源码并在 Rust 中以 typed table
维护，不以空表降级。上游 blob 仓库没有可确认的 license 元数据，因此只沿用现有
按需供应机制，不将 blob 再分发进源码包。

## 依赖与源码门禁

- `aic8800` 默认依赖树不得出现 `rd-net`、`rdif-eth`、`ax-sync`、`ax-task` 或
  `axruntime`；不得出现全局 runtime、线程创建、sleep、yield 或 spawn。
- `rdif` feature 只增加便携 capability 依赖，不得引入 `ax-sync`、`ax-task`
  或 `axruntime`，所有队列必须有界且通过所有权转移。
- 带子模块的源码目录使用 `foo/mod.rs + foo/child.rs`；约 400 行开始审视职责，
  800 行以上必须拆分。寄存器字段使用 `tock-registers`，卡协议 wire bits 保持
  强类型编解码，不冒充 MMIO 寄存器。

## 测试边界

纯编解码、状态机、fake MMIO/DMA/host 使用 std host test；私有单元测试在
源文件末尾，`tests/` 只调用公开 API。必须启动 ArceOS/QEMU 的路径使用
axtest。FDT、真实 IRQ、CIS、固件、持续收发和吞吐最终以
`AKA-00-SG2002` 实板结果为准，QEMU 不能替代。

参考资料：

- [SDIO Simplified Specification 3.00](https://www.sdcard.org/cms/wp-content/themes/sdcard-org/dl.php?f=PartE1_SDIO_Simplified_Specification_Ver3.00.pdf)
- [Linux SDIO function definition](https://github.com/torvalds/linux/blob/45c13f3f9e3bb15fd89ff2864c6f627a3b4b4229/include/linux/mmc/sdio_func.h)
- [Linux SDIO IRQ lifecycle](https://github.com/torvalds/linux/blob/45c13f3f9e3bb15fd89ff2864c6f627a3b4b4229/drivers/mmc/core/sdio_irq.c)
- [RustCrypto password hashes](https://github.com/RustCrypto/password-hashes)
- [RustCrypto AES key wraps](https://github.com/RustCrypto/key-wraps)
