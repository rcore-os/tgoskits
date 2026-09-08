//! Core FXMAC Ethernet controller functionality.
//!
//! This module provides the main data structures and functions for controlling
//! the FXMAC Ethernet MAC controller.

use alloc::sync::Arc;

use dma_api::{DeviceDma, DmaError};
use mmio_api::Mmio;

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

const FXMAC_MODE_SEL_OFFSET: u64 = 0x1c00;
const FXMAC_LOOPBACK_SEL_OFFSET: u64 = 0x1c04;
const FXMAC_DEFAULT_QUEUE_COUNT: u32 = 4;

/// Mask for upper 32 bits of 64-bit address.
pub const ULONG64_HI_MASK: u64 = 0xFFFFFFFF00000000;
/// Mask for lower 32 bits of 64-bit address.
pub const ULONG64_LO_MASK: u64 = !ULONG64_HI_MASK;

/// Component is initialized and ready.
pub const FT_COMPONENT_IS_READY: u32 = 0x11111111;
/// Component is started.
pub const FT_COMPONENT_IS_STARTED: u32 = 0x22222222;

/// Minimum register aperture required by the controller core and clock block.
pub const FXMAC_MMIO_REQUIRED_SIZE: usize = 0x2000;

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

/// Owned hard-IRQ endpoint for one FXMAC controller.
///
/// The endpoint owns a reference to the register mapping, but none of the DMA
/// rings or mutable controller state. Hard IRQ context can therefore snapshot,
/// mask, and acknowledge the source without taking the task-side device lock.
pub struct FxmacIrqEndpoint {
    _mmio: Arc<Mmio>,
    base_address: u64,
    clear_on_write: bool,
    mask: u32,
}

/// Result of constructing the task-side controller owner and its hard-IRQ
/// endpoint as one indivisible device capability.
pub type FxmacInitResult = Result<(FXmac, FxmacIrqEndpoint), FxmacInitError>;

impl FxmacIrqEndpoint {
    /// Snapshots the pending IRQ bits and masks the controller until the
    /// deferred owner has consumed the snapshot.
    pub fn snapshot_and_mask(&mut self) -> FXmacIrqStatus {
        snapshot_and_mask_irq(self.base_address, self.clear_on_write, self.mask)
    }
}

/// Main FXMAC Ethernet controller instance.
///
/// This structure holds all state information for an FXMAC controller instance,
/// including configuration, DMA queues, and runtime status.
///
/// # Example
///
/// ```ignore
/// let (fxmac, irq_endpoint) = xmac_init(device_dma, mmio, hardware)?;
///
/// // Check link status
/// if fxmac.is_link_up() {
///     println!("Network link is up!");
/// }
/// ```
pub struct FXmac {
    _mmio: Arc<Mmio>,
    pub(crate) config: FXmacConfig,
    pub(crate) is_ready: u32,
    pub(crate) is_started: u32,
    pub(crate) link_status: u32,
    pub(crate) options: u32,
    pub(crate) mask: u32,
    pub(crate) caps: u32,
    pub(crate) lwipport: FXmacLwipPort,
    pub(crate) tx_bd_queue: FXmacQueue,
    pub(crate) rx_bd_queue: FXmacQueue,
    pub(crate) moudle_id: u32,
    pub(crate) max_mtu_size: u32,
    pub(crate) max_frame_size: u32,
    pub(crate) phy_address: u32,
    pub(crate) rxbuf_mask: u32,
}

/// Firmware-provided hardware properties that are not encoded in the register
/// aperture itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FxmacHardwareConfig {
    peripheral_clock_hz: core::num::NonZeroU32,
}

impl FxmacHardwareConfig {
    pub const fn new(peripheral_clock_hz: core::num::NonZeroU32) -> Self {
        Self {
            peripheral_clock_hz,
        }
    }
}

/// Hardware configuration for the FXMAC controller.
///
/// This structure contains all hardware-level configuration parameters
/// required to initialize and operate the FXMAC Ethernet controller.
pub(crate) struct FXmacConfig {
    /// Base address of the MAC controller registers.
    pub(crate) base_address: u64,
    /// Base address for extended mode configuration.
    pub(crate) extral_mode_base: u64,
    /// Base address for loopback configuration.
    pub(crate) extral_loopback_base: u64,
    /// PHY interface type (SGMII, RGMII, etc.).
    pub(crate) interface: FXmacPhyInterface,
    /// Link speed in Mbps (10, 100, 1000, etc.).
    pub(crate) speed: u32,
    /// Duplex mode: 1 for full-duplex, 0 for half-duplex.
    pub(crate) duplex: u32,
    /// Auto-negotiation enable: 1 to enable, 0 to disable.
    pub(crate) auto_neg: u32,
    /// Peripheral clock frequency in Hz.
    pub(crate) pclk_hz: u32,
    /// Maximum number of hardware queues.
    pub(crate) max_queue_num: u32,
    /// TX queue index within `max_queue_num`.
    pub(crate) tx_queue_id: u32,
    /// RX queue index within `max_queue_num`.
    pub(crate) rx_queue_id: u32,
    /// DMA burst length setting.
    pub(crate) dma_brust_length: u32,
    /// Default network configuration options.
    pub(crate) network_default_config: u32,
    /// Capability flags (e.g., tail pointer support).
    pub(crate) caps: u32,
    /// MAC address (6 bytes).
    pub(crate) mac: [u8; 6],
}

