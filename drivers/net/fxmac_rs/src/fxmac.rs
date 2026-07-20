//! Core FXMAC Ethernet controller functionality.
//!
//! This module provides the main data structures and functions for controlling
//! the FXMAC Ethernet MAC controller.

use alloc::sync::Arc;
use core::{
    fmt,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};

use crate::{fxmac_const::*, fxmac_dma::*, fxmac_intr::*, fxmac_phy::*, utils::*};

/// Handler type for DMA send (TX) interrupts.
pub const FXMAC_HANDLER_DMASEND: u32 = 1;
/// Handler type for DMA receive (RX) interrupts.
pub const FXMAC_HANDLER_DMARECV: u32 = 2;
/// Handler type for error interrupts.
pub const FXMAC_HANDLER_ERROR: u32 = 3;
/// Handler type for link status change interrupts.
pub const FXMAC_HANDLER_LINKCHANGE: u32 = 4;
/// Handler type for TX descriptor queue restart.
pub const FXMAC_HANDLER_RESTART: u32 = 5;

/// Link status: down.
pub const FXMAC_LINKDOWN: u32 = 0;
/// Link status: up.
pub const FXMAC_LINKUP: u32 = 1;
/// Link status: negotiating.
pub const FXMAC_NEGOTIATING: u32 = 2;

/// FXMAC0 peripheral clock frequency in Hz.
pub const FXMAC0_PCLK: u32 = 50000000;
/// FXMAC0 hotplug IRQ number.
pub const FXMAC0_HOTPLUG_IRQ_NUM: u32 = 53 + 30;
/// Maximum number of hardware queues supported.
pub const FXMAC_QUEUE_MAX_NUM: u32 = 4;

/// Mask for upper 32 bits of 64-bit address.
pub const ULONG64_HI_MASK: u64 = 0xFFFFFFFF00000000;
/// Mask for lower 32 bits of 64-bit address.
pub const ULONG64_LO_MASK: u64 = !ULONG64_HI_MASK;

/// Component is initialized and ready.
pub const FT_COMPONENT_IS_READY: u32 = 0x11111111;
/// Component is started.
pub const FT_COMPONENT_IS_STARTED: u32 = 0x22222222;

/// Memory page size in bytes.
pub const PAGE_SIZE: usize = 4096;
/// Default physical base of the first FXMAC controller.
pub const FXMAC_MMIO_PHYS_BASE: usize = 0x3200_c000;
/// Minimum register aperture required by this driver.
pub const FXMAC_MMIO_SIZE: usize = 0x2000;
/// Exact queue-0 sources used by the runtime data path.
pub const FXMAC_RUNTIME_IRQ_MASK: u32 = FXMAC_INTR_MASK;

const FXMAC_MODE_SELECT_OFFSET: usize = 0x1c00;
const FXMAC_LOOPBACK_SELECT_OFFSET: usize = 0x1c04;

/// Invalid mapped-register capability supplied at discovery time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FXmacDiscoveryError {
    /// The virtual register base is not aligned for 32-bit accesses.
    MisalignedBase,
    /// The mapped range does not cover every register used by the driver.
    RangeTooSmall { provided: usize, required: usize },
    /// Computing the end of the mapped range overflowed `usize`.
    AddressOverflow,
}

impl fmt::Display for FXmacDiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MisalignedBase => write!(f, "FXMAC register mapping is not u32-aligned"),
            Self::RangeTooSmall { provided, required } => write!(
                f,
                "FXMAC register mapping is too small: {provided:#x} < {required:#x}"
            ),
            Self::AddressOverflow => write!(f, "FXMAC register mapping address overflows usize"),
        }
    }
}

impl core::error::Error for FXmacDiscoveryError {}

/// Register role retained exclusively by the maintenance owner.
struct FXmacOwnerRegisters {
    base: NonNull<u8>,
    size: usize,
}

// SAFETY: the mapping lease outlives this role and the value is moved exactly
// once into the CPU-pinned maintenance owner.
unsafe impl Send for FXmacOwnerRegisters {}

impl FXmacOwnerRegisters {
    fn base_address(&self) -> u64 {
        self.base.as_ptr() as usize as u64
    }

    fn write(&self, offset: u64, value: u32) {
        debug_assert!(offset as usize + core::mem::size_of::<u32>() <= self.size);
        unsafe {
            // SAFETY: discovery validated this complete, aligned aperture.
            // Only the maintenance owner retains this non-copyable role.
            self.base
                .add(offset as usize)
                .cast::<u32>()
                .write_volatile(value);
        }
    }
}

/// Destructive status/containment role moved exclusively into the IRQ action.
struct FXmacIrqRegisters {
    base: NonNull<u8>,
    size: usize,
}

// SAFETY: the mapping lease outlives the registered callback and the role is
// moved, never cloned, into one same-CPU IRQ action.
unsafe impl Send for FXmacIrqRegisters {}

impl FXmacIrqRegisters {
    fn read_status(&self) -> u32 {
        self.read(FXMAC_ISR_OFFSET)
    }

    fn mask_runtime_sources(&mut self) {
        self.write(FXMAC_IDR_OFFSET, FXMAC_RUNTIME_IRQ_MASK);
    }

    fn acknowledge_runtime_status(&mut self, status: u32) {
        self.write(FXMAC_ISR_OFFSET, status & FXMAC_RUNTIME_IRQ_MASK);
    }

    fn read(&self, offset: u64) -> u32 {
        debug_assert!(offset as usize + core::mem::size_of::<u32>() <= self.size);
        unsafe {
            // SAFETY: discovery validated this complete, aligned aperture and
            // the move-only IRQ role is the sole runtime ISR reader.
            self.base.add(offset as usize).cast::<u32>().read_volatile()
        }
    }

    fn write(&mut self, offset: u64, value: u32) {
        debug_assert!(offset as usize + core::mem::size_of::<u32>() <= self.size);
        unsafe {
            // SAFETY: the same mapped-range and unique IRQ-role invariants as
            // read() cover these bounded ISR/IDR writes.
            self.base
                .add(offset as usize)
                .cast::<u32>()
                .write_volatile(value);
        }
    }
}

fn validate_mapped_registers(base: NonNull<u8>, size: usize) -> Result<(), FXmacDiscoveryError> {
    if !(base.as_ptr() as usize).is_multiple_of(core::mem::align_of::<u32>()) {
        return Err(FXmacDiscoveryError::MisalignedBase);
    }
    if size < FXMAC_MMIO_SIZE {
        return Err(FXmacDiscoveryError::RangeTooSmall {
            provided: size,
            required: FXMAC_MMIO_SIZE,
        });
    }
    (base.as_ptr() as usize)
        .checked_add(size)
        .ok_or(FXmacDiscoveryError::AddressOverflow)?;
    Ok(())
}

struct FXmacIrqConfig {
    clear_on_write: AtomicBool,
}

/// Discovery-only controller state. It performs no reset, PHY, or DMA work.
pub struct FXmacPending {
    registers: FXmacOwnerRegisters,
    irq_config: Arc<FXmacIrqConfig>,
}

// SAFETY: the pending capability is moved exactly once into the CPU-pinned
// maintenance owner. Its OS mapping lease remains alive in the adapter.
unsafe impl Send for FXmacPending {}

impl FXmacPending {
    /// Keeps the exact runtime sources disabled before IRQ registration.
    pub fn disable_irq(&mut self) {
        self.registers
            .write(FXMAC_IDR_OFFSET, FXMAC_RUNTIME_IRQ_MASK);
    }
}

/// Discovery result split between the owner thread and hard-IRQ action.
pub struct FXmacDiscovery {
    pending: FXmacPending,
    irq_port: FXmacIrqPort,
}

impl FXmacDiscovery {
    /// Separates the controller initialization capability from IRQ capture.
    pub fn into_parts(self) -> (FXmacPending, FXmacIrqPort) {
        (self.pending, self.irq_port)
    }
}

/// Validates one already-mapped FXMAC register aperture without touching it.
///
/// # Safety
///
/// `mapped_base..mapped_base + mapped_size` must remain a readable and
/// writable device mapping until both returned capabilities have been closed.
/// The caller must retain the unique mapping lease for that whole lifetime.
pub unsafe fn discover_xmac(
    mapped_base: NonNull<u8>,
    mapped_size: usize,
) -> Result<FXmacDiscovery, FXmacDiscoveryError> {
    validate_mapped_registers(mapped_base, mapped_size)?;
    let irq_config = Arc::new(FXmacIrqConfig {
        clear_on_write: AtomicBool::new(false),
    });
    Ok(FXmacDiscovery {
        pending: FXmacPending {
            registers: FXmacOwnerRegisters {
                base: mapped_base,
                size: mapped_size,
            },
            irq_config: Arc::clone(&irq_config),
        },
        irq_port: FXmacIrqPort {
            registers: FXmacIrqRegisters {
                base: mapped_base,
                size: mapped_size,
            },
            irq_config,
        },
    })
}

/// Snapshot of the controller interrupt status read by the IRQ endpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FXmacIrqStatus {
    raw: u32,
}

impl FXmacIrqStatus {
    pub const fn from_raw(raw: u32) -> Self {
        Self { raw }
    }

    pub const fn raw(self) -> u32 {
        self.raw
    }

    pub const fn is_empty(self) -> bool {
        self.raw == 0
    }

    pub const fn tx_ready(self) -> bool {
        (self.raw & (FXMAC_IXR_TXCOMPL_MASK | FXMAC_IXR_TX_ERR_MASK | FXMAC_IXR_TXUSED_MASK)) != 0
    }

    pub const fn rx_ready(self) -> bool {
        (self.raw & (FXMAC_IXR_RXCOMPL_MASK | FXMAC_IXR_RX_ERR_MASK)) != 0
    }
}

/// Move-only register capability owned by the OS hard-IRQ action.
///
/// This endpoint is deliberately detached from [`FXmac`]'s descriptor and
/// queue state. The maintenance owner may therefore exclude local IRQs while
/// it mutates that state without making the hard handler acquire the same
/// task-owned lock. Only this value reads and acknowledges the destructive
/// queue-0 interrupt status during normal runtime operation.
pub struct FXmacIrqPort {
    registers: FXmacIrqRegisters,
    irq_config: Arc<FXmacIrqConfig>,
}

impl FXmacIrqPort {
    /// Captures one stable queue-0 IRQ snapshot and leaves its source masked.
    ///
    /// An empty status leaves the source unchanged so a shared interrupt can
    /// be reported as unhandled. A non-empty status is acknowledged before
    /// returning and can only be rearmed by the maintenance owner.
    pub fn capture_and_mask(&mut self) -> FXmacIrqStatus {
        let status =
            FXmacIrqStatus::from_raw(self.registers.read_status() & FXMAC_RUNTIME_IRQ_MASK);
        if status.is_empty() {
            return status;
        }

        self.mask();
        if self.irq_config.clear_on_write.load(Ordering::Acquire) {
            self.registers.acknowledge_runtime_status(status.raw());
        }
        status
    }

    /// Masks the exact queue-0 device interrupt source.
    pub fn mask(&mut self) {
        self.registers.mask_runtime_sources();
    }
}

/// Main FXMAC Ethernet controller instance.
///
/// This structure holds all state information for an FXMAC controller instance,
/// including configuration, DMA queues, and runtime status.
///
/// # Thread Safety
///
/// This structure is `Send` so the complete controller can be moved into its
/// maintenance owner. It is intentionally not `Sync`; queue and control access
/// must stay serialized by that owner rather than being called concurrently.
///
/// # Example
///
/// ```ignore
/// let hwaddr: [u8; 6] = [0x55, 0x44, 0x33, 0x22, 0x11, 0x00];
/// let mut fxmac = xmac_init(pending);
///
/// // Check link status
/// if fxmac.link_status == FXMAC_LINKUP {
///     println!("Network link is up!");
/// }
/// ```
pub struct FXmac {
    registers: FXmacOwnerRegisters,
    /// Hardware configuration settings.
    pub config: FXmacConfig,
    /// Device initialization state (FT_COMPONENT_IS_READY when initialized).
    pub is_ready: u32,
    /// Device running state (FT_COMPONENT_IS_STARTED when active).
    pub is_started: u32,
    /// Current link status (FXMAC_LINKUP, FXMAC_LINKDOWN, or FXMAC_NEGOTIATING).
    pub link_status: u32,
    /// Currently enabled MAC options.
    pub options: u32,
    /// Interrupt mask for enabled interrupts.
    pub mask: u32,
    /// Capability mask bits.
    pub caps: u32,
    /// Network buffer management (lwIP port compatibility).
    pub lwipport: FXmacLwipPort,
    /// Transmit buffer descriptor queue.
    pub tx_bd_queue: FXmacQueue,
    /// Receive buffer descriptor queue.
    pub rx_bd_queue: FXmacQueue,
    /// Hardware module identification number.
    pub moudle_id: u32,
    /// Maximum transmission unit size.
    pub max_mtu_size: u32,
    /// Maximum frame size including headers.
    pub max_frame_size: u32,
    /// PHY address on the MDIO bus.
    pub phy_address: u32,
    /// Receive buffer mask for speed settings.
    pub rxbuf_mask: u32,
}

// SAFETY: FXmac can be sent between threads as long as proper synchronization
// is used for concurrent access.
unsafe impl Send for FXmac {}

/// Hardware configuration for the FXMAC controller.
///
/// This structure contains all hardware-level configuration parameters
/// required to initialize and operate the FXMAC Ethernet controller.
pub struct FXmacConfig {
    /// Instance identifier for multi-controller setups.
    pub instance_id: u32,
    /// Mapped register addresses are crate-private implementation details.
    /// External users must not manufacture an alias to either register role.
    pub(crate) base_address: u64,
    pub(crate) extral_mode_base: u64,
    pub(crate) extral_loopback_base: u64,
    /// PHY interface type (SGMII, RGMII, etc.).
    pub interface: FXmacPhyInterface,
    /// Link speed in Mbps (10, 100, 1000, etc.).
    pub speed: u32,
    /// Duplex mode: 1 for full-duplex, 0 for half-duplex.
    pub duplex: u32,
    /// Auto-negotiation enable: 1 to enable, 0 to disable.
    pub auto_neg: u32,
    /// Peripheral clock frequency in Hz.
    pub pclk_hz: u32,
    /// Maximum number of hardware queues.
    pub max_queue_num: u32,
    /// TX queue index (0 to FXMAC_QUEUE_MAX_NUM-1).
    pub tx_queue_id: u32,
    /// RX queue index (0 to FXMAC_QUEUE_MAX_NUM-1).
    pub rx_queue_id: u32,
    /// Hotplug IRQ number.
    pub hotplug_irq_num: u32,
    /// DMA burst length setting.
    pub dma_brust_length: u32,
    /// Default network configuration options.
    pub network_default_config: u32,
    /// IRQ numbers for each hardware queue.
    pub queue_irq_num: [u32; FXMAC_QUEUE_MAX_NUM as usize],
    /// Capability flags (e.g., tail pointer support).
    pub caps: u32,
    /// MAC address (6 bytes).
    pub mac: [u8; 6],
}

