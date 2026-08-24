use alloc::{collections::VecDeque, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU32, Ordering};

use ax_sync::SpinLock as Mutex;
use rd_net::DmaBuffer;

use crate::{common::SDIOWIFI_INTR_CONFIG_REG, fdrv::core::sdio_transport::SdioTransport};

/// 总线状态
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BusState {
    Down,
    Up,
}

/// TX 帧封装
pub struct TxFrame {
    pub data: Vec<u8>,
    pub priority: u8,
    /// true: data 为完整 802.11 管理帧(raw)，按 TXU_CNTRL_MGMT 发送，
    /// 固件不做以太网→802.11 转换；false: data 为以太网帧。
    pub is_mgmt: bool,
    /// Runtime DMA ownership returned only after the SDIO FIFO consumes this
    /// frame. Internal management/control frames carry no token.
    pub completion: Option<DmaBuffer>,
}

/// 连接状态
pub struct ConnectionState {
    /// WiFi 连接状态 (ConnectionStatus 的 u8 表示)
    /// 0 = Disconnected, 1 = Connecting, 2 = Connected, 3 = Failed
    status: AtomicU8,
    pub vif_idx: AtomicU8,
    pub sta_idx: AtomicU8,
    pub sta_mac: Mutex<Option<[u8; 6]>>,
    pub ap_mac: Mutex<Option<[u8; 6]>>,
}

/// 连接状态常量
pub const STATUS_DISCONNECTED: u8 = 0;
pub const STATUS_CONNECTING: u8 = 1;
pub const STATUS_CONNECTED: u8 = 2;
pub const STATUS_FAILED: u8 = 3;

impl Default for ConnectionState {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectionState {
    pub fn new() -> Self {
        Self {
            status: AtomicU8::new(STATUS_DISCONNECTED),
            vif_idx: AtomicU8::new(0xFF),
            sta_idx: AtomicU8::new(0xFF),
            sta_mac: Mutex::new(None),
            ap_mac: Mutex::new(None),
        }
    }

    pub fn get_status(&self) -> u8 {
        self.status.load(Ordering::Acquire)
    }

    pub fn set_status(&self, s: u8) {
        self.status.store(s, Ordering::Release)
    }

    pub fn is_connected(&self) -> bool {
        self.status.load(Ordering::Acquire) == STATUS_CONNECTED
    }
}

/// CMD 状态
pub struct CmdState {
    pub pending: Mutex<Option<Vec<u8>>>,
    pub pending_flag: AtomicBool,
    pub expected_cfm_id: AtomicU16,
    pub rsp_error: AtomicBool,
    pub rsp_queue: Mutex<VecDeque<Vec<u8>>>,
}

impl Default for CmdState {
    fn default() -> Self {
        Self::new()
    }
}

impl CmdState {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(None),
            pending_flag: AtomicBool::new(false),
            expected_cfm_id: AtomicU16::new(0),
            rsp_error: AtomicBool::new(false),
            rsp_queue: Mutex::new(VecDeque::new()),
        }
    }
}

/// RX 状态
pub struct RxState {
    /// Published by the move-only hard endpoint and consumed by the fixed-CPU
    /// queue executor.
    pub irq_pending: AtomicBool,
    pub data_queue: Mutex<VecDeque<Vec<u8>>>,
    pub eapol_queue: Mutex<VecDeque<Vec<u8>>>,
    pub tx_cfm_queue: Mutex<VecDeque<Vec<u8>>>,
}

impl Default for RxState {
    fn default() -> Self {
        Self::new()
    }
}

impl RxState {
    pub fn new() -> Self {
        Self {
            irq_pending: AtomicBool::new(false),
            data_queue: Mutex::new(VecDeque::new()),
            eapol_queue: Mutex::new(VecDeque::new()),
            tx_cfm_queue: Mutex::new(VecDeque::new()),
        }
    }
}

/// TX 状态
pub struct TxState {
    pub queue: Mutex<VecDeque<TxFrame>>,
    pub pktcnt: AtomicU32,
    pub completed: Mutex<VecDeque<DmaBuffer>>,
    pub ind_queue: Mutex<VecDeque<Vec<u8>>>,
}