/// Hardware queue structure for TX/RX operations.
pub(crate) struct FXmacQueue {
    /// Queue identifier.
    pub(crate) queue_id: u32,
    /// Buffer descriptor ring for this queue.
    pub(crate) bdring: FXmacBdRing,
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

/// Reads a register from the CPU-visible mapping owned by [`FXmac`].
pub(crate) fn read_reg<T>(src: *const T) -> T {
    unsafe { core::ptr::read_volatile(src) }
}

/// Writes a register in the CPU-visible mapping owned by [`FXmac`].
pub(crate) fn write_reg<T>(dst: *mut T, value: T) {
    unsafe { core::ptr::write_volatile(dst, value) }
}

#[derive(Debug, thiserror::Error)]
pub enum FxmacInitError {
    #[error(transparent)]
    Dma(#[from] DmaError),
    #[error("FXMAC MMIO aperture is too small: {actual:#x} < {required:#x}")]
    MmioTooSmall { actual: usize, required: usize },
    #[error("FXMAC initialization failed during {stage} with status {status}")]
    Hardware { stage: FxmacInitStage, status: u32 },
    #[error("FXMAC RX descriptor allocation returned a null descriptor")]
    NullRxDescriptor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FxmacInitStage {
    ProgramMacAddress,
    InitializePhy,
    CreateRxRing,
    CloneRxRing,
    CreateTxRing,
    CloneTxRing,
    AllocateRxDescriptor,
    SubmitRxDescriptor,
}

impl core::fmt::Display for FxmacInitStage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::ProgramMacAddress => "MAC address programming",
            Self::InitializePhy => "PHY initialization",
            Self::CreateRxRing => "RX ring creation",
            Self::CloneRxRing => "RX ring initialization",
            Self::CreateTxRing => "TX ring creation",
            Self::CloneTxRing => "TX ring initialization",
            Self::AllocateRxDescriptor => "RX descriptor allocation",
            Self::SubmitRxDescriptor => "RX descriptor submission",
        })
    }
}

pub(crate) fn init_status(stage: FxmacInitStage, status: u32) -> Result<(), FxmacInitError> {
    if status == 0 {
        Ok(())
    } else {
        Err(FxmacInitError::Hardware { stage, status })
    }
}

