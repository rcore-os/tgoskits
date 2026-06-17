use alloc::{collections::VecDeque, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU32, AtomicUsize, Ordering};

use atomic_waker::AtomicWaker;
use spin::Mutex;

use crate::{
    common::SDIOWIFI_INTR_CONFIG_REG,
    fdrv::core::{pollset::PollSet, sdio_transport::SdioTransport},
};

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
    pub rsp_pollset: PollSet,
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
            rsp_pollset: PollSet::new(),
        }
    }
}

/// RX 状态
pub struct RxState {
    /// SDIO 卡中断唤醒。由 ISR (`sdio1_irq_handler`) 唤醒、RX 线程注册，
    /// 是唯一跨中断/线程共享的唤醒点，故用无锁的 `AtomicWaker`（单 waiter）。
    pub irq_waker: AtomicWaker,
    pub irq_pending: AtomicBool,
    pub data_queue: Mutex<VecDeque<Vec<u8>>>,
    pub data_pollset: PollSet,
    pub eapol_queue: Mutex<VecDeque<Vec<u8>>>,
    pub eapol_pollset: PollSet,
    pub tx_cfm_queue: Mutex<VecDeque<Vec<u8>>>,
    pub tx_cfm_pollset: PollSet,
}

impl Default for RxState {
    fn default() -> Self {
        Self::new()
    }
}

impl RxState {
    pub fn new() -> Self {
        Self {
            irq_waker: AtomicWaker::new(),
            irq_pending: AtomicBool::new(false),
            data_queue: Mutex::new(VecDeque::new()),
            data_pollset: PollSet::new(),
            eapol_queue: Mutex::new(VecDeque::new()),
            eapol_pollset: PollSet::new(),
            tx_cfm_queue: Mutex::new(VecDeque::new()),
            tx_cfm_pollset: PollSet::new(),
        }
    }
}

/// TX 状态
pub struct TxState {
    pub queue: Mutex<VecDeque<TxFrame>>,
    pub pktcnt: AtomicU32,
    pub wake_pollset: PollSet,
    pub ind_queue: Mutex<VecDeque<Vec<u8>>>,
    pub ind_pollset: PollSet,
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
            wake_pollset: PollSet::new(),
            ind_queue: Mutex::new(VecDeque::new()),
            ind_pollset: PollSet::new(),
        }
    }
}

/// AP 模式状态：待处理的关联请求队列。
///
/// RX 线程收到 AssocReq 时把整帧 mpdu 入队(非阻塞)，由独立的 AP worker
/// 线程取出处理(ME_STA_ADD + Assoc Response)。必须用独立线程，因为
/// ME_STA_ADD 走 send_cmd 阻塞等 CFM，而 CFM 由 RX 线程处理 —— 在 RX
/// 线程里 send_cmd 会死锁。
pub struct ApState {
    pub assoc_queue: Mutex<VecDeque<Vec<u8>>>,
    pub assoc_pollset: PollSet,
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
            assoc_pollset: PollSet::new(),
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

    /// 关闭总线，停止所有线程
    pub fn shutdown(self: &Arc<Self>) {
        *self.state.lock() = BusState::Down;

        let _ = self.transport.write_byte(1, SDIOWIFI_INTR_CONFIG_REG, 0x00);
        self.transport.disable_irq();

        self.tx.wake_pollset.wake();
        self.tx.queue.lock().clear();

        self.rx.irq_waker.wake();

        self.rx.data_queue.lock().clear();

        self.ap.assoc_pollset.wake();
        self.ap.assoc_queue.lock().clear();

        self.cmd.rsp_error.store(true, Ordering::Release);
        self.cmd.rsp_pollset.wake();
        self.rx.tx_cfm_pollset.wake();

        self.rx.eapol_pollset.wake();
        self.rx.eapol_queue.lock().clear();

        self.tx.ind_pollset.wake();
        self.tx.ind_queue.lock().clear();

        clear_global_bus();

        log::debug!("[wifi-bus] shutdown complete");
    }
}

/// 全局 WifiBus 引用
static WIFI_BUS_PTR: AtomicUsize = AtomicUsize::new(0);

pub fn set_global_bus(bus: &Arc<WifiBus>) {
    let ptr = Arc::into_raw(Arc::clone(bus));
    let old = WIFI_BUS_PTR.swap(ptr as usize, Ordering::AcqRel);
    if old != 0 {
        unsafe {
            Arc::from_raw(old as *const WifiBus);
        }
    }
}

pub fn get_global_bus() -> Option<&'static WifiBus> {
    let ptr = WIFI_BUS_PTR.load(Ordering::Acquire);
    if ptr == 0 {
        None
    } else {
        unsafe { Some(&*(ptr as *const WifiBus)) }
    }
}

pub fn clear_global_bus() {
    let old = WIFI_BUS_PTR.swap(0, Ordering::AcqRel);
    if old != 0 {
        unsafe {
            Arc::from_raw(old as *const WifiBus);
        }
    }
}

use core::sync::atomic::AtomicU64;
pub(crate) static IRQ_COUNT: AtomicU64 = AtomicU64::new(0);

/// PLIC IRQ #38 处理函数
///
/// 约束：不持锁、不分配堆、不调度。仅操作 Atomic + mask_card_irq + waker.wake()
pub fn sdio1_irq_handler() {
    IRQ_COUNT.fetch_add(1, Ordering::Relaxed);

    let Some(bus) = get_global_bus() else { return };

    // 屏蔽 CARD_INT，防止电平触发导致 ISR 重入
    bus.transport.mask_card_irq();

    bus.rx.irq_pending.store(true, Ordering::Release);
    bus.rx.irq_waker.wake();
}