impl Default for TxState {
    fn default() -> Self {
        Self::new()
    }
}

impl TxState {
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            pktcnt: AtomicU32::new(0),
            completed: Mutex::new(VecDeque::new()),
            ind_queue: Mutex::new(VecDeque::new()),
        }
    }
}

/// 已注册 STA 的表项:(MAC, sta_idx, 控制端口是否已成功打开, 控制端口重试次数)。
pub type RegisteredSta = ([u8; 6], u8, bool, u8);

/// AP 模式状态：待处理的关联请求队列。
///
/// RX 线程收到 AssocReq 时把整帧 mpdu 入队(非阻塞)，由独立的 AP worker
/// 线程取出处理(ME_STA_ADD + Assoc Response)。必须用独立线程，因为
/// ME_STA_ADD 走 send_cmd 阻塞等 CFM，而 CFM 由 RX 线程处理 —— 在 RX
/// 线程里 send_cmd 会死锁。
pub struct ApState {
    pub assoc_queue: Mutex<VecDeque<Vec<u8>>>,
    /// 待从固件 STA 表删除的 sta_idx 队列。deauth/disassoc 在 RX 线程触发,但
    /// MM_STA_DEL_REQ 走 send_cmd 阻塞等 CFM(CFM 由 RX 线程处理)——在 RX 线程里
    /// 直接发会死锁,故入队交给 AP worker 线程执行(与 assoc_queue 同理)。
    pub sta_del_queue: Mutex<VecDeque<u8>>,
    /// 已注册 STA 的 (MAC, sta_idx, 控制端口是否已成功打开, 控制端口重试次数)。
    /// - 手机重传 AssocReq 时据此去重:同一 MAC 已注册就不再发 ME_STA_ADD
    ///   (固件对重复注册不回 CFM,会让 AP worker 阻塞 5 秒超时、连接抖动)。
    /// - 控制端口标志为 false 时(首次开失败/超时),AP worker 周期对账会主动重试
    ///   打开(不依赖手机重传 AssocReq),实现自愈,避免数据帧被固件丢弃导致
    ///   ping 不通;重试次数到上限后放弃,防止已离线 STA 空转。
    /// - 收到 deauth/disassoc 时移除该 MAC,使重连能完整重新注册。
    pub registered_stas: Mutex<Vec<RegisteredSta>>,
}

impl Default for ApState {
    fn default() -> Self {
        Self::new()
    }
}

impl ApState {
    pub fn new() -> Self {
        Self {
            assoc_queue: Mutex::new(VecDeque::new()),
            sta_del_queue: Mutex::new(VecDeque::new()),
            registered_stas: Mutex::new(Vec::new()),
        }
    }
}

/// SDIO 总线共享资源
pub struct WifiBus {
    /// SDIO 传输层
    pub transport: Arc<SdioTransport>,

    /// 总线状态
    pub state: Mutex<BusState>,

    /// 连接状态
    pub conn: ConnectionState,

    /// CMD 状态
    pub cmd: CmdState,

    /// RX 状态
    pub rx: RxState,

    /// TX 状态
    pub tx: TxState,

    /// AP 模式状态
    pub ap: ApState,
}

impl WifiBus {
    pub fn new(transport: Arc<SdioTransport>) -> Arc<Self> {
        Arc::new(Self {
            transport,
            state: Mutex::new(BusState::Down),
            conn: ConnectionState::new(),
            cmd: CmdState::new(),
            rx: RxState::new(),
            tx: TxState::new(),
            ap: ApState::new(),
        })
    }

    /// Quiesces the owner-controlled bus and rejects future work.
    pub fn shutdown(self: &Arc<Self>) {
        *self.state.lock() = BusState::Down;

        let _ = self.transport.write_byte(1, SDIOWIFI_INTR_CONFIG_REG, 0x00);
        self.transport.disable_irq();

        self.tx.queue.lock().clear();
        self.tx.completed.lock().clear();
        self.rx.data_queue.lock().clear();
        self.ap.assoc_queue.lock().clear();
        self.cmd.rsp_error.store(true, Ordering::Release);
        self.rx.eapol_queue.lock().clear();
        self.tx.ind_queue.lock().clear();

        log::debug!("[wifi-bus] shutdown complete");
    }
}
