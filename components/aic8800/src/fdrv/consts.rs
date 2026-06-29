//! aic8800_fdrv 驱动内部常量
//!
//! 仅包含本驱动特有的配置常量（缓冲区大小、队列、TX 功率、超时、延迟等）。
//! 协议层常量（消息 ID、认证类型、加密套件、结构体大小等）统一在 protocol::lmac_msg 中定义。
//! 芯片级常量（寄存器地址、SDIO 类型、固件地址等）统一在 aic8800_common 中定义。

// ============================================================
// SDIO 帧构造常量（驱动内部使用的别名）
// ============================================================

/// SDIO 功能块大小 (字节) — 驱动内部使用 usize 类型
pub const SDIOWIFI_FUNC_BLOCKSIZE: usize = 512;

/// Dummy word 长度 (字节)
pub const DUMMY_WORD_LEN: usize = 4;

/// 尾部长度 (字节)
pub const TAIL_LEN: usize = 4;

/// 发送对齐 (字节)
pub const TX_ALIGNMENT: usize = 4;

// ============================================================
// 流控常量
// ============================================================

/// 流控重试最大次数
pub const FLOW_CONTROL_MAX_RETRY: u32 = 100;

// ============================================================
// 响应等待常量
// ============================================================

/// SDIO_OTHER_INTERRUPT 标志位
pub const SDIO_OTHER_INTERRUPT: u8 = 0x80;

/// 块计数掩码 (低 7 位)
pub const BLOCK_COUNT_MASK: u8 = 0x7F;

/// 响应超时最大重试次数
pub const RESPONSE_MAX_RETRY: u32 = 10_000;

// ============================================================
// 芯片初始化常量
// ============================================================

/// 中断配置寄存器值 (使能所有中断)
pub const INTR_CONFIG_VALUE: u8 = 0x07;

/// 堆栈起始参数
pub const STACK_START_PARAM: [u8; 4] = [0x01, 0x00, 0x00, 0x00];

/// AP 模式下注册表(registered_stas)的容量上限。防止异常情况下(如持续收到
/// 不同 MAC 的 AssocReq 而无对应 deauth)注册表无界增长。当前固件 SoftAP 实际
/// 关联数远低于此值。
pub const MAX_REGISTERED_STAS: usize = 16;

/// AP 控制端口(authorize)打开命令的最大重试次数。开放网络关联后必须显式授权,
/// 否则固件丢弃该 STA 的所有数据帧(DHCP/ARP/IP)。单核协作调度下该命令可能
/// 概率性超时,AP worker 周期对账据此重试,到上限仍失败则放弃(STA 多半已离线)。
pub const CONTROL_PORT_MAX_RETRY: u8 = 20;

/// AP worker 控制端口对账的周期(毫秒)。仅在存在未授权 STA 时才周期性自唤醒。
pub const CONTROL_PORT_RECONCILE_MS: u64 = 50;

// ============================================================
// 协议头部常量
// ============================================================

/// SDIO 头部大小 (字节)
pub const SDIO_HEADER_SIZE: usize = 4;

/// LMAC 消息头部大小 (字节)
pub const LMAC_MSG_HEADER_SIZE: usize = 8;

/// 完整协议头部大小 (SDIO + Dummy + LMAC)
pub const PROTO_HEADER_SIZE: usize = SDIO_HEADER_SIZE + DUMMY_WORD_LEN + LMAC_MSG_HEADER_SIZE; // = 16

// ============================================================
// 超时常量 (毫秒)
// ============================================================

/// 默认命令超时
pub const DEFAULT_CMD_TIMEOUT_MS: u64 = 2000;

/// 扫描超时
pub const SCAN_TIMEOUT_MS: u64 = 15000;

/// 连接超时
pub const CONNECT_TIMEOUT_MS: u64 = 10000;

/// 断连超时
pub const DISCONNECT_TIMEOUT_MS: u64 = 5000;

// ============================================================
// WiFi 配置常量
// ============================================================

/// 默认 VIF 索引
pub const DEFAULT_VIF_IDX: u8 = 0;

/// 最大 SSID 长度
pub const MAX_SSID_LEN: usize = 32;

/// 最大密码长度
pub const MAX_PASSPHRASE_LEN: usize = 64;

/// 默认信道
pub const DEFAULT_CHANNEL: u8 = 6;

// ============================================================
// 缓冲区大小常量
// ============================================================

/// 命令响应缓冲区大小
pub const CMD_RESPONSE_BUF_SIZE: usize = 512;

/// 扫描结果最大数量
pub const MAX_SCAN_RESULTS: usize = 64;

/// TX 队列大小
pub const TX_QUEUE_SIZE: usize = 128;

/// RX 队列大小
pub const RX_QUEUE_SIZE: usize = 256;

// ============================================================
// 数据掩码常量
// ============================================================

/// 字节低 8 位掩码
pub const U8_MASK: u8 = 0xFF;