/// Hardware queue structure for TX/RX operations.
pub struct FXmacQueue {
    /// Queue identifier.
    pub queue_id: u32,
    /// Buffer descriptor ring for this queue.
    pub bdring: FXmacBdRing,
}

/// PHY interface mode definitions.
///
/// Specifies the physical layer interface type used for communication
/// between the MAC controller and the PHY chip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FXmacPhyInterface {
    /// SGMII (Serial Gigabit Media Independent Interface).
    FXMAC_PHY_INTERFACE_MODE_SGMII = 0,
    /// RMII (Reduced Media Independent Interface).
    FXMAC_PHY_INTERFACE_MODE_RMII = 1,
    /// RGMII (Reduced Gigabit Media Independent Interface).
    FXMAC_PHY_INTERFACE_MODE_RGMII = 2,
    /// XGMII (10 Gigabit Media Independent Interface).
    FXMAC_PHY_INTERFACE_MODE_XGMII = 3,
    /// USXGMII (Universal Serial 10 Gigabit Media Independent Interface).
    FXMAC_PHY_INTERFACE_MODE_USXGMII = 4,
    /// 5GBASE-R interface mode.
    FXMAC_PHY_INTERFACE_MODE_5GBASER = 5,
    /// 2500BASE-X interface mode.
    FXMAC_PHY_INTERFACE_MODE_2500BASEX = 6,
}

/// Reads a register or DMA descriptor through an already-valid CPU pointer.
///
/// Register pointers are derived from the validated mapped aperture during
/// discovery. Descriptor pointers come from the CPU address returned by the
/// DMA allocator; neither path performs an implicit address translation.
pub(crate) fn read_reg<T>(src: *const T) -> T {
    unsafe { core::ptr::read_volatile(src) }
}

/// Writes a register or DMA descriptor through an already-valid CPU pointer.
pub(crate) fn write_reg<T>(dst: *mut T, value: T) {
    unsafe { core::ptr::write_volatile(dst, value) }
}

const MDIO_POLL_INTERVAL_NS: u64 = 50_000;
const MDIO_OPERATION_TIMEOUT_NS: u64 = 10_000_000;
const PHY_RESET_POLL_INTERVAL_NS: u64 = 1_000_000;
const PHY_RESET_TIMEOUT_NS: u64 = 1_000_000_000;
const PHY_AUTONEG_POLL_INTERVAL_NS: u64 = 50_000_000;
const PHY_AUTONEG_TIMEOUT_NS: u64 = 12_750_000_000;
const PHY_MANUAL_SETTLE_NS: u64 = 1_500_000_000;

/// Scheduling request returned by the portable initialization state machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FXmacInitSchedule {
    /// Whether another pure in-memory transition is immediately available.
    pub run_again: bool,
    /// Absolute monotonic time for the next hardware-state observation.
    pub wake_at_ns: Option<u64>,
}

impl FXmacInitSchedule {
    const fn run_again() -> Self {
        Self {
            run_again: true,
            wake_at_ns: None,
        }
    }

    const fn wait_until(wake_at_ns: u64) -> Self {
        Self {
            run_again: false,
            wake_at_ns: Some(wake_at_ns),
        }
    }
}

/// Terminal FXMAC initialization error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FXmacInitError {
    /// The state machine was polled after returning a terminal result.
    AlreadyFinished,
    /// An MDIO command did not become idle before its absolute deadline.
    MdioTimeout,
    /// No valid PHY identifier was found on the MDIO bus.
    PhyNotFound,
    /// PHY reset did not clear before its absolute deadline.
    PhyResetTimeout,
    /// PHY auto-negotiation did not complete before its absolute deadline.
    AutoNegotiationTimeout,
    /// A bounded driver phase returned a non-zero status code.
    DriverStatus(u32),
}

impl fmt::Display for FXmacInitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyFinished => write!(f, "FXMAC initialization already finished"),
            Self::MdioTimeout => write!(f, "FXMAC MDIO operation timed out"),
            Self::PhyNotFound => write!(f, "FXMAC PHY was not found"),
            Self::PhyResetTimeout => write!(f, "FXMAC PHY reset timed out"),
            Self::AutoNegotiationTimeout => {
                write!(f, "FXMAC PHY auto-negotiation timed out")
            }
            Self::DriverStatus(status) => {
                write!(f, "FXMAC initialization failed with status {status}")
            }
        }
    }
}

impl core::error::Error for FXmacInitError {}

