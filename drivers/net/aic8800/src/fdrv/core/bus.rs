use alloc::{collections::VecDeque, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU32, Ordering};

use ax_sync::SpinLock as Mutex;
use rd_net::DmaBuffer;

use crate::fdrv::core::sdio_transport::SdioTransport;

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

/// AP-mode work published by RX and consumed by the queue owner executor.
///
/// Association and station-removal events are queued without blocking while
/// RX is drained. The same fixed-CPU executor processes them after the RX step,
/// where command waits can cooperatively advance command TX/RX confirmations.
pub struct ApState {
    pub assoc_queue: Mutex<VecDeque<Vec<u8>>>,
    /// Station indices awaiting firmware removal by the owner executor.
    pub sta_del_queue: Mutex<VecDeque<u8>>,
    /// Registered `(MAC, station index, control-port-open, retry count)` rows.
    /// Real AP events drive deduplication and bounded control-port retries; no
    /// timer or periodic reconciliation task owns this state.
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
    pub fn shutdown(self: &Arc<Self>) -> Result<(), sdio_host::error::SdioError> {
        *self.state.lock() = BusState::Down;

        self.transport.mask_card_irq();
        let chip_irq_result =
            self.transport
                .write_byte(1, self.transport.intr_config_reg_addr(), 0x00);
        self.transport.disable_irq();

        self.tx.queue.lock().clear();
        self.tx.completed.lock().clear();
        self.rx.data_queue.lock().clear();
        self.ap.assoc_queue.lock().clear();
        self.cmd.rsp_error.store(true, Ordering::Release);
        self.rx.eapol_queue.lock().clear();
        self.tx.ind_queue.lock().clear();

        log::debug!("[wifi-bus] shutdown complete");
        chip_irq_result
    }
}