/// 字节低 4 位掩码
pub const LOW_NIBBLE_MASK: u8 = 0x0F;

/// u16 低 8 位掩码
pub const U16_LOW_MASK: u16 = 0xFF;

/// u16 高 4 位掩码
pub const U16_HIGH_NIBBLE_MASK: u16 = 0x0F;

/// u16 低 10 位掩码 (用于 msg_id 提取)
pub const MSG_INDEX_MASK: u16 = (1 << 10) - 1; // = 0x3FF

// ============================================================
// HostDesc 常量
// ============================================================

/// HostDesc 大小 (字节)
pub const HOSTDESC_SIZE: usize = 28;

/// HostDesc 中 hostid 需要设置 TX 确认标志
pub const HOSTDESC_TX_CFM_FLAG: u32 = 0x8000_0000;

// ============================================================
// 802.11 帧常量
// ============================================================

/// 802.11 帧头部大小 (字节)
pub const IEEE80211_HDR_SIZE: usize = 24;

/// 802.11 管理 Beacon 帧最小大小 (字节)
pub const IEEE80211_BEACON_MIN_SIZE: usize = 36;

/// 802.3 Ethernet 头部大小 (字节)
pub const ETH_HDR_SIZE: usize = 14;

// ============================================================
// EAPOL / 802.1X 常量
// ============================================================

/// 802.1X Authentication EtherType
pub const ETH_P_PAE: u16 = 0x888E;

/// EAPOL 帧版本 (802.1X-2004)
pub const EAPOL_VERSION: u8 = 0x01;

/// EAPOL 帧类型：EAPOL-Packet
pub const EAPOL_PACKET: u8 = 0x00;

/// EAPOL 帧类型：EAPOL-Start
pub const EAPOL_START: u8 = 0x01;

/// EAPOL 帧类型：EAPOL-Logoff
pub const EAPOL_LOGOFF: u8 = 0x02;

/// EAPOL 帧类型：EAPOL-Key
pub const EAPOL_KEY: u8 = 0x03;

/// EAPOL 帧类型：EAPOL-Encapsulated-ASF-Alert
pub const EAPOL_ASF_ALERT: u8 = 0x04;

// ============================================================
// TX Power 常量
// ============================================================

/// 默认 TX Power Index (AIC8801)
pub const DEFAULT_TXPWR_DSSS: u8 = 9;

/// 默认 TX Power Index (2.4GHz OFDM 低速率)
pub const DEFAULT_TXPWR_OFDM_LOW_2G4: u8 = 8;

/// 默认 TX Power Index (2.4GHz OFDM 64QAM)
pub const DEFAULT_TXPWR_OFDM64_2G4: u8 = 8;

/// 默认 TX Power Index (2.4GHz OFDM 256QAM)
pub const DEFAULT_TXPWR_OFDM256_2G4: u8 = 8;

/// 默认 TX Power Index (2.4GHz OFDM 1024QAM)
pub const DEFAULT_TXPWR_OFDM1024_2G4: u8 = 8;

/// 默认 TX Power Index (5GHz OFDM 低速率)
pub const DEFAULT_TXPWR_OFDM_LOW_5G: u8 = 11;

/// 默认 TX Power Index (5GHz OFDM 64QAM)
pub const DEFAULT_TXPWR_OFDM64_5G: u8 = 10;

/// 默认 TX Power Index (5GHz OFDM 256QAM)
pub const DEFAULT_TXPWR_OFDM256_5G: u8 = 9;

/// 默认 TX Power Index (5GHz OFDM 1024QAM)
pub const DEFAULT_TXPWR_OFDM1024_5G: u8 = 9;

/// 默认 TX Power Offset
pub const DEFAULT_TXPWR_OFST: [u8; 8] = [1, 0, 0, 0, 0, 0, 0, 0];

// ============================================================
// RF 校准常量
// ============================================================

/// RF 校准配置：2.4GHz
pub const RF_CALIB_CFG_24G: u32 = 0x0000_00BF;

/// RF 校准配置：5GHz
pub const RF_CALIB_CFG_5G: u32 = 0x0000_003F;

/// RF 校准参数 alpha
pub const RF_CALIB_PARAM_ALPHA: u32 = 0x0C34_C008;

/// RF 校准 BT 参数
pub const RF_CALIB_BT_PARAM: u32 = 0x0026_4203;

// ============================================================
// MM 启动配置常量
// ============================================================

/// 默认 UAPSD 超时值 (ms)
pub const DEFAULT_UAPSD_TIMEOUT: u32 = 300;

/// 默认低功耗时钟精度 (ppm)
pub const DEFAULT_LP_CLK_ACCURACY: u16 = 20;

// ============================================================
// 时钟配置常量
// ============================================================
// DEFAULT_CLOCK_FREQ, FIRMWARE_START_CLOCK_FREQ 定义在 aic8800_fw::chip::variant，此处不再重复