/// Result of one bounded initialization transition.
pub enum FXmacInitPoll {
    /// Initialization completed; the caller may take controller ownership.
    Ready,
    /// The owner must schedule another activation.
    Pending(FXmacInitSchedule),
    /// Initialization failed and the device must remain unpublished.
    Failed(FXmacInitError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MdioKind {
    Read,
    Write(u16),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MdioOperation {
    kind: MdioKind,
    phy_address: u32,
    register: u32,
    issued: bool,
    deadline_ns: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MdioPoll {
    Ready(u16),
    Pending(u64),
    Failed(FXmacInitError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FXmacInitStage {
    ReadMac,
    Reset,
    ConfigureBeforePhy,
    DetectStatus {
        phy_address: u32,
    },
    DetectIdentifier1 {
        phy_address: u32,
    },
    DetectIdentifier2 {
        phy_address: u32,
        id1: u16,
    },
    ResetPhyRead,
    ResetPhyWrite {
        control: u16,
    },
    ResetPhyPoll {
        deadline_ns: u64,
        next_check_ns: u64,
    },
    ManualAdvertiseRead,
    ManualAdvertiseWrite {
        advertise: u16,
    },
    ManualControlRead,
    ManualControlWrite {
        control: u16,
    },
    ManualSettle {
        wake_at_ns: u64,
    },
    ManualStatusRead,
    AutoControlRead,
    AutoControlWrite {
        control: u16,
    },
    AutoPoll {
        deadline_ns: u64,
        next_check_ns: u64,
    },
    AutoStatusRead,
    ConfigureDatapath,
    InitializeDma,
    Start,
    Finished,
}

/// Linear initialization object owned by one CPU-pinned maintenance thread.
pub struct FXmacInitialization {
    device: Option<FXmac>,
    irq_config: Arc<FXmacIrqConfig>,
    stage: FXmacInitStage,
    mdio: Option<MdioOperation>,
}

// SAFETY: initialization is moved into the same CPU-pinned maintenance owner
// as `FXmacPending`; it never exposes the contained register capability.
unsafe impl Send for FXmacInitialization {}

impl FXmacInitialization {
    /// Takes the initialized controller after [`FXmacInitPoll::Ready`].
    pub fn take_ready(&mut self) -> Result<FXmac, FXmacInitError> {
        if self.stage != FXmacInitStage::Finished {
            return Err(FXmacInitError::AlreadyFinished);
        }
        self.device.take().ok_or(FXmacInitError::AlreadyFinished)
    }
}

/// Constructs a legal initialization object without issuing hardware commands.
pub fn begin_xmac_init(pending: FXmacPending) -> FXmacInitialization {
    let FXmacPending {
        registers,
        irq_config,
    } = pending;
    let mapped_base = registers.base_address();
    // FXmacConfig mac_config:
    // mac_config.instance_id=0,
    // mac_config.base_address=0x3200c000,
    // mac_config.extral_mode_base=0x3200dc00,
    // mac_config.extral_loopback_base=0x3200dc04,
    // mac_config.interface=0,
    // mac_config.speed=100,
    // mac_config.duplex=1,
    // mac_config.auto_neg=0,
    // mac_config.pclk_hz=50000000,
    // mac_config.max_queue_num=4,
    // mac_config.tx_queue_id=0,
    // mac_config.rx_queue_id=0
    // mac_config.hotplug_irq_num=83,
    // mac_config.dma_brust_length=16,
    // mac_config.network_default_config=0x37f0,
    // mac_config.queue_irq_num[0]=87,
    // mac_config.caps=0
    let mac_config = FXmacConfig {
        instance_id: FXMAC0_ID,
        base_address: mapped_base,
        extral_mode_base: mapped_base + FXMAC_MODE_SELECT_OFFSET as u64,
        extral_loopback_base: mapped_base + FXMAC_LOOPBACK_SELECT_OFFSET as u64,
        interface: FXmacPhyInterface::FXMAC_PHY_INTERFACE_MODE_SGMII,
        speed: 100,
        duplex: 1,
        auto_neg: 0,
        pclk_hz: FXMAC0_PCLK,
        max_queue_num: 4, // .max_queue_num = 16
        tx_queue_id: 0,
        rx_queue_id: 0,
        hotplug_irq_num: FXMAC0_HOTPLUG_IRQ_NUM,
        dma_brust_length: 16,
        network_default_config: FXMAC_DEFAULT_OPTIONS,
        queue_irq_num: [
            FXMAC0_QUEUE0_IRQ_NUM,
            FXMAC0_QUEUE1_IRQ_NUM,
            FXMAC0_QUEUE2_IRQ_NUM,
            FXMAC0_QUEUE3_IRQ_NUM,
        ],
        caps: 0,
        mac: [0; 6],
    };

    let xmac = FXmac {
        registers,
        config: mac_config,
        is_ready: FT_COMPONENT_IS_READY,
        is_started: 0,
        link_status: FXMAC_LINKDOWN,
        options: 0,
        mask: 0,
        caps: 0,
        lwipport: FXmacLwipPort {
            buffer: FXmacNetifBuffer::default(),
            feature: FXMAC_LWIP_PORT_CONFIG_MULTICAST_ADDRESS_FILITER,
            hwaddr: [0; 6],
            recv_flg: 0,
        },
        tx_bd_queue: FXmacQueue {
            queue_id: 0,
            bdring: FXmacBdRing::default(),
        },
        rx_bd_queue: FXmacQueue {
            queue_id: 0,
            bdring: FXmacBdRing::default(),
        },
        moudle_id: 0,
        max_mtu_size: 0,
        max_frame_size: 0,
        phy_address: 0,
        rxbuf_mask: 0,
    };

    FXmacInitialization {
        device: Some(xmac),
        irq_config,
        stage: FXmacInitStage::ReadMac,
        mdio: None,
    }
}

/// Advances at most one bounded FXMAC initialization transition.
pub fn poll_xmac_init(initialization: &mut FXmacInitialization, now_ns: u64) -> FXmacInitPoll {
    let Some(device) = initialization.device.as_mut() else {
        return FXmacInitPoll::Failed(FXmacInitError::AlreadyFinished);
    };

    let next = match initialization.stage {
        FXmacInitStage::ReadMac => {
            let mut hwaddr = [0; 6];
            FXmacGetMacAddress(device, &mut hwaddr, 0);
            device.config.mac = hwaddr;
            device.lwipport.hwaddr = hwaddr;
            FXmacInitStage::Reset
        }
        FXmacInitStage::Reset => {
            FXmacReset(device);
            FXmacInitStage::ConfigureBeforePhy
        }
        FXmacInitStage::ConfigureBeforePhy => {
            device.mask = FXMAC_RUNTIME_IRQ_MASK;
            if (device.config.caps & FXMAC_CAPS_TAILPTR) != 0 {
                FXmacSetOptions(device, FXMAC_TAIL_PTR_OPTION, 0);
                device.mask &= !FXMAC_IXR_TXUSED_MASK;
            }
            FxmacFeatureSetOptions(device.lwipport.feature, device);
            let status = FXmacSetMacAddress(device, &device.lwipport.hwaddr, 0);
            if status != FT_SUCCESS {
                return FXmacInitPoll::Failed(FXmacInitError::DriverStatus(status));
            }
            if device.config.interface == FXmacPhyInterface::FXMAC_PHY_INTERFACE_MODE_USXGMII {
                FXmacInitStage::ConfigureDatapath
            } else {
                FXmacInitStage::DetectStatus { phy_address: 0 }
            }
        }
        FXmacInitStage::DetectStatus { phy_address } => {
            let status = match poll_mdio(
                device,
                &mut initialization.mdio,
                MdioKind::Read,
                phy_address,
                PHY_STATUS_REG_OFFSET,
                now_ns,
            ) {
                MdioPoll::Ready(status) => status,
                MdioPoll::Pending(wake_at_ns) => return wait_until(wake_at_ns),
                MdioPoll::Failed(error) => return FXmacInitPoll::Failed(error),
            };
            if status == u16::MAX {
                match phy_address.checked_add(1) {
                    Some(next) if next < FXMAC_PHY_MAX_NUM => {
                        FXmacInitStage::DetectStatus { phy_address: next }
                    }
                    _ => return FXmacInitPoll::Failed(FXmacInitError::PhyNotFound),
                }
            } else {
                FXmacInitStage::DetectIdentifier1 { phy_address }
            }
        }
        FXmacInitStage::DetectIdentifier1 { phy_address } => {
            let id1 = match poll_mdio(
                device,
                &mut initialization.mdio,
                MdioKind::Read,
                phy_address,
                PHY_IDENTIFIER_1_REG,
                now_ns,
            ) {
                MdioPoll::Ready(id1) => id1,
                MdioPoll::Pending(wake_at_ns) => return wait_until(wake_at_ns),
                MdioPoll::Failed(error) => return FXmacInitPoll::Failed(error),
            };
            FXmacInitStage::DetectIdentifier2 { phy_address, id1 }
        }
        FXmacInitStage::DetectIdentifier2 { phy_address, id1 } => {
            let id2 = match poll_mdio(
                device,
                &mut initialization.mdio,
                MdioKind::Read,
                phy_address,
                PHY_IDENTIFIER_2_REG,
                now_ns,
            ) {
                MdioPoll::Ready(id2) => id2,
                MdioPoll::Pending(wake_at_ns) => return wait_until(wake_at_ns),
                MdioPoll::Failed(error) => return FXmacInitPoll::Failed(error),
            };
            if id1 != u16::MAX && id2 != 0 && id2 != u16::MAX {
                device.phy_address = phy_address;
                FXmacInitStage::ResetPhyRead
            } else {
                match phy_address.checked_add(1) {
                    Some(next) if next < FXMAC_PHY_MAX_NUM => {
                        FXmacInitStage::DetectStatus { phy_address: next }
                    }
                    _ => return FXmacInitPoll::Failed(FXmacInitError::PhyNotFound),
                }
            }
        }
        FXmacInitStage::ResetPhyRead => {
            let control = match poll_mdio(
                device,
                &mut initialization.mdio,
                MdioKind::Read,
                device.phy_address,
                PHY_CONTROL_REG_OFFSET,
                now_ns,
            ) {
                MdioPoll::Ready(control) => control,
                MdioPoll::Pending(wake_at_ns) => return wait_until(wake_at_ns),
                MdioPoll::Failed(error) => return FXmacInitPoll::Failed(error),
            };
            FXmacInitStage::ResetPhyWrite {
                control: control | PHY_CONTROL_RESET_MASK,
            }
        }
        FXmacInitStage::ResetPhyWrite { control } => {
            match poll_mdio(
                device,
                &mut initialization.mdio,
                MdioKind::Write(control),
                device.phy_address,
                PHY_CONTROL_REG_OFFSET,
                now_ns,
            ) {
                MdioPoll::Ready(_) => {}
                MdioPoll::Pending(wake_at_ns) => return wait_until(wake_at_ns),
                MdioPoll::Failed(error) => return FXmacInitPoll::Failed(error),
            }
            FXmacInitStage::ResetPhyPoll {
                deadline_ns: now_ns.saturating_add(PHY_RESET_TIMEOUT_NS),
                next_check_ns: now_ns.saturating_add(PHY_RESET_POLL_INTERVAL_NS),
            }
        }
        FXmacInitStage::ResetPhyPoll {
            deadline_ns,
            next_check_ns,
        } => {
            if now_ns >= deadline_ns {
                return FXmacInitPoll::Failed(FXmacInitError::PhyResetTimeout);
            }
            if now_ns < next_check_ns {
                return wait_until(next_check_ns);
            }
            let control = match poll_mdio(
                device,
                &mut initialization.mdio,
                MdioKind::Read,
                device.phy_address,
                PHY_CONTROL_REG_OFFSET,
                now_ns,
            ) {
                MdioPoll::Ready(control) => control,
                MdioPoll::Pending(wake_at_ns) => return wait_until(wake_at_ns.min(deadline_ns)),
                MdioPoll::Failed(error) => return FXmacInitPoll::Failed(error),
            };
            if (control & PHY_CONTROL_RESET_MASK) != 0 {
                FXmacInitStage::ResetPhyPoll {
                    deadline_ns,
                    next_check_ns: now_ns.saturating_add(PHY_RESET_POLL_INTERVAL_NS),
                }
            } else if device.config.auto_neg != 0 {
                FXmacInitStage::AutoControlRead
            } else {
                FXmacInitStage::ManualAdvertiseRead
            }
        }
        FXmacInitStage::ManualAdvertiseRead => {
            let advertise = match poll_mdio(
                device,
                &mut initialization.mdio,
                MdioKind::Read,
                device.phy_address,
                PHY_AUTONEGO_ADVERTISE_REG,
                now_ns,
            ) {
                MdioPoll::Ready(advertise) => advertise,
                MdioPoll::Pending(wake_at_ns) => return wait_until(wake_at_ns),
                MdioPoll::Failed(error) => return FXmacInitPoll::Failed(error),
            };
            FXmacInitStage::ManualAdvertiseWrite {
                advertise: advertise
                    | PHY_AUTOADVERTISE_ASYMMETRIC_PAUSE_MASK
                    | PHY_AUTOADVERTISE_PAUSE_MASK,
            }
        }
        FXmacInitStage::ManualAdvertiseWrite { advertise } => {
            match poll_mdio(
                device,
                &mut initialization.mdio,
                MdioKind::Write(advertise),
                device.phy_address,
                PHY_AUTONEGO_ADVERTISE_REG,
                now_ns,
            ) {
                MdioPoll::Ready(_) => FXmacInitStage::ManualControlRead,
                MdioPoll::Pending(wake_at_ns) => return wait_until(wake_at_ns),
                MdioPoll::Failed(error) => return FXmacInitPoll::Failed(error),
            }
        }
        FXmacInitStage::ManualControlRead => {
            let control = match poll_mdio(
                device,
                &mut initialization.mdio,
                MdioKind::Read,
                device.phy_address,
                PHY_CONTROL_REG_OFFSET,
                now_ns,
            ) {
                MdioPoll::Ready(control) => control,
                MdioPoll::Pending(wake_at_ns) => return wait_until(wake_at_ns),
                MdioPoll::Failed(error) => return FXmacInitPoll::Failed(error),
            };
            FXmacInitStage::ManualControlWrite {
                control: manual_phy_control(control, device.config.speed, device.config.duplex),
            }
        }
        FXmacInitStage::ManualControlWrite { control } => {
            match poll_mdio(
                device,
                &mut initialization.mdio,
                MdioKind::Write(control),
                device.phy_address,
                PHY_CONTROL_REG_OFFSET,
                now_ns,
            ) {
                MdioPoll::Ready(_) => {}
                MdioPoll::Pending(wake_at_ns) => return wait_until(wake_at_ns),
                MdioPoll::Failed(error) => return FXmacInitPoll::Failed(error),
            }
            FXmacInitStage::ManualSettle {
                wake_at_ns: now_ns.saturating_add(PHY_MANUAL_SETTLE_NS),
            }
        }
        FXmacInitStage::ManualSettle { wake_at_ns } => {
            if now_ns < wake_at_ns {
                return wait_until(wake_at_ns);
            }
            FXmacInitStage::ManualStatusRead
        }
        FXmacInitStage::ManualStatusRead => {
            let status = match poll_mdio(
                device,
                &mut initialization.mdio,
                MdioKind::Read,
                device.phy_address,
                PHY_SPECIFIC_STATUS_REG,
                now_ns,
            ) {
                MdioPoll::Ready(status) => status,
                MdioPoll::Pending(wake_at_ns) => return wait_until(wake_at_ns),
                MdioPoll::Failed(error) => return FXmacInitPoll::Failed(error),
            };
            apply_phy_status(device, status);
            FXmacInitStage::ConfigureDatapath
        }
        FXmacInitStage::AutoControlRead => {
            let control = match poll_mdio(
                device,
                &mut initialization.mdio,
                MdioKind::Read,
                device.phy_address,
                PHY_CONTROL_REG_OFFSET,
                now_ns,
            ) {
                MdioPoll::Ready(control) => control,
                MdioPoll::Pending(wake_at_ns) => return wait_until(wake_at_ns),
                MdioPoll::Failed(error) => return FXmacInitPoll::Failed(error),
            };
            FXmacInitStage::AutoControlWrite {
                control: control
                    | PHY_CONTROL_AUTONEGOTIATE_ENABLE
                    | PHY_CONTROL_AUTONEGOTIATE_RESTART,
            }
        }
        FXmacInitStage::AutoControlWrite { control } => {
            match poll_mdio(
                device,
                &mut initialization.mdio,
                MdioKind::Write(control),
                device.phy_address,
                PHY_CONTROL_REG_OFFSET,
                now_ns,
            ) {
                MdioPoll::Ready(_) => {}
                MdioPoll::Pending(wake_at_ns) => return wait_until(wake_at_ns),
                MdioPoll::Failed(error) => return FXmacInitPoll::Failed(error),
            }
            FXmacInitStage::AutoPoll {
                deadline_ns: now_ns.saturating_add(PHY_AUTONEG_TIMEOUT_NS),
                next_check_ns: now_ns.saturating_add(PHY_AUTONEG_POLL_INTERVAL_NS),
            }
        }
        FXmacInitStage::AutoPoll {
            deadline_ns,
            next_check_ns,
        } => {
            if now_ns >= deadline_ns {
                return FXmacInitPoll::Failed(FXmacInitError::AutoNegotiationTimeout);
            }
            if now_ns < next_check_ns {
                return wait_until(next_check_ns);
            }
            let status = match poll_mdio(
                device,
                &mut initialization.mdio,
                MdioKind::Read,
                device.phy_address,
                PHY_STATUS_REG_OFFSET,
                now_ns,
            ) {
                MdioPoll::Ready(status) => status,
                MdioPoll::Pending(wake_at_ns) => return wait_until(wake_at_ns.min(deadline_ns)),
                MdioPoll::Failed(error) => return FXmacInitPoll::Failed(error),
            };
            if (status & PHY_STATUS_AUTONEGOTIATE_COMPLETE) != 0 {
                FXmacInitStage::AutoStatusRead
            } else {
                FXmacInitStage::AutoPoll {
                    deadline_ns,
                    next_check_ns: now_ns.saturating_add(PHY_AUTONEG_POLL_INTERVAL_NS),
                }
            }
        }
        FXmacInitStage::AutoStatusRead => {
            let status = match poll_mdio(
                device,
                &mut initialization.mdio,
                MdioKind::Read,
                device.phy_address,
                PHY_SPECIFIC_STATUS_REG,
                now_ns,
            ) {
                MdioPoll::Ready(status) => status,
                MdioPoll::Pending(wake_at_ns) => return wait_until(wake_at_ns),
                MdioPoll::Failed(error) => return FXmacInitPoll::Failed(error),
            };
            apply_phy_status(device, status);
            FXmacInitStage::ConfigureDatapath
        }
        FXmacInitStage::ConfigureDatapath => {
            FXmacSelectClk(device);
            FXmacInitInterface(device);
            let mut dma = read_reg((device.config.base_address + FXMAC_DMACR_OFFSET) as *const u32);
            dma &= !FXMAC_DMACR_BLENGTH_MASK;
            dma |= FXMAC_DMACR_INCR16_AHB_AXI_BURST;
            write_reg(
                (device.config.base_address + FXMAC_DMACR_OFFSET) as *mut u32,
                dma,
            );
            FXmacInitStage::InitializeDma
        }
        FXmacInitStage::InitializeDma => {
            let status = FXmacInitDma(device);
            if status != FT_SUCCESS {
                return FXmacInitPoll::Failed(FXmacInitError::DriverStatus(status));
            }
            if (device.lwipport.feature & FXMAC_LWIP_PORT_CONFIG_UNICAST_ADDRESS_FILITER) != 0 {
                let hwaddr = device.lwipport.hwaddr;
                let status = FXmac_SetHash(device, &hwaddr);
                if status != FT_SUCCESS {
                    return FXmacInitPoll::Failed(FXmacInitError::DriverStatus(status));
                }
            }
            FXmacInitStage::Start
        }
        FXmacInitStage::Start => {
            FXmacStart(device);
            initialization.irq_config.clear_on_write.store(
                (device.caps & FXMAC_CAPS_ISR_CLEAR_ON_WRITE) != 0,
                Ordering::Release,
            );
            initialization.stage = FXmacInitStage::Finished;
            return FXmacInitPoll::Ready;
        }
        FXmacInitStage::Finished => {
            return FXmacInitPoll::Failed(FXmacInitError::AlreadyFinished);
        }
    };

    initialization.stage = next;
    FXmacInitPoll::Pending(FXmacInitSchedule::run_again())
}

fn wait_until(wake_at_ns: u64) -> FXmacInitPoll {
    FXmacInitPoll::Pending(FXmacInitSchedule::wait_until(wake_at_ns))
}

fn poll_mdio(
    device: &mut FXmac,
    slot: &mut Option<MdioOperation>,
    kind: MdioKind,
    phy_address: u32,
    register: u32,
    now_ns: u64,
) -> MdioPoll {
    let operation = slot.get_or_insert(MdioOperation {
        kind,
        phy_address,
        register,
        issued: false,
        deadline_ns: now_ns.saturating_add(MDIO_OPERATION_TIMEOUT_NS),
    });
    if operation.kind != kind
        || operation.phy_address != phy_address
        || operation.register != register
    {
        return MdioPoll::Failed(FXmacInitError::DriverStatus(6));
    }
    if now_ns >= operation.deadline_ns {
        *slot = None;
        return MdioPoll::Failed(FXmacInitError::MdioTimeout);
    }

    let idle = (read_reg((device.config.base_address + FXMAC_NWSR_OFFSET) as *const u32)
        & FXMAC_NWSR_MDIOIDLE_MASK)
        != 0;
    if !operation.issued {
        if !idle {
            return MdioPoll::Pending(
                now_ns
                    .saturating_add(MDIO_POLL_INTERVAL_NS)
                    .min(operation.deadline_ns),
            );
        }
        let kind_bits = match kind {
            MdioKind::Read => FXMAC_PHYMNTNC_OP_R_MASK,
            MdioKind::Write(_) => FXMAC_PHYMNTNC_OP_W_MASK,
        };
        let payload = match kind {
            MdioKind::Read => 0,
            MdioKind::Write(value) => u32::from(value),
        };
        let command = FXMAC_PHYMNTNC_OP_MASK
            | kind_bits
            | (phy_address << FXMAC_PHYMNTNC_PHAD_SHFT_MSK)
            | (register << FXMAC_PHYMNTNC_PREG_SHFT_MSK)
            | payload;
        write_reg(
            (device.config.base_address + FXMAC_PHYMNTNC_OFFSET) as *mut u32,
            command,
        );
        operation.issued = true;
        return MdioPoll::Pending(
            now_ns
                .saturating_add(MDIO_POLL_INTERVAL_NS)
                .min(operation.deadline_ns),
        );
    }
    if !idle {
        return MdioPoll::Pending(
            now_ns
                .saturating_add(MDIO_POLL_INTERVAL_NS)
                .min(operation.deadline_ns),
        );
    }

    let result = match kind {
        MdioKind::Read => {
            read_reg((device.config.base_address + FXMAC_PHYMNTNC_OFFSET) as *const u32) as u16
        }
        MdioKind::Write(_) => 0,
    };
    *slot = None;
    MdioPoll::Ready(result)
}

fn manual_phy_control(mut control: u16, speed: u32, duplex: u32) -> u16 {
    control &= !(PHY_CONTROL_LINKSPEED_1000M
        | PHY_CONTROL_LINKSPEED_100M
        | PHY_CONTROL_LINKSPEED_10M
        | PHY_CONTROL_AUTONEGOTIATE_ENABLE
        | PHY_CONTROL_AUTONEGOTIATE_RESTART);
    match speed {
        FXMAC_SPEED_100 => control |= PHY_CONTROL_LINKSPEED_100M,
        FXMAC_SPEED_10 => control |= PHY_CONTROL_LINKSPEED_10M,
        _ => {}
    }
    if duplex == FXMAC_PHY_MODE_FULLDUPLEX {
        control |= PHY_CONTROL_FULL_DUPLEX_MASK;
    } else {
        control &= !PHY_CONTROL_FULL_DUPLEX_MASK;
    }
    control
}

fn apply_phy_status(device: &mut FXmac, status: u16) {
    device.config.duplex = u32::from((status & (1 << 13)) != 0);
    device.config.speed = match status & 0xc000 {
        PHY_SPECIFIC_STATUS_SPEED_1000M => FXMAC_SPEED_1000,
        PHY_SPECIFIC_STATUS_SPEED_100M => FXMAC_SPEED_100,
        _ => FXMAC_SPEED_10,
    };
    device.link_status = FXMAC_LINKUP;
}

impl FXmac {
    /// Returns the controller-local hardware IRQ line for queue 0.
    pub fn irq_hwirq(&self) -> u32 {
        self.config.queue_irq_num[0]
    }

    /// Advances queue/error state from a status captured by the IRQ endpoint.
    ///
    /// This may invoke the driver's receive/send handlers and therefore must
    /// run only in the maintenance owner thread, never in hard IRQ context.
    pub fn service_irq_status(&mut self, status: FXmacIrqStatus) {
        if !status.is_empty() {
            crate::fxmac_intr::FXmacIntrHandlerWithStatus(self.irq_hwirq() as i32, self, status);
        }
    }

    /// Enables queue 0 interrupts after the OS has registered an IRQ handler.
    pub fn enable_irq(&mut self) {
        self.registers
            .write(FXMAC_IER_OFFSET, FXMAC_RUNTIME_IRQ_MASK);
    }

    /// Disables queue 0 interrupts while the OS mutates shared device state.
    pub fn disable_irq(&mut self) {
        self.registers
            .write(FXMAC_IDR_OFFSET, FXMAC_RUNTIME_IRQ_MASK);
    }
}

/// Starts the Ethernet controller.
///
/// This enables TX/RX paths based on configured options and starts DMA
/// channels. Interrupt sources remain masked until the OS maintenance owner
/// registers its action and calls [`FXmac::enable_irq`].
///
/// # Panics
///
/// Panics if the instance is not in the ready state.
pub fn FXmacStart(instance_p: &mut FXmac) {
    assert!(instance_p.is_ready == FT_COMPONENT_IS_READY);

    // ISR is a destructive register owned exclusively by FXmacIrqPort. Any
    // status accumulated while the source was masked is captured after the OS
    // registers and enables that endpoint; the owner never acknowledges it.

    // Enable transmitter if not already enabled
    if (instance_p.config.network_default_config & FXMAC_TRANSMITTER_ENABLE_OPTION) != 0 {
        let reg_val =
            read_reg((instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *const u32);
        if (reg_val & FXMAC_NWCTRL_TXEN_MASK) == 0 {
            write_reg(
                (instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *mut u32,
                reg_val | FXMAC_NWCTRL_TXEN_MASK,
            );
        }
    }

    // Enable receiver if not already enabled
    if (instance_p.config.network_default_config & FXMAC_RECEIVER_ENABLE_OPTION) != 0 {
        let reg_val =
            read_reg((instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *const u32);
        info!("Enable receiver, FXMAC_NWCTRL_OFFSET = {:#x}", reg_val);
        if (reg_val & FXMAC_NWCTRL_RXEN_MASK) == 0 {
            write_reg(
                (instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *mut u32,
                reg_val | FXMAC_NWCTRL_RXEN_MASK,
            );
        }
    }
    info!(
        "FXMAC_NWCTRL_OFFSET = {:#x}",
        read_reg((instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *const u32)
    );

    // Mark as started
    instance_p.is_started = FT_COMPONENT_IS_STARTED;
}

/// Gracefully stops the Ethernet MAC.
///
/// This disables interrupts, stops DMA channels, and shuts down TX/RX paths.
///
/// # Panics
///
/// Panics if the instance is not in the ready state.
pub fn FXmacStop(instance_p: &mut FXmac) {
    assert!(instance_p.is_ready == FT_COMPONENT_IS_READY);
    // Disable all interrupts
    write_reg(
        (instance_p.config.base_address + FXMAC_IDR_OFFSET) as *mut u32,
        FXMAC_IXR_ALL_MASK,
    );

    // Disable the receiver & transmitter
    let mut reg_val: u32 =
        read_reg((instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *const u32);
    reg_val &= !FXMAC_NWCTRL_RXEN_MASK;
    reg_val &= !FXMAC_NWCTRL_TXEN_MASK;
    write_reg(
        (instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *mut u32,
        reg_val,
    );

    // Mark as stopped
    instance_p.is_started = 0;
}

// Perform a graceful reset of the Ethernet MAC. Resets the DMA channels, the
// transmitter, and the receiver.
//
// Steps to reset
// - Stops transmit and receive channels
// - Stops DMA
// - Configure transmit and receive buffer size to default
// - Clear transmit and receive status register and counters
// - Clear all interrupt sources
// - Clear phy (if there is any previously detected) address
// - Clear MAC addresses (1-4) as well as Type IDs and hash value
//

fn FXmacReset(instance_p: &mut FXmac) {
    let mut mac_addr: [u8; 6] = [0; 6];

    // Stop the device and reset hardware
    FXmacStop(instance_p);

    // Module identification number
    // instance_p->moudle_id = 12
    instance_p.moudle_id =
        (read_reg((instance_p.config.base_address + FXMAC_REVISION_REG_OFFSET) as *const u32)
            & FXMAC_IDENTIFICATION_MASK)
            >> 16;
    info!(
        "FXmacReset, Got Moudle IDENTIFICATION: {}",
        instance_p.moudle_id
    );

    instance_p.max_mtu_size = FXMAC_MTU;
    instance_p.max_frame_size = FXMAC_MAX_FRAME_SIZE;
    instance_p.config.max_queue_num = 16;
    instance_p.config.dma_brust_length = 16;
    instance_p.config.network_default_config = FXMAC_DEFAULT_OPTIONS;

    instance_p.config.pclk_hz = FXMAC0_PCLK; // 50000000

    let netctrl =
        (FXMAC_NWCTRL_STATCLR_MASK & !FXMAC_NWCTRL_LOOPBACK_LOCAL_MASK) | FXMAC_NWCTRL_MDEN_MASK;
    write_reg(
        (instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *mut u32,
        netctrl,
    );

    FXmacConfigureCaps(instance_p);

    // mdio clock division
    let mut w_reg: u32 = FXmacClkDivGet(instance_p);
    // DMA bus width, DMA位宽为128
    w_reg |= FXmacDmaWidth(instance_p);
    write_reg(
        (instance_p.config.base_address + FXMAC_NWCFG_OFFSET) as *mut u32,
        w_reg,
    );

    FXmacDmaReset(instance_p);

    // This register, when read provides details of the status of the receive path.
    write_reg(
        (instance_p.config.base_address + FXMAC_RXSR_OFFSET) as *mut u32,
        FXMAC_SR_ALL_MASK,
    );

    // write 1 ro the relavant bit location disable that particular interrupt
    write_reg(
        (instance_p.config.base_address + FXMAC_IDR_OFFSET) as *mut u32,
        FXMAC_IXR_ALL_MASK,
    );

    write_reg(
        (instance_p.config.base_address + FXMAC_TXSR_OFFSET) as *mut u32,
        FXMAC_SR_ALL_MASK,
    );

    FXmacClearHash(instance_p);

    // set default mac address
    for i in 0..4 {
        FXmacSetMacAddress(instance_p, &mac_addr, i);
        FXmacGetMacAddress(instance_p, &mut mac_addr, i);
        FXmacSetTypeIdCheck(instance_p, 0, i);
    }

    // clear all counters
    for i in 0..((FXMAC_LAST_OFFSET - FXMAC_OCTTXL_OFFSET) / 4) {
        read_reg((instance_p.config.base_address + FXMAC_OCTTXL_OFFSET + (i * 4)) as *mut u32);
    }

    // Sync default options with hardware but leave receiver and
    // transmitter disabled. They get enabled with FXmacStart() if
    // FXMAC_TRANSMITTER_ENABLE_OPTION and FXMAC_RECEIVER_ENABLE_OPTION are set.
    let options = instance_p.config.network_default_config
        & !(FXMAC_TRANSMITTER_ENABLE_OPTION | FXMAC_RECEIVER_ENABLE_OPTION);
    FXmacSetOptions(instance_p, options, 0);
    let options = !instance_p.config.network_default_config;
    FXmacClearOptions(instance_p, options, 0);
}

fn FXmacDmaReset(instance_p: &mut FXmac) {
    let max_frame_size: u32 = instance_p.max_frame_size;

    let mut dmacfg: u32 = 0;
    // let max_queue_num = 16;
    // let dma_brust_length = 16;

    let mut rx_buf_size: u32 = max_frame_size / FXMAC_RX_BUF_UNIT;
    rx_buf_size += if !max_frame_size.is_multiple_of(FXMAC_RX_BUF_UNIT) {
        1
    } else {
        0
    }; /* roundup */

    // moudle_id=12
    if (instance_p.moudle_id >= 2) {
        for queue in 0..instance_p.config.max_queue_num {
            dmacfg = 0;

            // 设置发包/收包 buffer队列的基地址
            FXmacSetQueuePtr(instance_p, 0, queue as u8, FXMAC_SEND);
            FXmacSetQueuePtr(instance_p, 0, queue as u8, FXMAC_RECV);

            if queue != 0 {
                write_reg(
                    (instance_p.config.base_address + FXMAC_RXBUFQX_SIZE_OFFSET(queue as u64))
                        as *mut u32,
                    rx_buf_size,
                );
            } else
            // queue is 0
            {
                dmacfg |= (FXMAC_DMACR_RXBUF_MASK & (rx_buf_size << FXMAC_DMACR_RXBUF_SHIFT));
            }
        }

        dmacfg |= (instance_p.config.dma_brust_length & FXMAC_DMACR_BLENGTH_MASK);

        dmacfg &= !FXMAC_DMACR_ENDIAN_MASK;
        dmacfg &= !FXMAC_DMACR_SWAP_MANAGEMENT_MASK; /* 选择小端 */

        dmacfg &= !FXMAC_DMACR_TCPCKSUM_MASK; /* close  transmitter checksum generation engine */

        dmacfg &= !FXMAC_DMACR_ADDR_WIDTH_64;
        dmacfg |= FXMAC_DMACR_RXSIZE_MASK | FXMAC_DMACR_TXSIZE_MASK;
        // set this bit can enable auto discard rx frame when lack of receive source,
        // which avoid endless rx buffer not available error intrrupts.
        dmacfg |= FXMAC_DMACR_ORCE_DISCARD_ON_ERR_MASK; /* force_discard_on_rx_err */

        dmacfg |= FXMAC_DMACR_ADDR_WIDTH_64; // Just for aarch64
    } else {
        FXmacSetQueuePtr(instance_p, 0, 0, FXMAC_SEND);
        FXmacSetQueuePtr(instance_p, 0, 0, FXMAC_RECV);
        dmacfg |= (FXMAC_DMACR_RXBUF_MASK & (rx_buf_size << FXMAC_DMACR_RXBUF_SHIFT));
        dmacfg |= (instance_p.config.dma_brust_length & FXMAC_DMACR_BLENGTH_MASK);

        dmacfg &= !FXMAC_DMACR_ENDIAN_MASK;
        dmacfg &= !FXMAC_DMACR_SWAP_MANAGEMENT_MASK; /* 选择小端 */

        dmacfg &= !FXMAC_DMACR_TCPCKSUM_MASK; /* close  transmitter checksum generation engine */

        dmacfg &= !FXMAC_DMACR_ADDR_WIDTH_64;
        dmacfg |= FXMAC_DMACR_RXSIZE_MASK | FXMAC_DMACR_TXSIZE_MASK;
        // set this bit can enable auto discard rx frame when lack of receive source,
        // which avoid endless rx buffer not available error intrrupts.
        dmacfg |= FXMAC_DMACR_ORCE_DISCARD_ON_ERR_MASK; /* force_discard_on_rx_err */
        dmacfg |= FXMAC_DMACR_ADDR_WIDTH_64; // Just for aarch64
    }

    write_reg(
        (instance_p.config.base_address + FXMAC_DMACR_OFFSET) as *mut u32,
        dmacfg,
    );
}

fn FXmacDmaWidth(instance_p: &FXmac) -> u32 {
    if instance_p.moudle_id < 2 {
        return FXMAC_NWCFG_BUS_WIDTH_32_MASK;
    }

    let read_regs =
        read_reg((instance_p.config.base_address + FXMAC_DESIGNCFG_DEBUG1_OFFSET) as *const u32);
    match ((read_regs & FXMAC_DESIGNCFG_DEBUG1_BUS_WIDTH_MASK) >> 25) {
        4 => {
            info!("bus width is 128");
            FXMAC_NWCFG_BUS_WIDTH_128_MASK
        }
        2 => {
            info!("bus width is 64");
            FXMAC_NWCFG_BUS_WIDTH_64_MASK
        }
        _ => {
            info!("bus width is 32");
            FXMAC_NWCFG_BUS_WIDTH_32_MASK
        }
    }
}

fn FxmacFeatureSetOptions(feature: u32, xmac_p: &mut FXmac) {
    let mut options: u32 = 0;

    if (feature & FXMAC_LWIP_PORT_CONFIG_JUMBO) != 0 {
        info!("FXMAC_JUMBO_ENABLE_OPTION is ok");
        options |= FXMAC_JUMBO_ENABLE_OPTION;
    }

    if (feature & FXMAC_LWIP_PORT_CONFIG_UNICAST_ADDRESS_FILITER) != 0 {
        info!("FXMAC_UNICAST_OPTION is ok");
        options |= FXMAC_UNICAST_OPTION;
    }

    if (feature & FXMAC_LWIP_PORT_CONFIG_MULTICAST_ADDRESS_FILITER) != 0 {
        info!("FXMAC_MULTICAST_OPTION is ok");
        options |= FXMAC_MULTICAST_OPTION;
    }
    // enable copy all frames
    if (feature & FXMAC_LWIP_PORT_CONFIG_COPY_ALL_FRAMES) != 0 {
        info!("FXMAC_PROMISC_OPTION is ok");
        options |= FXMAC_PROMISC_OPTION;
    }
    // close fcs check
    if (feature & FXMAC_LWIP_PORT_CONFIG_CLOSE_FCS_CHECK) != 0 {
        info!("FXMAC_FCS_STRIP_OPTION is ok");
        options |= FXMAC_FCS_STRIP_OPTION;
    }

    FXmacSetOptions(xmac_p, options, 0);
}

/// Sets the start address of the transmit/receive buffer queue.
///
/// # Arguments
///
/// * `queue_p` - Physical base address of the queue ring.
/// * `queue_num` - Queue index to configure.
/// * `direction` - [`FXMAC_SEND`] or [`FXMAC_RECV`].
///
/// # Note
///
/// The buffer queue address must be configured before calling [`FXmacStart`].
pub fn FXmacSetQueuePtr(instance_p: &FXmac, queue_p: u64, queue_num: u8, direction: u32) {
    // assert!(instance_p.is_ready == FT_COMPONENT_IS_READY);
    // If already started, then just return

    let flag_queue_p = if queue_p == 0 { 1 } else { 0 };
    let FXMAC_QUEUE_REGISTER_OFFSET =
        |base_addr: u64, queue_id: u64| (base_addr + (queue_id - 1) * 4);

    if queue_num == 0 {
        if direction == FXMAC_SEND {
            // set base start address of TX buffer queue (tx buffer descriptor list)
            write_reg(
                (instance_p.config.base_address + FXMAC_TXQBASE_OFFSET) as *mut u32,
                ((queue_p & ULONG64_LO_MASK) | flag_queue_p) as u32,
            );
        } else {
            // set base start address of RX buffer queue (rx buffer descriptor list)
            write_reg(
                (instance_p.config.base_address + FXMAC_RXQBASE_OFFSET) as *mut u32,
                ((queue_p & ULONG64_LO_MASK) | flag_queue_p) as u32,
            );
        }
    } else if direction == FXMAC_SEND {
        write_reg(
            (instance_p.config.base_address
                + FXMAC_QUEUE_REGISTER_OFFSET(FXMAC_TXQ1BASE_OFFSET, queue_num as u64))
                as *mut u32,
            ((queue_p & ULONG64_LO_MASK) | flag_queue_p) as u32,
        );
    } else {
        write_reg(
            (instance_p.config.base_address
                + FXMAC_QUEUE_REGISTER_OFFSET(FXMAC_RXQ1BASE_OFFSET, queue_num as u64))
                as *mut u32,
            ((queue_p & ULONG64_LO_MASK) | flag_queue_p) as u32,
        );
    }

    if direction == FXMAC_SEND
    // Only for aarch64
    {
        // Set the MSB of TX Queue start address
        write_reg(
            (instance_p.config.base_address + FXMAC_MSBBUF_TXQBASE_OFFSET) as *mut u32,
            ((queue_p & ULONG64_HI_MASK) >> 32) as u32,
        );
    } else {
        // Set the MSB of RX Queue start address
        write_reg(
            (instance_p.config.base_address + FXMAC_MSBBUF_RXQBASE_OFFSET) as *mut u32,
            ((queue_p & ULONG64_HI_MASK) >> 32) as u32,
        );
    }
}

fn FXmacConfigureCaps(instance_p: &mut FXmac) {
    instance_p.caps = 0;
    let read_regs =
        read_reg((instance_p.config.base_address + FXMAC_DESIGNCFG_DEBUG1_OFFSET) as *const u32);
    if (read_regs & FXMAC_DESIGNCFG_DEBUG1_BUS_IRQCOR_MASK) == 0 {
        instance_p.caps |= FXMAC_CAPS_ISR_CLEAR_ON_WRITE;
        info!(
            "Design ConfigReg1: {:#x} Has FXMAC_CAPS_ISR_CLEAR_ON_WRITE feature",
            read_regs
        );
    }
}

fn FXmacClkDivGet(instance_p: &mut FXmac) -> u32 {
    // moudle_id=12
    // let pclk_hz = 50000000;
    let pclk_hz = instance_p.config.pclk_hz; // FXMAC0_PCLK;

    if (pclk_hz <= 20000000) {
        FXMAC_NWCFG_CLOCK_DIV8_MASK
    } else if (pclk_hz <= 40000000) {
        FXMAC_NWCFG_CLOCK_DIV16_MASK
    } else if (pclk_hz <= 80000000) {
        FXMAC_NWCFG_CLOCK_DIV32_MASK
    } else if (instance_p.moudle_id >= 2) {
        if (pclk_hz <= 120000000) {
            FXMAC_NWCFG_CLOCK_DIV48_MASK
        } else if (pclk_hz <= 160000000) {
            FXMAC_NWCFG_CLOCK_DIV64_MASK
        } else if (pclk_hz <= 240000000) {
            FXMAC_NWCFG_CLOCK_DIV96_MASK
        } else if (pclk_hz <= 320000000) {
            FXMAC_NWCFG_CLOCK_DIV128_MASK
        } else {
            FXMAC_NWCFG_CLOCK_DIV224_MASK
        }
    } else {
        FXMAC_NWCFG_CLOCK_DIV64_MASK
    }
}

/// Set options for the driver/device. The driver should be stopped with
/// FXmacStop() before changing options.
fn FXmacSetOptions(instance_p: &mut FXmac, options: u32, queue_num: u32) -> u32 {
    let mut reg: u32 = 0; /* Generic register contents */
    let mut reg_netcfg: u32 = 0; /* Reflects original contents of NET_CONFIG */
    let mut reg_new_netcfg: u32 = 0; /* Reflects new contents of NET_CONFIG */
    let mut status: u32 = 0;

    // let is_started = 0;

    info!(
        "FXmacSetOptions, is_started={}, options={}, queue_num={}, max_queue_num={}",
        instance_p.is_started, options, queue_num, instance_p.config.max_queue_num
    );

    // Be sure device has been stopped
    if instance_p.is_started == FT_COMPONENT_IS_STARTED {
        status = 9; //FXMAC_ERR_MAC_IS_PROCESSING;
        error!("FXMAC is processing when calling FXmacSetOptions function");
    } else {
        // Many of these options will change the NET_CONFIG registers.
        // To reduce the amount of IO to the device, group these options here
        // and change them all at once.

        // Grab current register contents
        reg_netcfg = read_reg((instance_p.config.base_address + FXMAC_NWCFG_OFFSET) as *const u32);

        reg_new_netcfg = reg_netcfg;

        // It is configured to max 1536.
        if (options & FXMAC_FRAME1536_OPTION) != 0 {
            reg_new_netcfg |= FXMAC_NWCFG_1536RXEN_MASK;
        }

        // Turn on VLAN packet only, only VLAN tagged will be accepted
        if (options & FXMAC_VLAN_OPTION) != 0 {
            reg_new_netcfg |= FXMAC_NWCFG_NVLANDISC_MASK;
        }

        // Turn on FCS stripping on receive packets
        if (options & FXMAC_FCS_STRIP_OPTION) != 0 {
            reg_new_netcfg |= FXMAC_NWCFG_FCS_REMOVE_MASK;
        }

        // Turn on length/type field checking on receive packets
        if (options & FXMAC_LENTYPE_ERR_OPTION) != 0 {
            reg_new_netcfg |= FXMAC_NWCFG_LENGTH_FIELD_ERROR_FRAME_DISCARD_MASK;
        }

        // Turn on flow control
        if (options & FXMAC_FLOW_CONTROL_OPTION) != 0 {
            reg_new_netcfg |= FXMAC_NWCFG_PAUSE_ENABLE_MASK;
        }

        // Turn on promiscuous frame filtering (all frames are received)
        if (options & FXMAC_PROMISC_OPTION) != 0 {
            reg_new_netcfg |= FXMAC_NWCFG_COPYALLEN_MASK;
        }

        // Allow broadcast address reception
        if (options & FXMAC_BROADCAST_OPTION) != 0 {
            reg_new_netcfg &= !FXMAC_NWCFG_BCASTDI_MASK;
        }

        // Allow multicast address filtering
        if (options & FXMAC_MULTICAST_OPTION) != 0 {
            reg_new_netcfg |= FXMAC_NWCFG_MCASTHASHEN_MASK;
        }

        if (options & FXMAC_UNICAST_OPTION) != 0 {
            reg_new_netcfg |= FXMAC_NWCFG_UCASTHASHEN_MASK;
        }

        if (options & FXMAC_TAIL_PTR_OPTION) != 0 {
            write_reg(
                (instance_p.config.base_address + FXMAC_TAIL_ENABLE) as *mut u32,
                0x80000001,
            );
        }

        // enable RX checksum offload
        if (options & FXMAC_RX_CHKSUM_ENABLE_OPTION) != 0 {
            reg_new_netcfg |= FXMAC_NWCFG_RXCHKSUMEN_MASK;
        }

        // Enable jumbo frames
        if (options & FXMAC_JUMBO_ENABLE_OPTION) != 0 {
            instance_p.max_mtu_size = FXMAC_MTU_JUMBO;
            instance_p.max_frame_size = FXMAC_MAX_FRAME_SIZE_JUMBO;

            reg_new_netcfg |= FXMAC_NWCFG_JUMBO_MASK;

            write_reg(
                (instance_p.config.base_address + FXMAC_JUMBOMAXLEN_OFFSET) as *mut u32,
                FXMAC_MAX_FRAME_SIZE_JUMBO,
            );

            write_reg(
                (instance_p.config.base_address + FXMAC_TXQSEGALLOC_QLOWER_OFFSET) as *mut u32,
                FXMAC_TXQSEGALLOC_QLOWER_JUMBO_MASK,
            );

            if queue_num == 0 {
                let mut rx_buf_size: u32 = 0;
                reg = read_reg((instance_p.config.base_address + FXMAC_DMACR_OFFSET) as *const u32);

                reg &= !FXMAC_DMACR_RXBUF_MASK;

                rx_buf_size = instance_p.max_frame_size / FXMAC_RX_BUF_UNIT;
                rx_buf_size += if !instance_p.max_frame_size.is_multiple_of(FXMAC_RX_BUF_UNIT) {
                    1
                } else {
                    0
                };

                reg |= (rx_buf_size << FXMAC_DMACR_RXBUF_SHIFT) & FXMAC_DMACR_RXBUF_MASK;
                write_reg(
                    (instance_p.config.base_address + FXMAC_DMACR_OFFSET) as *mut u32,
                    reg,
                );
            } else if queue_num < instance_p.config.max_queue_num {
                let mut rx_buf_size: u32 = 0;
                rx_buf_size = instance_p.max_frame_size / FXMAC_RX_BUF_UNIT;
                rx_buf_size += if !instance_p.max_frame_size.is_multiple_of(FXMAC_RX_BUF_UNIT) {
                    1
                } else {
                    0
                };

                write_reg(
                    (instance_p.config.base_address + FXMAC_RXBUFQX_SIZE_OFFSET(queue_num as u64))
                        as *mut u32,
                    rx_buf_size & FXMAC_RXBUFQX_SIZE_MASK,
                );
            }
        }

        if (options & FXMAC_SGMII_ENABLE_OPTION) != 0 {
            reg_new_netcfg |= (FXMAC_NWCFG_SGMII_MODE_ENABLE_MASK | FXMAC_NWCFG_PCSSEL_MASK);
        }

        if (options & FXMAC_LOOPBACK_NO_MII_OPTION) != 0 {
            reg = read_reg((instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *const u32);
            reg |= FXMAC_NWCTRL_LOOPBACK_LOCAL_MASK;
            write_reg(
                (instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *mut u32,
                reg,
            );
        }

        if (options & FXMAC_LOOPBACK_USXGMII_OPTION) != 0 {
            write_reg(
                (instance_p.config.base_address + FXMAC_TEST_CONTROL_OFFSET) as *mut u32,
                2,
            );
        }

        // Officially change the NET_CONFIG registers if it needs to be
        // modified.
        if (reg_netcfg != reg_new_netcfg) {
            write_reg(
                (instance_p.config.base_address + FXMAC_NWCFG_OFFSET) as *mut u32,
                reg_new_netcfg,
            );
        }

        // Enable TX checksum offload
        if (options & FXMAC_TX_CHKSUM_ENABLE_OPTION) != 0 {
            reg = read_reg((instance_p.config.base_address + FXMAC_DMACR_OFFSET) as *const u32);
            reg |= FXMAC_DMACR_TCPCKSUM_MASK;
            write_reg(
                (instance_p.config.base_address + FXMAC_DMACR_OFFSET) as *mut u32,
                reg,
            );
        }

        // Enable transmitter
        if (options & FXMAC_TRANSMITTER_ENABLE_OPTION) != 0 {
            reg = read_reg((instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *const u32);
            reg |= FXMAC_NWCTRL_TXEN_MASK;
            write_reg(
                (instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *mut u32,
                reg,
            );
        }

        // Enable receiver
        if (options & FXMAC_RECEIVER_ENABLE_OPTION) != 0 {
            reg = read_reg((instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *const u32);
            reg |= FXMAC_NWCTRL_RXEN_MASK;

            write_reg(
                (instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *mut u32,
                reg,
            );
        }

        // The remaining options not handled here are managed elsewhere in the
        // driver. No register modifications are needed at this time. Reflecting
        // the option in instance_p->options is good enough for now.

        // Set options word to its new value
        instance_p.options |= options;

        status = 0; // FT_SUCCESS;
    }

    status
}

/// Clear options for the driver/device
fn FXmacClearOptions(instance_p: &mut FXmac, options: u32, queue_num: u32) -> u32 {
    let mut reg: u32 = 0; /* Generic */
    let mut reg_net_cfg: u32 = 0; /* Reflects original contents of NET_CONFIG */
    let mut reg_new_net_cfg: u32 = 0; /* Reflects new contents of NET_CONFIG */
    let mut status: u32 = 0;

    // let is_started = 0;
    // Be sure device has been stopped
    if (instance_p.is_started == FT_COMPONENT_IS_STARTED) {
        status = 9; //FXMAC_ERR_MAC_IS_PROCESSING
        error!("FXMAC is processing when calling FXmacClearOptions function");
    } else {
        // Many of these options will change the NET_CONFIG registers.
        // To reduce the amount of IO to the device, group these options here
        // and change them all at once.
        // Grab current register contents
        reg_net_cfg = read_reg((instance_p.config.base_address + FXMAC_NWCFG_OFFSET) as *const u32);
        reg_new_net_cfg = reg_net_cfg;
        // There is only RX configuration!?
        // It is configured in two different length, up to 1536 and 10240 bytes
        if (options & FXMAC_FRAME1536_OPTION) != 0 {
            reg_new_net_cfg &= !FXMAC_NWCFG_1536RXEN_MASK;
        }

        // Turn off VLAN packet only
        if (options & FXMAC_VLAN_OPTION) != 0 {
            reg_new_net_cfg &= !FXMAC_NWCFG_NVLANDISC_MASK;
        }

        // Turn off FCS stripping on receive packets
        if (options & FXMAC_FCS_STRIP_OPTION) != 0 {
            reg_new_net_cfg &= !FXMAC_NWCFG_FCS_REMOVE_MASK;
        }

        // Turn off length/type field checking on receive packets
        if (options & FXMAC_LENTYPE_ERR_OPTION) != 0 {
            reg_new_net_cfg &= !FXMAC_NWCFG_LENGTH_FIELD_ERROR_FRAME_DISCARD_MASK;
        }

        // Turn off flow control
        if (options & FXMAC_FLOW_CONTROL_OPTION) != 0 {
            reg_new_net_cfg &= !FXMAC_NWCFG_PAUSE_ENABLE_MASK;
        }

        // Turn off promiscuous frame filtering (all frames are received)
        if (options & FXMAC_PROMISC_OPTION) != 0 {
            reg_new_net_cfg &= !FXMAC_NWCFG_COPYALLEN_MASK;
        }

        // Disallow broadcast address filtering => broadcast reception
        if (options & FXMAC_BROADCAST_OPTION) != 0 {
            reg_new_net_cfg |= FXMAC_NWCFG_BCASTDI_MASK;
        }

        // Disallow unicast address filtering
        if (options & FXMAC_UNICAST_OPTION) != 0 {
            reg_new_net_cfg &= !FXMAC_NWCFG_UCASTHASHEN_MASK;
        }

        // Disallow multicast address filtering
        if (options & FXMAC_MULTICAST_OPTION) != 0 {
            reg_new_net_cfg &= !FXMAC_NWCFG_MCASTHASHEN_MASK;
        }

        if (options & FXMAC_TAIL_PTR_OPTION) != 0 {
            write_reg(
                (instance_p.config.base_address + FXMAC_TAIL_ENABLE) as *mut u32,
                0,
            );
        }

        // Disable RX checksum offload
        if (options & FXMAC_RX_CHKSUM_ENABLE_OPTION) != 0 {
            reg_new_net_cfg &= !FXMAC_NWCFG_RXCHKSUMEN_MASK;
        }

        // Disable jumbo frames
        if (options & FXMAC_JUMBO_ENABLE_OPTION) != 0
        // 恢复之前buffer 容量
        {
            instance_p.max_mtu_size = FXMAC_MTU;
            instance_p.max_frame_size = FXMAC_MAX_FRAME_SIZE;

            reg_new_net_cfg &= !FXMAC_NWCFG_JUMBO_MASK;

            reg = read_reg((instance_p.config.base_address + FXMAC_DMACR_OFFSET) as *const u32);

            reg &= !FXMAC_DMACR_RXBUF_MASK;

            if queue_num == 0 {
                let mut rx_buf_size: u32 = 0;

                reg = read_reg((instance_p.config.base_address + FXMAC_DMACR_OFFSET) as *const u32);
                reg &= !FXMAC_DMACR_RXBUF_MASK;

                rx_buf_size = instance_p.max_frame_size / FXMAC_RX_BUF_UNIT;
                rx_buf_size += if !instance_p.max_frame_size.is_multiple_of(FXMAC_RX_BUF_UNIT) {
                    1
                } else {
                    0
                };

                reg |= (rx_buf_size << FXMAC_DMACR_RXBUF_SHIFT) & FXMAC_DMACR_RXBUF_MASK;

                write_reg(
                    (instance_p.config.base_address + FXMAC_DMACR_OFFSET) as *mut u32,
                    reg,
                );
            } else if (queue_num < instance_p.config.max_queue_num) {
                let mut rx_buf_size: u32 = 0;
                rx_buf_size = instance_p.max_frame_size / FXMAC_RX_BUF_UNIT;
                rx_buf_size += if !instance_p.max_frame_size.is_multiple_of(FXMAC_RX_BUF_UNIT) {
                    1
                } else {
                    0
                };

                write_reg(
                    (instance_p.config.base_address + FXMAC_RXBUFQX_SIZE_OFFSET(queue_num as u64))
                        as *mut u32,
                    rx_buf_size & FXMAC_RXBUFQX_SIZE_MASK,
                );
            }
        }

        if (options & FXMAC_SGMII_ENABLE_OPTION) != 0 {
            reg_new_net_cfg &= !(FXMAC_NWCFG_SGMII_MODE_ENABLE_MASK | FXMAC_NWCFG_PCSSEL_MASK);
        }

        if (options & FXMAC_LOOPBACK_NO_MII_OPTION) != 0 {
            reg = read_reg((instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *const u32);
            reg &= !FXMAC_NWCTRL_LOOPBACK_LOCAL_MASK;
            write_reg(
                (instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *mut u32,
                reg,
            );
        }

        if (options & FXMAC_LOOPBACK_USXGMII_OPTION) != 0 {
            write_reg(
                (instance_p.config.base_address + FXMAC_TEST_CONTROL_OFFSET) as *mut u32,
                read_reg(
                    (instance_p.config.base_address + FXMAC_TEST_CONTROL_OFFSET) as *const u32,
                ) & !2,
            );
        }

        // Officially change the NET_CONFIG registers if it needs to be
        // modified.
        if reg_net_cfg != reg_new_net_cfg {
            write_reg(
                (instance_p.config.base_address + FXMAC_NWCFG_OFFSET) as *mut u32,
                reg_new_net_cfg,
            );
        }

        // Disable TX checksum offload
        if (options & FXMAC_TX_CHKSUM_ENABLE_OPTION) != 0 {
            reg = read_reg((instance_p.config.base_address + FXMAC_DMACR_OFFSET) as *const u32);
            reg &= !FXMAC_DMACR_TCPCKSUM_MASK;
            write_reg(
                (instance_p.config.base_address + FXMAC_DMACR_OFFSET) as *mut u32,
                reg,
            );
        }

        // Disable transmitter
        if (options & FXMAC_TRANSMITTER_ENABLE_OPTION) != 0 {
            reg = read_reg((instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *const u32);
            reg &= !FXMAC_NWCTRL_TXEN_MASK;
            write_reg(
                (instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *mut u32,
                reg,
            );
        }

        // Disable receiver
        if (options & FXMAC_RECEIVER_ENABLE_OPTION) != 0 {
            reg = read_reg((instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *const u32);
            reg &= !FXMAC_NWCTRL_RXEN_MASK;
            write_reg(
                (instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *mut u32,
                reg,
            );
        }

        // The remaining options not handled here are managed elsewhere in the
        // driver. No register modifications are needed at this time. Reflecting
        // option in instance_p->options is good enough for now.

        // Set options word to its new value
        instance_p.options &= !options;

        status = 0; // FT_SUCCESS
    }
    status
}

///  Clear the Hash registers for the mac address pointed by address_ptr.
fn FXmacClearHash(instance_p: &FXmac) {
    write_reg(
        (instance_p.config.base_address + FXMAC_HASHL_OFFSET) as *mut u32,
        0,
    );

    // write bits [63:32] in TOP
    write_reg(
        (instance_p.config.base_address + FXMAC_HASHH_OFFSET) as *mut u32,
        0,
    );
}

/// Sets the MAC address for the specified address slot.
///
/// The device must be stopped before calling this function.
///
/// # Arguments
///
/// * `address_ptr` - 6-byte MAC address.
/// * `index` - Address slot index (0..FXMAC_MAX_MAC_ADDR).
///
/// # Panics
///
/// Panics if `index` is out of range.
pub fn FXmacSetMacAddress(instance_p: &FXmac, address_ptr: &[u8; 6], index: u8) -> u32 {
    let mut mac_addr: u32 = 0;
    let aptr = address_ptr;
    let index_loc: u8 = index;
    let mut status: u32 = 0;
    assert!(
        (index_loc < FXMAC_MAX_MAC_ADDR as u8),
        "index of Mac Address exceed {}",
        FXMAC_MAX_MAC_ADDR
    );

    let is_started = 0;
    // Be sure device has been stopped
    if is_started == FT_COMPONENT_IS_STARTED {
        // status = FXMAC_ERR_MAC_IS_PROCESSING;
        status = 9;
        error!("FXMAC is processing when calling FXmacSetMacAddress function");
    } else {
        // Set the MAC bits [31:0] in BOT
        mac_addr = aptr[0] as u32
            | ((aptr[1] as u32) << 8)
            | ((aptr[2] as u32) << 16)
            | ((aptr[3] as u32) << 24);
        write_reg(
            (instance_p.config.base_address + FXMAC_GEM_SA1B as u64 + (index_loc * 8) as u64)
                as *mut u32,
            mac_addr,
        );

        // There are reserved bits in TOP so don't affect them
        mac_addr = read_reg(
            (instance_p.config.base_address + FXMAC_GEM_SA1T as u64 + (index_loc * 8) as u64)
                as *const u32,
        );
        mac_addr &= !FXMAC_GEM_SAB_MASK;

        // Set MAC bits [47:32] in TOP
        mac_addr |= aptr[4] as u32;
        mac_addr |= (aptr[5] as u32) << 8;

        write_reg(
            (instance_p.config.base_address + FXMAC_GEM_SA1T as u64 + (index_loc * 8) as u64)
                as *mut u32,
            mac_addr,
        );

        status = 0; // FT_SUCCESS
    }

    status
}
/// Reads a MAC address from the specified address slot.
///
/// # Arguments
///
/// * `address_ptr` - Output buffer for the MAC address.
/// * `index` - Address slot index (0..FXMAC_MAX_MAC_ADDR).
///
/// # Panics
///
/// Panics if `index` is out of range.
pub fn FXmacGetMacAddress(instance_p: &FXmac, address_ptr: &mut [u8; 6], index: u8) {
    assert!((index as u32) < FXMAC_MAX_MAC_ADDR);

    let mut reg_value: u32 = read_reg(
        (instance_p.config.base_address + FXMAC_GEM_SA1B as u64 + (index as u64 * 8)) as *const u32,
    );
    address_ptr[0] = reg_value as u8;
    address_ptr[1] = (reg_value >> 8) as u8;
    address_ptr[2] = (reg_value >> 16) as u8;
    address_ptr[3] = (reg_value >> 24) as u8;

    reg_value = read_reg(
        (instance_p.config.base_address + FXMAC_GEM_SA1T as u64 + (index as u64 * 8)) as *const u32,
    );
    address_ptr[4] = (reg_value) as u8;
    address_ptr[5] = (reg_value >> 8) as u8;
}

/// Sets a 48-bit MAC address entry in the hash table.
///
/// The device must be stopped before calling this function.
///
/// # Arguments
///
/// * `intance_p` - Mutable reference to the FXMAC instance.
/// * `mac_address` - The MAC address to hash.
///
/// The hash address register is 64 bits long and takes up two locations in
/// the memory map. The least significant bits are stored in hash register
/// bottom and the most significant bits in hash register top.
///
/// The unicast hash enable and the multicast hash enable bits in the network
/// configuration register enable the reception of hash matched frames. The
/// destination address is reduced to a 6 bit index into the 64 bit hash
/// register using the following hash function. The hash function is an XOR
/// of every sixth bit of the destination address.
pub fn FXmac_SetHash(intance_p: &mut FXmac, mac_address: &[u8; 6]) -> u32 {
    let mut HashAddr: u32 = 0;
    let mut Status: u32 = 0;
    debug!("Set MAC: {:x?} in hash table", mac_address);

    // Check that the Ethernet address (MAC) is not 00:00:00:00:00:00
    assert!(!((mac_address[0] == 0) && (mac_address[5] == 0)));
    assert!(intance_p.is_ready == FT_COMPONENT_IS_READY);

    // Be sure device has been stopped
    if (intance_p.is_started == FT_COMPONENT_IS_STARTED) {
        error!("FXmac_SetHash failed: FXMAC_ERR_MAC_IS_PROCESSING");
        Status = 9; // FXMAC_ERR_MAC_IS_PROCESSING
    } else {
        let Temp1: u8 = (mac_address[0]) & 0x3F;
        let Temp2: u8 = ((mac_address[0] >> 6) & 0x03) | ((mac_address[1] & 0x0F) << 2);
        let Temp3: u8 = ((mac_address[1] >> 4) & 0x0F) | ((mac_address[2] & 0x3) << 4);
        let Temp4: u8 = ((mac_address[2] >> 2) & 0x3F);
        let Temp5: u8 = mac_address[3] & 0x3F;
        let Temp6: u8 = ((mac_address[3] >> 6) & 0x03) | ((mac_address[4] & 0x0F) << 2);
        let Temp7: u8 = ((mac_address[4] >> 4) & 0x0F) | ((mac_address[5] & 0x03) << 4);
        let Temp8: u8 = ((mac_address[5] >> 2) & 0x3F);

        let Result: u32 = (Temp1 as u32)
            ^ (Temp2 as u32)
            ^ (Temp3 as u32)
            ^ (Temp4 as u32)
            ^ (Temp5 as u32)
            ^ (Temp6 as u32)
            ^ (Temp7 as u32)
            ^ (Temp8 as u32);

        if (Result >= FXMAC_MAX_HASH_BITS) {
            Status = 1; // FXMAC_ERR_INVALID_PARAM
        } else {
            if (Result < 32) {
                HashAddr =
                    read_reg((intance_p.config.base_address + FXMAC_HASHL_OFFSET) as *const u32);
                HashAddr |= 1 << Result;
                write_reg(
                    (intance_p.config.base_address + FXMAC_HASHL_OFFSET) as *mut u32,
                    HashAddr,
                );
            } else {
                HashAddr =
                    read_reg((intance_p.config.base_address + FXMAC_HASHH_OFFSET) as *const u32);
                HashAddr |= 1 << (Result - 32);
                write_reg(
                    (intance_p.config.base_address + FXMAC_HASHH_OFFSET) as *mut u32,
                    HashAddr,
                );
            }
            Status = 0;
        }
    }

    Status
}

/// Delete 48-bit MAC addresses in hash table.
/// The device must be stopped before calling this function.
pub fn FXmac_DeleteHash(intance_p: &mut FXmac, mac_address: &[u8; 6]) -> u32 {
    let mut HashAddr: u32 = 0;
    let mut Status: u32 = 0;

    assert!(intance_p.is_ready == FT_COMPONENT_IS_READY);

    // Be sure device has been stopped
    if (intance_p.is_started == FT_COMPONENT_IS_STARTED) {
        Status = 9; // (FXMAC_ERR_MAC_IS_PROCESSING);
    } else {
        let mut Temp1: u8 = (mac_address[0]) & 0x3F;
        let mut Temp2: u8 = ((mac_address[0] >> 6) & 0x03) | ((mac_address[1] & 0x0F) << 2);
        let mut Temp3: u8 = ((mac_address[1] >> 4) & 0x0F) | ((mac_address[2] & 0x03) << 4);
        let mut Temp4: u8 = ((mac_address[2] >> 2) & 0x3F);
        let mut Temp5: u8 = (mac_address[3]) & 0x3F;
        let mut Temp6: u8 = ((mac_address[3] >> 6) & 0x03) | ((mac_address[4] & 0x0F) << 2);
        let mut Temp7: u8 = ((mac_address[4] >> 4) & 0x0F) | ((mac_address[5] & 0x03) << 4);
        let mut Temp8: u8 = ((mac_address[5] >> 2) & 0x3F);

        let Result: u32 = (Temp1 as u32)
            ^ (Temp2 as u32)
            ^ (Temp3 as u32)
            ^ (Temp4 as u32)
            ^ (Temp5 as u32)
            ^ (Temp6 as u32)
            ^ (Temp7 as u32)
            ^ (Temp8 as u32);

        if Result >= FXMAC_MAX_HASH_BITS {
            Status = 1; //(FXMAC_ERR_INVALID_PARAM);
        } else {
            if Result < 32 {
                HashAddr =
                    read_reg((intance_p.config.base_address + FXMAC_HASHL_OFFSET) as *const u32);
                HashAddr &= !((1 << Result) as u32);
                write_reg(
                    (intance_p.config.base_address + FXMAC_HASHL_OFFSET) as *mut u32,
                    HashAddr,
                );
            } else {
                HashAddr =
                    read_reg((intance_p.config.base_address + FXMAC_HASHH_OFFSET) as *const u32);
                HashAddr &= !((1 << (Result - 32)) as u32);
                write_reg(
                    (intance_p.config.base_address + FXMAC_HASHH_OFFSET) as *mut u32,
                    HashAddr,
                );
            }
            Status = 0;
        }
    }
    Status
}

/// Set the Type ID match for this driver/device.  The register is a 32-bit value.
/// The device must be stopped before calling this function.
fn FXmacSetTypeIdCheck(instance_p: &FXmac, id_check: u32, index: u8) -> u32 {
    let mut status: u32 = 0;
    assert!(
        (index < FXMAC_MAX_TYPE_ID as u8),
        "index of Type ID exceed {}",
        FXMAC_MAX_TYPE_ID
    );

    let is_started = 0;
    // Be sure device has been stopped
    if is_started == FT_COMPONENT_IS_STARTED {
        status = 9; //FXMAC_ERR_MAC_IS_PROCESSING
        error!("FXMAC is processing when calling FXmacSetTypeIdCheck function");
    } else {
        // Set the ID bits in MATCHx register
        write_reg(
            (instance_p.config.base_address + FXMAC_MATCH1_OFFSET + (index * 4) as u64) as *mut u32,
            id_check,
        );

        status = FT_SUCCESS;
    }

    status
}

/// FXmacSelectClk
/// Determine the driver clock configuration based on the media independent interface
/// FXMAC_CLK_TYPE_0
fn FXmacSelectClk(instance_p: &mut FXmac) {
    let speed: u32 = instance_p.config.speed;
    let FXMAC_WRITEREG32 = |base_address: u64, offset: u32, reg_value: u32| {
        write_reg((base_address + offset as u64) as *mut u32, reg_value)
    };

    assert!(
        (speed == FXMAC_SPEED_10)
            || (speed == FXMAC_SPEED_100)
            || (speed == FXMAC_SPEED_1000)
            || (speed == FXMAC_SPEED_2500)
            || (speed == FXMAC_SPEED_10000)
    );

    if (instance_p.config.interface == FXmacPhyInterface::FXMAC_PHY_INTERFACE_MODE_USXGMII)
        || (instance_p.config.interface == FXmacPhyInterface::FXMAC_PHY_INTERFACE_MODE_XGMII)
    {
        if speed == FXMAC_SPEED_10000 {
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_SRC_SEL_LN, 0x1); /*0x1c04*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_DIV_SEL0_LN, 0x4); /*0x1c08*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_DIV_SEL1_LN, 0x1); /*0x1c0c*/
            FXMAC_WRITEREG32(
                instance_p.config.base_address,
                FXMAC_GEM_PMA_XCVR_POWER_STATE,
                0x1,
            ); /*0x1c10*/
        } else if speed == FXMAC_SPEED_5000 {
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_SRC_SEL_LN, 0x1); /*0x1c04*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_DIV_SEL0_LN, 0x8); /*0x1c08*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_DIV_SEL1_LN, 0x2); /*0x1c0c*/
            FXMAC_WRITEREG32(
                instance_p.config.base_address,
                FXMAC_GEM_PMA_XCVR_POWER_STATE,
                0,
            ); /*0x1c10*/
        }
    } else if instance_p.config.interface == FXmacPhyInterface::FXMAC_PHY_INTERFACE_MODE_5GBASER {
        if speed == FXMAC_SPEED_5000 {
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_SRC_SEL_LN, 0x1); /*0x1c04*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_DIV_SEL0_LN, 0x8); /*0x1c08*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_DIV_SEL1_LN, 0x2); /*0x1c0c*/
            FXMAC_WRITEREG32(
                instance_p.config.base_address,
                FXMAC_GEM_PMA_XCVR_POWER_STATE,
                0x0,
            ); /*0x1c10*/
        }
    } else if instance_p.config.interface == FXmacPhyInterface::FXMAC_PHY_INTERFACE_MODE_2500BASEX {
        if speed == FXMAC_SPEED_25000 {
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_SRC_SEL_LN, 0x1); /*0x1c04*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_DIV_SEL0_LN, 0x1); /*0x1c08*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_DIV_SEL1_LN, 0x2); /*0x1c0c*/
            FXMAC_WRITEREG32(
                instance_p.config.base_address,
                FXMAC_GEM_PMA_XCVR_POWER_STATE,
                0x1,
            ); /*0x1c10*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL0, 0); /*0x1c20*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL1, 0x1); /*0x1c24*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL2, 0x1); /*0x1c28*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL3, 0x1); /*0x1c2c*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_RX_CLK_SEL0, 0x1); /*0x1c30*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_RX_CLK_SEL1, 0x0); /*0x1c34*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL3_0, 0x0); /*0x1c70*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL4_0, 0x0); /*0x1c74*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_RX_CLK_SEL3_0, 0x0); /*0x1c78*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_RX_CLK_SEL4_0, 0x0);
            // 0x1c7c
        }
    } else if instance_p.config.interface == FXmacPhyInterface::FXMAC_PHY_INTERFACE_MODE_SGMII {
        info!("FXMAC_PHY_INTERFACE_MODE_SGMII init");
        if speed == FXMAC_SPEED_2500 {
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_SRC_SEL_LN, 0);
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_DIV_SEL0_LN, 0x1);
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_DIV_SEL1_LN, 0x2);
            FXMAC_WRITEREG32(
                instance_p.config.base_address,
                FXMAC_GEM_PMA_XCVR_POWER_STATE,
                0x1,
            );
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL0, 0);
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL1, 0x1);
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL2, 0x1);
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL3, 0x1);
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_RX_CLK_SEL0, 0x1);
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_RX_CLK_SEL1, 0x0);
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL3_0, 0x0); /*0x1c70*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL4_0, 0x0); /*0x1c74*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_RX_CLK_SEL3_0, 0x0); /*0x1c78*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_RX_CLK_SEL4_0, 0x0);
        // 0x1c7c
        } else if speed == FXMAC_SPEED_1000 {
            info!("sgmii FXMAC_SPEED_1000");
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_SRC_SEL_LN, 0x1); /*0x1c04*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_DIV_SEL0_LN, 0x4); /*0x1c08*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_DIV_SEL1_LN, 0x8); /*0x1c0c*/
            FXMAC_WRITEREG32(
                instance_p.config.base_address,
                FXMAC_GEM_PMA_XCVR_POWER_STATE,
                0x1,
            ); /*0x1c10*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL0, 0x0); /*0x1c20*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL1, 0x0); /*0x1c24*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL2, 0x0); /*0x1c28*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL3, 0x1); /*0x1c2c*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_RX_CLK_SEL0, 0x1); /*0x1c30*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_RX_CLK_SEL1, 0x0); /*0x1c34*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL3_0, 0x0); /*0x1c70*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL4_0, 0x0); /*0x1c74*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_RX_CLK_SEL3_0, 0x0); /*0x1c78*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_RX_CLK_SEL4_0, 0x0);
        // 0x1c7c
        } else if (speed == FXMAC_SPEED_100) || (speed == FXMAC_SPEED_10) {
            info!("sgmii FXMAC_SPEED_{}", speed);

            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_SRC_SEL_LN, 0x1); /*0x1c04*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_DIV_SEL0_LN, 0x4); /*0x1c08*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_DIV_SEL1_LN, 0x8); /*0x1c0c*/
            FXMAC_WRITEREG32(
                instance_p.config.base_address,
                FXMAC_GEM_PMA_XCVR_POWER_STATE,
                0x1,
            ); /*0x1c10*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL0, 0x0); /*0x1c20*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL1, 0x0); /*0x1c24*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL2, 0x1); /*0x1c28*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL3, 0x1); /*0x1c2c*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_RX_CLK_SEL0, 0x1); /*0x1c30*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_RX_CLK_SEL1, 0x0); /*0x1c34*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL3_0, 0x1); /*0x1c70*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL4_0, 0x0); /*0x1c74*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_RX_CLK_SEL3_0, 0x0); /*0x1c78*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_RX_CLK_SEL4_0, 0x1);
            // 0x1c7c
        }
    } else if instance_p.config.interface == FXmacPhyInterface::FXMAC_PHY_INTERFACE_MODE_RGMII {
        info!("FXMAC_PHY_INTERFACE_MODE_RGMII init");
        if speed == FXMAC_SPEED_1000 {
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_MII_SELECT, 0x1); /*0x1c18*/
            FXMAC_WRITEREG32(
                instance_p.config.base_address,
                FXMAC_GEM_SEL_MII_ON_RGMII,
                0x0,
            ); /*0x1c1c*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL0, 0x0); /*0x1c20*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL1, 0x1); /*0x1c24*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL2, 0x0); /*0x1c28*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL3, 0x0); /*0x1c2c*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_RX_CLK_SEL0, 0x0); /*0x1c30*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_RX_CLK_SEL1, 0x1); /*0x1c34*/
            FXMAC_WRITEREG32(
                instance_p.config.base_address,
                FXMAC_GEM_CLK_250M_DIV10_DIV100_SEL,
                0x0,
            ); /*0x1c38*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_RX_CLK_SEL5, 0x1); /*0x1c48*/
            FXMAC_WRITEREG32(
                instance_p.config.base_address,
                FXMAC_GEM_RGMII_TX_CLK_SEL0,
                0x1,
            ); /*0x1c80*/
            FXMAC_WRITEREG32(
                instance_p.config.base_address,
                FXMAC_GEM_RGMII_TX_CLK_SEL1,
                0x0,
            ); /*0x1c84*/
        } else if speed == FXMAC_SPEED_100 {
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_MII_SELECT, 0x1); /*0x1c18*/
            FXMAC_WRITEREG32(
                instance_p.config.base_address,
                FXMAC_GEM_SEL_MII_ON_RGMII,
                0x0,
            ); /*0x1c1c*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL0, 0x0); /*0x1c20*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL1, 0x1); /*0x1c24*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL2, 0x0); /*0x1c28*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL3, 0x0); /*0x1c2c*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_RX_CLK_SEL0, 0x0); /*0x1c30*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_RX_CLK_SEL1, 0x1); /*0x1c34*/
            FXMAC_WRITEREG32(
                instance_p.config.base_address,
                FXMAC_GEM_CLK_250M_DIV10_DIV100_SEL,
                0x0,
            ); /*0x1c38*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_RX_CLK_SEL5, 0x1); /*0x1c48*/
            FXMAC_WRITEREG32(
                instance_p.config.base_address,
                FXMAC_GEM_RGMII_TX_CLK_SEL0,
                0x0,
            ); /*0x1c80*/
            FXMAC_WRITEREG32(
                instance_p.config.base_address,
                FXMAC_GEM_RGMII_TX_CLK_SEL1,
                0x0,
            ); /*0x1c84*/
        } else {
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_MII_SELECT, 0x1); /*0x1c18*/
            FXMAC_WRITEREG32(
                instance_p.config.base_address,
                FXMAC_GEM_SEL_MII_ON_RGMII,
                0x0,
            ); /*0x1c1c*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL0, 0x0); /*0x1c20*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL1, 0x1); /*0x1c24*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL2, 0x0); /*0x1c28*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_TX_CLK_SEL3, 0x0); /*0x1c2c*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_RX_CLK_SEL0, 0x0); /*0x1c30*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_RX_CLK_SEL1, 0x1); /*0x1c34*/
            FXMAC_WRITEREG32(
                instance_p.config.base_address,
                FXMAC_GEM_CLK_250M_DIV10_DIV100_SEL,
                0x1,
            ); /*0x1c38*/
            FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_RX_CLK_SEL5, 0x1); /*0x1c48*/
            FXMAC_WRITEREG32(
                instance_p.config.base_address,
                FXMAC_GEM_RGMII_TX_CLK_SEL0,
                0x0,
            ); /*0x1c80*/
            FXMAC_WRITEREG32(
                instance_p.config.base_address,
                FXMAC_GEM_RGMII_TX_CLK_SEL1,
                0x0,
            ); /*0x1c84*/
        }
    } else if instance_p.config.interface == FXmacPhyInterface::FXMAC_PHY_INTERFACE_MODE_RMII {
        FXMAC_WRITEREG32(instance_p.config.base_address, FXMAC_GEM_RX_CLK_SEL5, 0x1);
        // 0x1c48
    }

    FXmacHighSpeedConfiguration(instance_p, speed);
}

fn FXmacHighSpeedConfiguration(instance_p: &mut FXmac, speed: u32) {
    let mut reg_value: u32 = 0;
    let mut set_speed: i32 = 0;
    match speed {
        FXMAC_SPEED_25000 => {
            set_speed = 2;
        }
        FXMAC_SPEED_10000 => {
            set_speed = 4;
        }
        FXMAC_SPEED_5000 => {
            set_speed = 3;
        }
        FXMAC_SPEED_2500 => {
            set_speed = 2;
        }
        FXMAC_SPEED_1000 => {
            set_speed = 1;
        }
        _ => {
            set_speed = 0;
        }
    }

    // GEM_HSMAC(0x0050) provide rate to the external
    reg_value = read_reg((instance_p.config.base_address + FXMAC_GEM_HSMAC as u64) as *const u32);
    reg_value &= !FXMAC_GEM_HSMACSPEED_MASK;
    reg_value |= (set_speed as u32) & FXMAC_GEM_HSMACSPEED_MASK;
    write_reg(
        (instance_p.config.base_address + FXMAC_GEM_HSMAC as u64) as *mut u32,
        reg_value,
    );

    reg_value = read_reg((instance_p.config.base_address + FXMAC_GEM_HSMAC as u64) as *const u32);

    info!("FXMAC_GEM_HSMAC is {:#x}", reg_value);
}

/// FXmacInitInterface
/// Initialize the MAC controller configuration based on the PHY interface type
fn FXmacInitInterface(instance_p: &mut FXmac) {
    let mut config: u32 = 0;
    let mut control: u32 = 0;

    info!(
        "FXmacInitInterface, PHY MODE:{:?}",
        instance_p.config.interface
    );

    if instance_p.config.interface == FXmacPhyInterface::FXMAC_PHY_INTERFACE_MODE_XGMII {
        config = read_reg((instance_p.config.base_address + FXMAC_NWCFG_OFFSET) as *const u32);
        config &= !FXMAC_NWCFG_PCSSEL_MASK;
        write_reg(
            (instance_p.config.base_address + FXMAC_NWCFG_OFFSET) as *mut u32,
            config,
        );

        control = read_reg((instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *const u32);
        control |= FXMAC_NWCTRL_ENABLE_HS_MAC_MASK; /* Use high speed MAC */
        write_reg(
            (instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *mut u32,
            control,
        );

        instance_p.config.duplex = 1;
    } else if (instance_p.config.interface == FXmacPhyInterface::FXMAC_PHY_INTERFACE_MODE_USXGMII)
        || (instance_p.config.interface == FXmacPhyInterface::FXMAC_PHY_INTERFACE_MODE_5GBASER)
    {
        info!("usx interface is {:?}", instance_p.config.interface);
        // network_config
        instance_p.config.duplex = 1;
        config = read_reg((instance_p.config.base_address + FXMAC_NWCFG_OFFSET) as *const u32);
        config |= FXMAC_NWCFG_PCSSEL_MASK;
        config &= !FXMAC_NWCFG_100_MASK;
        config &= !FXMAC_NWCFG_SGMII_MODE_ENABLE_MASK;
        if (instance_p.config.duplex == 1) {
            info!("is duplex");
            config |= FXMAC_NWCFG_FDEN_MASK;
        }

        write_reg(
            (instance_p.config.base_address + FXMAC_NWCFG_OFFSET) as *mut u32,
            config,
        );

        // network_control
        control = read_reg((instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *const u32);
        control |= FXMAC_NWCTRL_ENABLE_HS_MAC_MASK; /* Use high speed MAC */
        write_reg(
            (instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *mut u32,
            control,
        );

        // High speed PCS control register
        control =
            read_reg((instance_p.config.base_address + FXMAC_GEM_USX_CONTROL_OFFSET) as *const u32);

        if (instance_p.config.speed == FXMAC_SPEED_10000) {
            info!("is 10G");
            control |= FXMAC_GEM_USX_HS_MAC_SPEED_10G;
            control |= FXMAC_GEM_USX_SERDES_RATE_10G;
        } else if (instance_p.config.speed == FXMAC_SPEED_25000) {
            control |= FXMAC_GEM_USX_HS_MAC_SPEED_2_5G;
        } else if (instance_p.config.speed == FXMAC_SPEED_1000) {
            control |= FXMAC_GEM_USX_HS_MAC_SPEED_1G;
        } else if (instance_p.config.speed == FXMAC_SPEED_100) {
            control |= FXMAC_GEM_USX_HS_MAC_SPEED_100M;
        } else if (instance_p.config.speed == FXMAC_SPEED_5000) {
            control |= FXMAC_GEM_USX_HS_MAC_SPEED_5G;
            control |= FXMAC_GEM_USX_SERDES_RATE_5G;
        }

        control &= !(FXMAC_GEM_USX_TX_SCR_BYPASS | FXMAC_GEM_USX_RX_SCR_BYPASS);
        control |= FXMAC_GEM_USX_RX_SYNC_RESET;
        write_reg(
            (instance_p.config.base_address + FXMAC_GEM_USX_CONTROL_OFFSET) as *mut u32,
            control,
        );

        control =
            read_reg((instance_p.config.base_address + FXMAC_GEM_USX_CONTROL_OFFSET) as *const u32);
        control &= !FXMAC_GEM_USX_RX_SYNC_RESET;
        control |= FXMAC_GEM_USX_TX_DATAPATH_EN;
        control |= FXMAC_GEM_USX_SIGNAL_OK;

        write_reg(
            (instance_p.config.base_address + FXMAC_GEM_USX_CONTROL_OFFSET) as *mut u32,
            control,
        );
    } else if instance_p.config.interface == FXmacPhyInterface::FXMAC_PHY_INTERFACE_MODE_2500BASEX {
        // network_config
        instance_p.config.duplex = 1;
        config = read_reg((instance_p.config.base_address + FXMAC_NWCFG_OFFSET) as *const u32);
        config |= FXMAC_NWCFG_PCSSEL_MASK | FXMAC_NWCFG_SGMII_MODE_ENABLE_MASK;
        config &= !FXMAC_NWCFG_100_MASK;

        if (instance_p.config.duplex == 1) {
            config |= FXMAC_NWCFG_FDEN_MASK;
        }
        write_reg(
            (instance_p.config.base_address + FXMAC_NWCFG_OFFSET) as *mut u32,
            config,
        );

        // network_control
        control = read_reg((instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *const u32);
        control &= !FXMAC_NWCTRL_ENABLE_HS_MAC_MASK;
        control |= FXMAC_NWCTRL_TWO_PT_FIVE_GIG_MASK; /* Use high speed MAC */
        write_reg(
            (instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *mut u32,
            control,
        );

        // High speed PCS control register
        control =
            read_reg((instance_p.config.base_address + FXMAC_GEM_USX_CONTROL_OFFSET) as *const u32);

        if (instance_p.config.speed == FXMAC_SPEED_25000) {
            control |= FXMAC_GEM_USX_HS_MAC_SPEED_2_5G;
        }

        control &= !(FXMAC_GEM_USX_TX_SCR_BYPASS | FXMAC_GEM_USX_RX_SCR_BYPASS);
        control |= FXMAC_GEM_USX_RX_SYNC_RESET;
        write_reg(
            (instance_p.config.base_address + FXMAC_GEM_USX_CONTROL_OFFSET) as *mut u32,
            control,
        );

        control =
            read_reg((instance_p.config.base_address + FXMAC_GEM_USX_CONTROL_OFFSET) as *const u32);
        control &= !FXMAC_GEM_USX_RX_SYNC_RESET;
        control |= FXMAC_GEM_USX_TX_DATAPATH_EN;
        control |= FXMAC_GEM_USX_SIGNAL_OK;

        write_reg(
            (instance_p.config.base_address + FXMAC_GEM_USX_CONTROL_OFFSET) as *mut u32,
            control,
        );
    } else if instance_p.config.interface == FXmacPhyInterface::FXMAC_PHY_INTERFACE_MODE_SGMII {
        config = read_reg((instance_p.config.base_address + FXMAC_NWCFG_OFFSET) as *const u32);
        config |= FXMAC_NWCFG_PCSSEL_MASK | FXMAC_NWCFG_SGMII_MODE_ENABLE_MASK;

        config &= !(FXMAC_NWCFG_100_MASK
            | FXMAC_NWCFG_FDEN_MASK
            | FXMAC_NWCFG_LENGTH_FIELD_ERROR_FRAME_DISCARD_MASK);

        if instance_p.moudle_id >= 2 {
            config &= !FXMAC_NWCFG_1000_MASK;
        }

        if instance_p.config.duplex != 0 {
            config |= FXMAC_NWCFG_FDEN_MASK;
        }

        if instance_p.config.speed == FXMAC_SPEED_100 {
            config |= FXMAC_NWCFG_100_MASK;
        } else if instance_p.config.speed == FXMAC_SPEED_1000 {
            config |= FXMAC_NWCFG_1000_MASK;
        }

        write_reg(
            (instance_p.config.base_address + FXMAC_NWCFG_OFFSET) as *mut u32,
            config,
        );

        if instance_p.config.speed == FXMAC_SPEED_2500 {
            control =
                read_reg((instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *const u32);
            control |= FXMAC_NWCTRL_TWO_PT_FIVE_GIG_MASK;
            write_reg(
                (instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *mut u32,
                control,
            );
        } else {
            control =
                read_reg((instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *const u32);
            control &= !FXMAC_NWCTRL_TWO_PT_FIVE_GIG_MASK;
            write_reg(
                (instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *mut u32,
                control,
            );
        }

        control = read_reg((instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *const u32);
        control &= !FXMAC_NWCTRL_ENABLE_HS_MAC_MASK;
        write_reg(
            (instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *mut u32,
            control,
        );

        control =
            read_reg((instance_p.config.base_address + FXMAC_PCS_CONTROL_OFFSET) as *const u32);
        control |= FXMAC_PCS_CONTROL_ENABLE_AUTO_NEG;
        write_reg(
            (instance_p.config.base_address + FXMAC_PCS_CONTROL_OFFSET) as *mut u32,
            control,
        );
    } else {
        config = read_reg((instance_p.config.base_address + FXMAC_NWCFG_OFFSET) as *const u32);

        info!("select rgmii");

        config &= !FXMAC_NWCFG_PCSSEL_MASK;
        config &= !(FXMAC_NWCFG_100_MASK | FXMAC_NWCFG_FDEN_MASK);

        if instance_p.moudle_id >= 2 {
            config &= !FXMAC_NWCFG_1000_MASK;
        }

        if instance_p.config.duplex != 0 {
            config |= FXMAC_NWCFG_FDEN_MASK;
        }

        if instance_p.config.speed == FXMAC_SPEED_100 {
            config |= FXMAC_NWCFG_100_MASK;
        } else if instance_p.config.speed == FXMAC_SPEED_1000 {
            config |= FXMAC_NWCFG_1000_MASK;
        }

        write_reg(
            (instance_p.config.base_address + FXMAC_NWCFG_OFFSET) as *mut u32,
            config,
        );

        control = read_reg((instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *const u32);
        control &= !FXMAC_NWCTRL_ENABLE_HS_MAC_MASK; /* Use high speed MAC */
        write_reg(
            (instance_p.config.base_address + FXMAC_NWCTRL_OFFSET) as *mut u32,
            control,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_REGISTER_WORDS: usize = FXMAC_MMIO_SIZE / size_of::<u32>();

    fn test_irq_port(registers: &mut [u32; TEST_REGISTER_WORDS]) -> FXmacIrqPort {
        let discovery = unsafe {
            discover_xmac(
                NonNull::new(registers.as_mut_ptr().cast::<u8>()).unwrap(),
                FXMAC_MMIO_SIZE,
            )
            .unwrap()
        };
        let (_pending, port) = discovery.into_parts();
        port.irq_config
            .clear_on_write
            .store(true, Ordering::Release);
        port
    }

    fn test_initialization(registers: &mut [u32; TEST_REGISTER_WORDS]) -> FXmacInitialization {
        let discovery = unsafe {
            discover_xmac(
                NonNull::new(registers.as_mut_ptr().cast::<u8>()).unwrap(),
                FXMAC_MMIO_SIZE,
            )
            .unwrap()
        };
        let (pending, _port) = discovery.into_parts();
        begin_xmac_init(pending)
    }

    #[test]
    fn irq_status_ignores_non_queue_interrupts() {
        let empty = FXmacIrqStatus::from_raw(0);
        assert!(!empty.tx_ready());
        assert!(!empty.rx_ready());

        let link_only = FXmacIrqStatus::from_raw(FXMAC_IXR_LINKCHANGE_MASK);
        assert!(!link_only.tx_ready());
        assert!(!link_only.rx_ready());
    }

    #[test]
    fn irq_status_reports_queue_interrupts() {
        let tx = FXmacIrqStatus::from_raw(FXMAC_IXR_TXCOMPL_MASK);
        assert!(tx.tx_ready());
        assert!(!tx.rx_ready());

        let rx = FXmacIrqStatus::from_raw(FXMAC_IXR_RXCOMPL_MASK);
        assert!(!rx.tx_ready());
        assert!(rx.rx_ready());
    }

    #[test]
    fn empty_shared_irq_does_not_mask_fxmac_source() {
        let mut registers = [0_u32; TEST_REGISTER_WORDS];
        let mut port = test_irq_port(&mut registers);

        assert!(port.capture_and_mask().is_empty());
        assert_eq!(registers[FXMAC_IDR_OFFSET as usize / size_of::<u32>()], 0);
    }

    #[test]
    fn unrelated_shared_irq_status_is_not_claimed_or_masked() {
        let mut registers = [0_u32; TEST_REGISTER_WORDS];
        registers[FXMAC_ISR_OFFSET as usize / size_of::<u32>()] = FXMAC_IXR_MGMNT_MASK;
        let mut port = test_irq_port(&mut registers);

        assert!(port.capture_and_mask().is_empty());
        assert_eq!(registers[FXMAC_IDR_OFFSET as usize / size_of::<u32>()], 0);
    }

    #[test]
    fn captured_irq_is_acknowledged_and_left_device_masked() {
        let mut registers = [0_u32; TEST_REGISTER_WORDS];
        registers[FXMAC_ISR_OFFSET as usize / size_of::<u32>()] = FXMAC_IXR_RXCOMPL_MASK;
        let mut port = test_irq_port(&mut registers);

        let status = port.capture_and_mask();

        assert_eq!(status.raw(), FXMAC_IXR_RXCOMPL_MASK);
        assert_eq!(
            registers[FXMAC_IDR_OFFSET as usize / size_of::<u32>()],
            FXMAC_RUNTIME_IRQ_MASK
        );
        assert_eq!(
            registers[FXMAC_ISR_OFFSET as usize / size_of::<u32>()],
            FXMAC_IXR_RXCOMPL_MASK
        );
    }

    #[test]
    fn discovery_rejects_a_mapping_that_cannot_cover_extended_registers() {
        let mut registers = [0_u32; 4];
        let error = unsafe {
            discover_xmac(
                NonNull::new(registers.as_mut_ptr().cast::<u8>()).unwrap(),
                registers.len() * size_of::<u32>(),
            )
            .err()
            .unwrap()
        };

        assert_eq!(
            error,
            FXmacDiscoveryError::RangeTooSmall {
                provided: registers.len() * size_of::<u32>(),
                required: FXMAC_MMIO_SIZE,
            }
        );
        assert_eq!(registers, [0; 4]);
    }

    #[test]
    fn busy_mdio_returns_an_absolute_deadline_and_times_out_without_spinning() {
        let mut registers = [0_u32; TEST_REGISTER_WORDS];
        let mut initialization = test_initialization(&mut registers);
        initialization.stage = FXmacInitStage::DetectStatus { phy_address: 0 };

        let first = poll_xmac_init(&mut initialization, 100);
        assert!(matches!(
            first,
            FXmacInitPoll::Pending(FXmacInitSchedule {
                run_again: false,
                wake_at_ns: Some(50_100),
            })
        ));

        let timed_out = poll_xmac_init(&mut initialization, 10_000_100);
        assert!(matches!(
            timed_out,
            FXmacInitPoll::Failed(FXmacInitError::MdioTimeout)
        ));
    }

    #[test]
    fn manual_phy_settle_uses_the_original_absolute_deadline() {
        let mut registers = [0_u32; TEST_REGISTER_WORDS];
        let mut initialization = test_initialization(&mut registers);
        initialization.stage = FXmacInitStage::ManualSettle { wake_at_ns: 5_000 };

        let pending = poll_xmac_init(&mut initialization, 4_000);
        assert!(matches!(
            pending,
            FXmacInitPoll::Pending(FXmacInitSchedule {
                run_again: false,
                wake_at_ns: Some(5_000),
            })
        ));

        let due = poll_xmac_init(&mut initialization, 5_000);
        assert!(matches!(
            due,
            FXmacInitPoll::Pending(FXmacInitSchedule {
                run_again: true,
                wake_at_ns: None,
            })
        ));
    }
}