/// Initializes the FXMAC Ethernet controller.
///
/// This function performs complete hardware initialization of the FXMAC controller,
/// including:
/// - Hardware reset and configuration
/// - PHY initialization and link establishment
/// - DMA buffer descriptor ring setup
/// - Interrupt handler registration
/// - MAC address configuration
///
/// # Arguments
///
/// * `dma` - Device-scoped DMA allocation capability.
/// * `mmio` - Owned mapping for the complete controller register aperture.
/// * `hardware` - Firmware-resolved clock properties for this controller.
///
/// # Returns
///
/// An owned initialized [`FXmac`] instance and its unique hard-IRQ endpoint.
///
/// # Example
///
/// ```ignore
/// // Initialize the controller
/// let (fxmac, irq_endpoint) = xmac_init(dma, mmio, hardware)?;
///
/// // The controller is configured but remains stopped until its IRQ endpoint
/// // is registered and enabled by the OS runtime.
/// ```
///
/// # Note
///
/// The caller owns the controller, MMIO mapping, and DMA buffers as one
/// lifetime-bound device object.
pub fn xmac_init(dma: DeviceDma, mmio: Mmio, hardware: FxmacHardwareConfig) -> FxmacInitResult {
    if mmio.size() < FXMAC_MMIO_REQUIRED_SIZE {
        return Err(FxmacInitError::MmioTooSmall {
            actual: mmio.size(),
            required: FXMAC_MMIO_REQUIRED_SIZE,
        });
    }
    let mmio = Arc::new(mmio);
    let base_address = mmio.as_ptr() as u64;
    let hwaddr = read_mac_address(base_address, 0);

    let mut mac_config: FXmacConfig = FXmacConfig {
        base_address,
        extral_mode_base: base_address + FXMAC_MODE_SEL_OFFSET,
        extral_loopback_base: base_address + FXMAC_LOOPBACK_SEL_OFFSET,
        interface: FXmacPhyInterface::FXMAC_PHY_INTERFACE_MODE_SGMII,
        speed: 100,
        duplex: 1,
        auto_neg: 0,
        pclk_hz: hardware.peripheral_clock_hz.get(),
        max_queue_num: FXMAC_DEFAULT_QUEUE_COUNT,
        tx_queue_id: 0,
        rx_queue_id: 0,
        dma_brust_length: 16,
        network_default_config: FXMAC_DEFAULT_OPTIONS,
        caps: 0,
        mac: hwaddr,
    };

    let feature = FXMAC_LWIP_PORT_CONFIG_MULTICAST_ADDRESS_FILITER;
    let frame_size = if (feature & FXMAC_LWIP_PORT_CONFIG_JUMBO) != 0 {
        FXMAC_MAX_FRAME_SIZE_JUMBO as usize
    } else {
        FXMAC_MAX_FRAME_SIZE as usize
    };
    let buffer = FXmacNetifBuffer::new(&dma, frame_size)?;

    let mut xmac = FXmac {
        _mmio: Arc::clone(&mmio),
        config: mac_config,
        is_ready: FT_COMPONENT_IS_READY,
        is_started: 0,
        link_status: FXMAC_LINKDOWN,
        options: 0,
        mask: 0,
        caps: 0,
        lwipport: FXmacLwipPort {
            buffer,
            feature,
            hwaddr,
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

    // xmac_config: interface=FXMAC_PHY_INTERFACE_MODE_SGMII, autonegotiation=0, phy_speed=FXMAC_PHY_SPEED_100M, phy_duplex=FXMAC_PHY_FULL_DUPLEX
    // FXmacDmaReset, moudle_id=12, max_frame_size=1518, max_queue_num=4 (或16), dma_brust_length=16
    // network_default_config = 0x37f0, base_address=0x3200c000,FXMAC_RXBUF_HASH_MASK: GENMASK(30, 29)= 0x60000000 (0b 0110_0000_0000_0000_0000000000000000)

    // mii_interface = 1 = FXMAC_LWIP_PORT_INTERFACE_SGMII;

    // FXmacLwipPortInit():
    // step 1: initialize instance
    // step 2: depend on config set some options : JUMBO / IGMP
    // step 3: FXmacSelectClk
    // step 4: FXmacInitInterface
    // step 5: initialize phy
    // step 6: initialize dma
    // step 7: initialize interrupt
    // step 8: start mac

    // Reset the hardware and set default options
    // xmac.link_status = FXMAC_LINKDOWN;
    // xmac.is_ready = FT_COMPONENT_IS_READY;

    FXmacReset(&mut xmac);

    // irq_handler = (FXmacIrqHandler)FXmacIrqStubHandler;
    // interrupts bit mask
    xmac.mask = FXMAC_IXR_LINKCHANGE_MASK
        | FXMAC_IXR_TX_ERR_MASK
        | FXMAC_IXR_RX_ERR_MASK
        | FXMAC_IXR_RXCOMPL_MASK; // FXMAC_INTR_MASK // 这里打开收包中断，关闭发包中断

    if (xmac.config.caps & FXMAC_CAPS_TAILPTR) != 0 {
        FXmacSetOptions(&mut xmac, FXMAC_TAIL_PTR_OPTION, 0);
        xmac.mask &= !FXMAC_IXR_TXUSED_MASK;
    }

    // xmac.lwipport.feature = LWIP_PORT_MODE_MULTICAST_ADDRESS_FILITER;
    FxmacFeatureSetOptions(xmac.lwipport.feature, &mut xmac);

    init_status(
        FxmacInitStage::ProgramMacAddress,
        FXmacSetMacAddress(&mut xmac, &hwaddr, 0),
    )?;

    // mac_config.interface = FXMAC_PHY_INTERFACE_MODE_SGMII;

    if xmac.config.interface != FXmacPhyInterface::FXMAC_PHY_INTERFACE_MODE_USXGMII {
        // initialize phy
        init_status(
            FxmacInitStage::InitializePhy,
            FXmacPhyInit(&mut xmac, XMAC_PHY_RESET_ENABLE),
        )?;
    } else {
        info!("interface == FXMAC_PHY_INTERFACE_MODE_USXGMII");
    }

    FXmacSelectClk(&mut xmac);
    FXmacInitInterface(&mut xmac);

    // initialize dma
    let mut dmacrreg: u32 = read_reg((xmac.config.base_address + FXMAC_DMACR_OFFSET) as *const u32);
    dmacrreg &= !(FXMAC_DMACR_BLENGTH_MASK);
    dmacrreg |= FXMAC_DMACR_INCR16_AHB_AXI_BURST; /* Attempt to use bursts of up to 16. */
    write_reg(
        (xmac.config.base_address + FXMAC_DMACR_OFFSET) as *mut u32,
        dmacrreg,
    );

    FXmacInitDma(&mut xmac)?;

    // end of FXmacLwipPortInit()

    if (xmac.lwipport.feature & FXMAC_LWIP_PORT_CONFIG_UNICAST_ADDRESS_FILITER) != 0 {
        debug!("Set unicast hash table");
        FXmac_SetHash(&mut xmac, &hwaddr);
    }

    // 注册了 lwip_port->ops:
    // ethernetif_link_detect()
    // ethernetif_input()
    // ethernetif_deinit()
    // ethernetif_start() -> FXmacLwipPortStart() -> FXmacStart()
    // ethernetif_debug()

    // 开始发包的函数：FXmacLwipPortTx()->FXmacSgsend() -> FXmacSendHandler() -> FXmacProcessSentBds()
    // 触发中断函数：FXmacIntrHandlerWithStatus()
    // 收包handle: FXmacRecvIsrHandler()->FXmacRecvHandler

    let irq_endpoint = FxmacIrqEndpoint {
        _mmio: mmio,
        base_address: xmac.config.base_address,
        clear_on_write: (xmac.caps & FXMAC_CAPS_ISR_CLEAR_ON_WRITE) != 0,
        mask: xmac.mask,
    };
    Ok((xmac, irq_endpoint))
}

impl FXmac {
    /// Returns the MAC address read from the controller during initialization.
    pub fn mac_address(&self) -> [u8; 6] {
        self.lwipport.hwaddr
    }

    pub fn is_link_up(&self) -> bool {
        self.link_status == FXMAC_LINKUP
    }

    /// Handles a queue interrupt from the OS-registered IRQ callback.
    pub fn handle_irq(&mut self) -> FXmacIrqStatus {
        snapshot_and_mask_irq(
            self.config.base_address,
            (self.caps & FXMAC_CAPS_ISR_CLEAR_ON_WRITE) != 0,
            self.mask,
        )
    }

    /// Consumes a hard-IRQ snapshot in task or deferred context.
    ///
    /// Interrupt rearming is deliberately owned by the queue runtime. Keeping
    /// this operation masked lets the poll owner drain descriptors before the
    /// final `enable_irq`/pending check closes the NAPI rearm window.
    pub fn process_irq_snapshot(&mut self, status: FXmacIrqStatus) {
        if status.is_empty() {
            return;
        }
        crate::fxmac_intr::FXmacIntrHandlerWithStatus(self, status);
    }

    /// Enables queue 0 interrupts after the OS has registered an IRQ handler.
    pub fn enable_irq(&mut self) {
        if self.is_started == FT_COMPONENT_IS_STARTED {
            FXmacQueueIrqEnable(self, 0, self.mask);
        } else {
            FXmacStart(self);
        }
    }

    /// Disables queue 0 interrupts while the OS mutates shared device state.
    pub fn disable_irq(&mut self) {
        FXmacQueueIrqDisable(self, 0, self.mask);
    }
}

fn snapshot_and_mask_irq(base_address: u64, clear_on_write: bool, mask: u32) -> FXmacIrqStatus {
    let status =
        FXmacIrqStatus::from_raw(read_reg((base_address + FXMAC_ISR_OFFSET) as *const u32));
    if status.is_empty() {
        return status;
    }

    write_reg(
        (base_address + FXMAC_IDR_OFFSET) as *mut u32,
        mask & FXMAC_IXR_ALL_MASK,
    );
    if clear_on_write {
        write_reg((base_address + FXMAC_ISR_OFFSET) as *mut u32, status.raw());
    }
    DSB();
    status
}

impl Drop for FXmac {
    fn drop(&mut self) {
        if self.is_ready == FT_COMPONENT_IS_READY {
            FXmacStop(self);
        }
    }
}

/// Starts the Ethernet controller.
///
/// This enables TX/RX paths based on configured options, starts DMA channels,
/// and enables the device interrupt mask.
///
/// # Panics
///
/// Panics if the instance is not in the ready state.
fn FXmacStart(instance_p: &mut FXmac) {
    assert!(instance_p.is_ready == FT_COMPONENT_IS_READY);

    // clear any existed int status
    write_reg(
        (instance_p.config.base_address + FXMAC_ISR_OFFSET) as *mut u32,
        FXMAC_IXR_ALL_MASK,
    );

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

    info!("Enable TX and RX by Mask={:#x}", instance_p.mask);

    // 使能网卡中断: Enable TX and RX interrupt
    // FXMAC_INT_ENABLE(instance_p, instance_p->mask);
    // Enable interrupts specified in 'Mask'. The corresponding interrupt for each bit set to 1 in 'Mask', will be enabled.
    write_reg(
        (instance_p.config.base_address + FXMAC_IER_OFFSET) as *mut u32,
        instance_p.mask & FXMAC_IXR_ALL_MASK,
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
fn FXmacStop(instance_p: &mut FXmac) {
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
    DSB();

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
    let base = instance_p.config.base_address;

    // Stop the device and reset hardware
    FXmacStop(instance_p);

    // Module identification number
    // instance_p->moudle_id = 12
    instance_p.moudle_id = (read_reg((base + FXMAC_REVISION_REG_OFFSET) as *const u32)
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

    let netctrl =
        (FXMAC_NWCTRL_STATCLR_MASK & !FXMAC_NWCTRL_LOOPBACK_LOCAL_MASK) | FXMAC_NWCTRL_MDEN_MASK;
    write_reg((base + FXMAC_NWCTRL_OFFSET) as *mut u32, netctrl);

    FXmacConfigureCaps(instance_p);

    // mdio clock division
    let mut w_reg: u32 = FXmacClkDivGet(instance_p);
    // DMA bus width, DMA位宽为128
    w_reg |= FXmacDmaWidth(instance_p);
    write_reg((base + FXMAC_NWCFG_OFFSET) as *mut u32, w_reg);

    FXmacDmaReset(instance_p);

    // This register, when read provides details of the status of the receive path.
    write_reg((base + FXMAC_RXSR_OFFSET) as *mut u32, FXMAC_SR_ALL_MASK);

    // write 1 ro the relavant bit location disable that particular interrupt
    write_reg((base + FXMAC_IDR_OFFSET) as *mut u32, FXMAC_IXR_ALL_MASK);

    let reg_val: u32 = read_reg((base + FXMAC_ISR_OFFSET) as *const u32);
    write_reg((base + FXMAC_ISR_OFFSET) as *mut u32, reg_val);

    write_reg((base + FXMAC_TXSR_OFFSET) as *mut u32, FXMAC_SR_ALL_MASK);

    FXmacClearHash(instance_p);

    // set default mac address
    for i in 0..4 {
        FXmacSetMacAddress(instance_p, &mac_addr, i);
        FXmacGetMacAddress(instance_p, &mut mac_addr, i);
        FXmacSetTypeIdCheck(instance_p, 0, i);
    }

    // clear all counters
    for i in 0..((FXMAC_LAST_OFFSET - FXMAC_OCTTXL_OFFSET) / 4) {
        read_reg((base + FXMAC_OCTTXL_OFFSET + (i * 4)) as *mut u32);
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
pub fn FXmacSetQueuePtr(instance_p: &mut FXmac, queue_p: u64, queue_num: u8, direction: u32) {
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
    let pclk_hz = instance_p.config.pclk_hz;

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
    let FXMAC_IOBASE = instance_p.config.base_address;
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
        reg_netcfg = read_reg((FXMAC_IOBASE + FXMAC_NWCFG_OFFSET) as *const u32);

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
            write_reg((FXMAC_IOBASE + FXMAC_TAIL_ENABLE) as *mut u32, 0x80000001);
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
                (FXMAC_IOBASE + FXMAC_JUMBOMAXLEN_OFFSET) as *mut u32,
                FXMAC_MAX_FRAME_SIZE_JUMBO,
            );

            write_reg(
                (FXMAC_IOBASE + FXMAC_TXQSEGALLOC_QLOWER_OFFSET) as *mut u32,
                FXMAC_TXQSEGALLOC_QLOWER_JUMBO_MASK,
            );

            if queue_num == 0 {
                let mut rx_buf_size: u32 = 0;
                reg = read_reg((FXMAC_IOBASE + FXMAC_DMACR_OFFSET) as *const u32);

                reg &= !FXMAC_DMACR_RXBUF_MASK;

                rx_buf_size = instance_p.max_frame_size / FXMAC_RX_BUF_UNIT;
                rx_buf_size += if !instance_p.max_frame_size.is_multiple_of(FXMAC_RX_BUF_UNIT) {
                    1
                } else {
                    0
                };

                reg |= (rx_buf_size << FXMAC_DMACR_RXBUF_SHIFT) & FXMAC_DMACR_RXBUF_MASK;
                write_reg((FXMAC_IOBASE + FXMAC_DMACR_OFFSET) as *mut u32, reg);
            } else if queue_num < instance_p.config.max_queue_num {
                let mut rx_buf_size: u32 = 0;
                rx_buf_size = instance_p.max_frame_size / FXMAC_RX_BUF_UNIT;
                rx_buf_size += if !instance_p.max_frame_size.is_multiple_of(FXMAC_RX_BUF_UNIT) {
                    1
                } else {
                    0
                };

                write_reg(
                    (FXMAC_IOBASE + FXMAC_RXBUFQX_SIZE_OFFSET(queue_num as u64)) as *mut u32,
                    rx_buf_size & FXMAC_RXBUFQX_SIZE_MASK,
                );
            }
        }

        if (options & FXMAC_SGMII_ENABLE_OPTION) != 0 {
            reg_new_netcfg |= (FXMAC_NWCFG_SGMII_MODE_ENABLE_MASK | FXMAC_NWCFG_PCSSEL_MASK);
        }

        if (options & FXMAC_LOOPBACK_NO_MII_OPTION) != 0 {
            reg = read_reg((FXMAC_IOBASE + FXMAC_NWCTRL_OFFSET) as *const u32);
            reg |= FXMAC_NWCTRL_LOOPBACK_LOCAL_MASK;
            write_reg((FXMAC_IOBASE + FXMAC_NWCTRL_OFFSET) as *mut u32, reg);
        }

        if (options & FXMAC_LOOPBACK_USXGMII_OPTION) != 0 {
            write_reg((FXMAC_IOBASE + FXMAC_TEST_CONTROL_OFFSET) as *mut u32, 2);
        }

        // Officially change the NET_CONFIG registers if it needs to be
        // modified.
        if (reg_netcfg != reg_new_netcfg) {
            write_reg(
                (FXMAC_IOBASE + FXMAC_NWCFG_OFFSET) as *mut u32,
                reg_new_netcfg,
            );
        }

        // Enable TX checksum offload
        if (options & FXMAC_TX_CHKSUM_ENABLE_OPTION) != 0 {
            reg = read_reg((FXMAC_IOBASE + FXMAC_DMACR_OFFSET) as *const u32);
            reg |= FXMAC_DMACR_TCPCKSUM_MASK;
            write_reg((FXMAC_IOBASE + FXMAC_DMACR_OFFSET) as *mut u32, reg);
        }

        // Enable transmitter
        if (options & FXMAC_TRANSMITTER_ENABLE_OPTION) != 0 {
            reg = read_reg((FXMAC_IOBASE + FXMAC_NWCTRL_OFFSET) as *const u32);
            reg |= FXMAC_NWCTRL_TXEN_MASK;
            write_reg((FXMAC_IOBASE + FXMAC_NWCTRL_OFFSET) as *mut u32, reg);
        }

        // Enable receiver
        if (options & FXMAC_RECEIVER_ENABLE_OPTION) != 0 {
            reg = read_reg((FXMAC_IOBASE + FXMAC_NWCTRL_OFFSET) as *const u32);
            reg |= FXMAC_NWCTRL_RXEN_MASK;

            write_reg((FXMAC_IOBASE + FXMAC_NWCTRL_OFFSET) as *mut u32, reg);
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
    let FXMAC_IOBASE = instance_p.config.base_address;
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
        reg_net_cfg = read_reg((FXMAC_IOBASE + FXMAC_NWCFG_OFFSET) as *const u32);
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
            write_reg((FXMAC_IOBASE + FXMAC_TAIL_ENABLE) as *mut u32, 0);
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

            reg = read_reg((FXMAC_IOBASE + FXMAC_DMACR_OFFSET) as *const u32);

            reg &= !FXMAC_DMACR_RXBUF_MASK;

            if queue_num == 0 {
                let mut rx_buf_size: u32 = 0;

                reg = read_reg((FXMAC_IOBASE + FXMAC_DMACR_OFFSET) as *const u32);
                reg &= !FXMAC_DMACR_RXBUF_MASK;

                rx_buf_size = instance_p.max_frame_size / FXMAC_RX_BUF_UNIT;
                rx_buf_size += if !instance_p.max_frame_size.is_multiple_of(FXMAC_RX_BUF_UNIT) {
                    1
                } else {
                    0
                };

                reg |= (rx_buf_size << FXMAC_DMACR_RXBUF_SHIFT) & FXMAC_DMACR_RXBUF_MASK;

                write_reg((FXMAC_IOBASE + FXMAC_DMACR_OFFSET) as *mut u32, reg);
            } else if (queue_num < instance_p.config.max_queue_num) {
                let mut rx_buf_size: u32 = 0;
                rx_buf_size = instance_p.max_frame_size / FXMAC_RX_BUF_UNIT;
                rx_buf_size += if !instance_p.max_frame_size.is_multiple_of(FXMAC_RX_BUF_UNIT) {
                    1
                } else {
                    0
                };

                write_reg(
                    (FXMAC_IOBASE + FXMAC_RXBUFQX_SIZE_OFFSET(queue_num as u64)) as *mut u32,
                    rx_buf_size & FXMAC_RXBUFQX_SIZE_MASK,
                );
            }
        }

        if (options & FXMAC_SGMII_ENABLE_OPTION) != 0 {
            reg_new_net_cfg &= !(FXMAC_NWCFG_SGMII_MODE_ENABLE_MASK | FXMAC_NWCFG_PCSSEL_MASK);
        }

        if (options & FXMAC_LOOPBACK_NO_MII_OPTION) != 0 {
            reg = read_reg((FXMAC_IOBASE + FXMAC_NWCTRL_OFFSET) as *const u32);
            reg &= !FXMAC_NWCTRL_LOOPBACK_LOCAL_MASK;
            write_reg((FXMAC_IOBASE + FXMAC_NWCTRL_OFFSET) as *mut u32, reg);
        }

        if (options & FXMAC_LOOPBACK_USXGMII_OPTION) != 0 {
            write_reg(
                (FXMAC_IOBASE + FXMAC_TEST_CONTROL_OFFSET) as *mut u32,
                read_reg((FXMAC_IOBASE + FXMAC_TEST_CONTROL_OFFSET) as *const u32) & !2,
            );
        }

        // Officially change the NET_CONFIG registers if it needs to be
        // modified.
        if reg_net_cfg != reg_new_net_cfg {
            write_reg(
                (FXMAC_IOBASE + FXMAC_NWCFG_OFFSET) as *mut u32,
                reg_new_net_cfg,
            );
        }

        // Disable TX checksum offload
        if (options & FXMAC_TX_CHKSUM_ENABLE_OPTION) != 0 {
            reg = read_reg((FXMAC_IOBASE + FXMAC_DMACR_OFFSET) as *const u32);
            reg &= !FXMAC_DMACR_TCPCKSUM_MASK;
            write_reg((FXMAC_IOBASE + FXMAC_DMACR_OFFSET) as *mut u32, reg);
        }

        // Disable transmitter
        if (options & FXMAC_TRANSMITTER_ENABLE_OPTION) != 0 {
            reg = read_reg((FXMAC_IOBASE + FXMAC_NWCTRL_OFFSET) as *const u32);
            reg &= !FXMAC_NWCTRL_TXEN_MASK;
            write_reg((FXMAC_IOBASE + FXMAC_NWCTRL_OFFSET) as *mut u32, reg);
        }

        // Disable receiver
        if (options & FXMAC_RECEIVER_ENABLE_OPTION) != 0 {
            reg = read_reg((FXMAC_IOBASE + FXMAC_NWCTRL_OFFSET) as *const u32);
            reg &= !FXMAC_NWCTRL_RXEN_MASK;
            write_reg((FXMAC_IOBASE + FXMAC_NWCTRL_OFFSET) as *mut u32, reg);
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
fn FXmacClearHash(instance_p: &mut FXmac) {
    let FXMAC_IOBASE = instance_p.config.base_address;
    write_reg((FXMAC_IOBASE + FXMAC_HASHL_OFFSET) as *mut u32, 0);

    // write bits [63:32] in TOP
    write_reg((FXMAC_IOBASE + FXMAC_HASHH_OFFSET) as *mut u32, 0);
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
pub fn FXmacSetMacAddress(instance_p: &mut FXmac, address_ptr: &[u8; 6], index: u8) -> u32 {
    let FXMAC_IOBASE = instance_p.config.base_address;
    let mut mac_addr: u32 = 0;
    let aptr = address_ptr;
    let index_loc: u8 = index;
    let mut status: u32 = 0;
    assert!(
        (index_loc < FXMAC_MAX_MAC_ADDR as u8),
        "index of Mac Address exceed {}",
        FXMAC_MAX_MAC_ADDR
    );

    // Be sure device has been stopped
    if instance_p.is_started == FT_COMPONENT_IS_STARTED {
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
            (FXMAC_IOBASE + FXMAC_GEM_SA1B as u64 + (index_loc * 8) as u64) as *mut u32,
            mac_addr,
        );

        // There are reserved bits in TOP so don't affect them
        mac_addr =
            read_reg((FXMAC_IOBASE + FXMAC_GEM_SA1T as u64 + (index_loc * 8) as u64) as *const u32);
        mac_addr &= !FXMAC_GEM_SAB_MASK;

        // Set MAC bits [47:32] in TOP
        mac_addr |= aptr[4] as u32;
        mac_addr |= (aptr[5] as u32) << 8;

        write_reg(
            (FXMAC_IOBASE + FXMAC_GEM_SA1T as u64 + (index_loc * 8) as u64) as *mut u32,
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
    *address_ptr = read_mac_address(instance_p.config.base_address, index);
}

fn read_mac_address(base_address: u64, index: u8) -> [u8; 6] {
    assert!((index as u32) < FXMAC_MAX_MAC_ADDR);

    let mut address = [0; 6];
    let mut reg_value: u32 =
        read_reg((base_address + FXMAC_GEM_SA1B as u64 + (index as u64 * 8)) as *const u32);
    address[0] = reg_value as u8;
    address[1] = (reg_value >> 8) as u8;
    address[2] = (reg_value >> 16) as u8;
    address[3] = (reg_value >> 24) as u8;

    reg_value = read_reg((base_address + FXMAC_GEM_SA1T as u64 + (index as u64 * 8)) as *const u32);
    address[4] = reg_value as u8;
    address[5] = (reg_value >> 8) as u8;
    address
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
fn FXmacSetTypeIdCheck(instance_p: &mut FXmac, id_check: u32, index: u8) -> u32 {
    let FXMAC_IOBASE = instance_p.config.base_address;
    let mut status: u32 = 0;
    assert!(
        (index < FXMAC_MAX_TYPE_ID as u8),
        "index of Type ID exceed {}",
        FXMAC_MAX_TYPE_ID
    );

    // Be sure device has been stopped
    if instance_p.is_started == FT_COMPONENT_IS_STARTED {
        status = 9; //FXMAC_ERR_MAC_IS_PROCESSING
        error!("FXMAC is processing when calling FXmacSetTypeIdCheck function");
    } else {
        // Set the ID bits in MATCHx register
        write_reg(
            (FXMAC_IOBASE + FXMAC_MATCH1_OFFSET + (index * 4) as u64) as *mut u32,
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
    let FXMAC_IOBASE = instance_p.config.base_address;
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
    reg_value = read_reg((FXMAC_IOBASE + FXMAC_GEM_HSMAC as u64) as *const u32);
    reg_value &= !FXMAC_GEM_HSMACSPEED_MASK;
    reg_value |= (set_speed as u32) & FXMAC_GEM_HSMACSPEED_MASK;
    write_reg(
        (FXMAC_IOBASE + FXMAC_GEM_HSMAC as u64) as *mut u32,
        reg_value,
    );

    reg_value = read_reg((FXMAC_IOBASE + FXMAC_GEM_HSMAC as u64) as *const u32);

    info!("FXMAC_GEM_HSMAC is {:#x}", reg_value);
}

/// FXmacInitInterface
/// Initialize the MAC controller configuration based on the PHY interface type
fn FXmacInitInterface(instance_p: &mut FXmac) {
    let FXMAC_IOBASE = instance_p.config.base_address;
    let mut config: u32 = 0;
    let mut control: u32 = 0;

    info!(
        "FXmacInitInterface, PHY MODE:{:?}",
        instance_p.config.interface
    );

    if instance_p.config.interface == FXmacPhyInterface::FXMAC_PHY_INTERFACE_MODE_XGMII {
        config = read_reg((FXMAC_IOBASE + FXMAC_NWCFG_OFFSET) as *const u32);
        config &= !FXMAC_NWCFG_PCSSEL_MASK;
        write_reg((FXMAC_IOBASE + FXMAC_NWCFG_OFFSET) as *mut u32, config);

        control = read_reg((FXMAC_IOBASE + FXMAC_NWCTRL_OFFSET) as *const u32);
        control |= FXMAC_NWCTRL_ENABLE_HS_MAC_MASK; /* Use high speed MAC */
        write_reg((FXMAC_IOBASE + FXMAC_NWCTRL_OFFSET) as *mut u32, control);

        instance_p.config.duplex = 1;
    } else if (instance_p.config.interface == FXmacPhyInterface::FXMAC_PHY_INTERFACE_MODE_USXGMII)
        || (instance_p.config.interface == FXmacPhyInterface::FXMAC_PHY_INTERFACE_MODE_5GBASER)
    {
        info!("usx interface is {:?}", instance_p.config.interface);
        // network_config
        instance_p.config.duplex = 1;
        config = read_reg((FXMAC_IOBASE + FXMAC_NWCFG_OFFSET) as *const u32);
        config |= FXMAC_NWCFG_PCSSEL_MASK;
        config &= !FXMAC_NWCFG_100_MASK;
        config &= !FXMAC_NWCFG_SGMII_MODE_ENABLE_MASK;
        if (instance_p.config.duplex == 1) {
            info!("is duplex");
            config |= FXMAC_NWCFG_FDEN_MASK;
        }

        write_reg((FXMAC_IOBASE + FXMAC_NWCFG_OFFSET) as *mut u32, config);

        // network_control
        control = read_reg((FXMAC_IOBASE + FXMAC_NWCTRL_OFFSET) as *const u32);
        control |= FXMAC_NWCTRL_ENABLE_HS_MAC_MASK; /* Use high speed MAC */
        write_reg((FXMAC_IOBASE + FXMAC_NWCTRL_OFFSET) as *mut u32, control);

        // High speed PCS control register
        control = read_reg((FXMAC_IOBASE + FXMAC_GEM_USX_CONTROL_OFFSET) as *const u32);

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
            (FXMAC_IOBASE + FXMAC_GEM_USX_CONTROL_OFFSET) as *mut u32,
            control,
        );

        control = read_reg((FXMAC_IOBASE + FXMAC_GEM_USX_CONTROL_OFFSET) as *const u32);
        control &= !FXMAC_GEM_USX_RX_SYNC_RESET;
        control |= FXMAC_GEM_USX_TX_DATAPATH_EN;
        control |= FXMAC_GEM_USX_SIGNAL_OK;

        write_reg(
            (FXMAC_IOBASE + FXMAC_GEM_USX_CONTROL_OFFSET) as *mut u32,
            control,
        );
    } else if instance_p.config.interface == FXmacPhyInterface::FXMAC_PHY_INTERFACE_MODE_2500BASEX {
        // network_config
        instance_p.config.duplex = 1;
        config = read_reg((FXMAC_IOBASE + FXMAC_NWCFG_OFFSET) as *const u32);
        config |= FXMAC_NWCFG_PCSSEL_MASK | FXMAC_NWCFG_SGMII_MODE_ENABLE_MASK;
        config &= !FXMAC_NWCFG_100_MASK;

        if (instance_p.config.duplex == 1) {
            config |= FXMAC_NWCFG_FDEN_MASK;
        }
        write_reg((FXMAC_IOBASE + FXMAC_NWCFG_OFFSET) as *mut u32, config);

        // network_control
        control = read_reg((FXMAC_IOBASE + FXMAC_NWCTRL_OFFSET) as *const u32);
        control &= !FXMAC_NWCTRL_ENABLE_HS_MAC_MASK;
        control |= FXMAC_NWCTRL_TWO_PT_FIVE_GIG_MASK; /* Use high speed MAC */
        write_reg((FXMAC_IOBASE + FXMAC_NWCTRL_OFFSET) as *mut u32, control);

        // High speed PCS control register
        control = read_reg((FXMAC_IOBASE + FXMAC_GEM_USX_CONTROL_OFFSET) as *const u32);

        if (instance_p.config.speed == FXMAC_SPEED_25000) {
            control |= FXMAC_GEM_USX_HS_MAC_SPEED_2_5G;
        }

        control &= !(FXMAC_GEM_USX_TX_SCR_BYPASS | FXMAC_GEM_USX_RX_SCR_BYPASS);
        control |= FXMAC_GEM_USX_RX_SYNC_RESET;
        write_reg(
            (FXMAC_IOBASE + FXMAC_GEM_USX_CONTROL_OFFSET) as *mut u32,
            control,
        );

        control = read_reg((FXMAC_IOBASE + FXMAC_GEM_USX_CONTROL_OFFSET) as *const u32);
        control &= !FXMAC_GEM_USX_RX_SYNC_RESET;
        control |= FXMAC_GEM_USX_TX_DATAPATH_EN;
        control |= FXMAC_GEM_USX_SIGNAL_OK;

        write_reg(
            (FXMAC_IOBASE + FXMAC_GEM_USX_CONTROL_OFFSET) as *mut u32,
            control,
        );
    } else if instance_p.config.interface == FXmacPhyInterface::FXMAC_PHY_INTERFACE_MODE_SGMII {
        config = read_reg((FXMAC_IOBASE + FXMAC_NWCFG_OFFSET) as *const u32);
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

        write_reg((FXMAC_IOBASE + FXMAC_NWCFG_OFFSET) as *mut u32, config);

        if instance_p.config.speed == FXMAC_SPEED_2500 {
            control = read_reg((FXMAC_IOBASE + FXMAC_NWCTRL_OFFSET) as *const u32);
            control |= FXMAC_NWCTRL_TWO_PT_FIVE_GIG_MASK;
            write_reg((FXMAC_IOBASE + FXMAC_NWCTRL_OFFSET) as *mut u32, control);
        } else {
            control = read_reg((FXMAC_IOBASE + FXMAC_NWCTRL_OFFSET) as *const u32);
            control &= !FXMAC_NWCTRL_TWO_PT_FIVE_GIG_MASK;
            write_reg((FXMAC_IOBASE + FXMAC_NWCTRL_OFFSET) as *mut u32, control);
        }

        control = read_reg((FXMAC_IOBASE + FXMAC_NWCTRL_OFFSET) as *const u32);
        control &= !FXMAC_NWCTRL_ENABLE_HS_MAC_MASK;
        write_reg((FXMAC_IOBASE + FXMAC_NWCTRL_OFFSET) as *mut u32, control);

        control = read_reg((FXMAC_IOBASE + FXMAC_PCS_CONTROL_OFFSET) as *const u32);
        control |= FXMAC_PCS_CONTROL_ENABLE_AUTO_NEG;
        write_reg(
            (FXMAC_IOBASE + FXMAC_PCS_CONTROL_OFFSET) as *mut u32,
            control,
        );
    } else {
        config = read_reg((FXMAC_IOBASE + FXMAC_NWCFG_OFFSET) as *const u32);

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

        write_reg((FXMAC_IOBASE + FXMAC_NWCFG_OFFSET) as *mut u32, config);

        control = read_reg((FXMAC_IOBASE + FXMAC_NWCTRL_OFFSET) as *const u32);
        control &= !FXMAC_NWCTRL_ENABLE_HS_MAC_MASK; /* Use high speed MAC */
        write_reg((FXMAC_IOBASE + FXMAC_NWCTRL_OFFSET) as *mut u32, control);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialization_requires_an_owned_mmio_mapping() {
        fn assert_constructor(
            _constructor: fn(DeviceDma, mmio_api::Mmio, FxmacHardwareConfig) -> FxmacInitResult,
        ) {
        }

        assert_constructor(xmac_init);
    }

    #[test]
    fn initialization_preserves_the_failing_hardware_stage() {
        assert!(matches!(
            init_status(FxmacInitStage::InitializePhy, 7),
            Err(FxmacInitError::Hardware {
                stage: FxmacInitStage::InitializePhy,
                status: 7,
            })
        ));
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
}