/// 初始化时钟频率 (Hz)
pub const INIT_CLOCK_FREQ: u32 = 400_000;

/// 高速时钟频率 (Hz)
pub const HIGH_SPEED_CLOCK_FREQ: u32 = 50_000_000;

// ============================================================
// RX 线程常量
// ============================================================

/// RX 硬件头部长度
pub const RX_HWHRD_LEN: usize = 60;

/// RX 对齐
pub const RX_ALIGNMENT: usize = 4;

/// 最大包长度
pub const MAX_PKT_LEN: u16 = 1600;

// ============================================================
// TX 线程常量
// ============================================================

/// TX 缓冲区大小
pub const BUFFER_SIZE: usize = 1536;

/// 流控命令重试次数
pub const FLOW_CTRL_CMD_RETRY: u32 = 10;

/// TX 批量发送上限
pub const TX_BATCH_LIMIT: u32 = 64;

/// TX 队列最大长度
pub const MAX_TX_QUEUE_LEN: usize = 256;

/// 数据帧流控阈值（credits <= 此值时暂停发送）
pub const DATA_FLOW_CTRL_THRESH: u8 = 2;

// ============================================================
// 过滤器常量 (NXMAC RX filter)
//
// 位定义照搬 vendor reg_access.h。注意：早期版本这里的位偏移是错的
// (用了 0/1/2/3)，导致 AP 模式收不到 Auth 帧。真实位偏移见下。
// STA 模式实际用的是硬编码的 RWNX_DEFAULT_RX_FILTER 值，一直正确；
// AP 模式之前误用错误位拼出的值，故 Auth/Assoc 被固件丢弃。
// ============================================================

/// 接收发往本机 MAC 的单播帧 (Auth/单播管理帧依赖此位)
pub const NXMAC_ACCEPT_MY_UNICAST_BIT: u32 = 1 << 7;
/// 接收任意单播帧 (混杂)
pub const NXMAC_ACCEPT_UNICAST_BIT: u32 = 1 << 6;
/// 接收多播帧
pub const NXMAC_ACCEPT_MULTICAST_BIT: u32 = 1 << 2;
/// 接收广播帧
pub const NXMAC_ACCEPT_BROADCAST_BIT: u32 = 1 << 3;
/// 接收 Probe Request 帧 (AP 模式)
pub const NXMAC_ACCEPT_PROBE_REQ_BIT: u32 = 1 << 8;
/// 接收 Probe Response 帧
pub const NXMAC_ACCEPT_PROBE_RESP_BIT: u32 = 1 << 9;
/// 接收 Beacon 帧
pub const NXMAC_ACCEPT_BEACON_BIT: u32 = 1 << 10;
/// 接收所有 Beacon
pub const NXMAC_ACCEPT_ALL_BEACON_BIT: u32 = 1 << 13;
/// 接收其他 BSSID 的帧
pub const NXMAC_ACCEPT_OTHER_BSSID_BIT: u32 = 1 << 4;
/// 接收 Auth/Assoc 等"其他管理帧" (AP 接客必需)
pub const NXMAC_ACCEPT_OTHER_MGMT_FRAMES_BIT: u32 = 1 << 15;
/// 接收 QoS-Null 帧
pub const NXMAC_ACCEPT_QO_S_NULL_BIT: u32 = 1 << 28;
/// 接收 QoS 数据帧
pub const NXMAC_ACCEPT_Q_DATA_BIT: u32 = 1 << 26;
/// 接收数据帧
pub const NXMAC_ACCEPT_DATA_BIT: u32 = 1 << 24;
/// 接收 Block-Ack 帧
pub const NXMAC_ACCEPT_BA_BIT: u32 = 1 << 17;

/// STA 模式过滤器：vendor 已验证的硬编码值 (RWNX_DEFAULT_RX_FILTER)。
/// 含 MY_UNICAST/OTHER_MGMT/DATA/MULTICAST/BA 等接收必需位。
pub const STA_MODE_FILTER_DEFAULT: u32 = 0x1502_868C;

/// AP 模式过滤器：在已验证的 STA filter 基础上，叠加 AP 接客所需的
/// ProbeReq(8) + AllBeacon(13) + OtherBSSID(4)。直接派生而非手拼，
/// 确保 Auth 帧依赖的 MY_UNICAST(7)/OTHER_MGMT(15) 一定在位。
/// = 0x1502A79C。对齐 vendor AP set_filter(FIF_PROBE_REQ|FIF_OTHER_BSS|...)。
pub const AP_MODE_FILTER_DEFAULT: u32 = STA_MODE_FILTER_DEFAULT
    | NXMAC_ACCEPT_PROBE_REQ_BIT
    | NXMAC_ACCEPT_ALL_BEACON_BIT
    | NXMAC_ACCEPT_OTHER_BSSID_BIT;
