use alloc::{
    boxed::Box,
    format,
    string::{String, ToString},
    sync::Arc,
};
use core::{
    mem::size_of,
    ptr::{NonNull, addr_of_mut},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use ax_kspin::SpinNoIrq;
use log::{debug, info, warn};
use rd_net::{
    ContainmentCause, DmaBuffer, EthernetIrqFault, Event, IRxQueue, ITxQueue, Interface,
    InterfaceIrqEndpoint, IrqCapture, MaskedSource, NetError, OwnerInitInput, OwnerInitPoll,
    OwnerInitSchedule, QueueConfig, QueueMemoryMode,
};
use rdrive::{
    probe::OnProbeError,
    register::{FdtInfo, ProbeFdt},
};

use crate::{BindingInfo, DriverGeneric, binding_info_from_fdt, net::PlatformDeviceNet};

const DEVICE_NAME: &str = "ls2k1000-gmac0";
const DEFAULT_MAC_ADDRESS: [u8; 6] = [0x62, 0x19, 0x1a, 0x02, 0xa8, 0x91];
const GMAC0_PADDR: usize = 0x4004_0000;

const MAC_BASE_OFFSET: usize = 0x0000;
const DMA_BASE_OFFSET: usize = 0x1000;
const DEFAULT_MMIO_SIZE: usize = 0x8000;

const GMAC_CONFIG: usize = 0x0000;
const GMAC_FRAME_FILTER: usize = 0x0004;
const GMAC_GMII_ADDR: usize = 0x0010;
const GMAC_GMII_DATA: usize = 0x0014;
const GMAC_FLOW_CONTROL: usize = 0x0018;
const GMAC_VERSION: usize = 0x0020;
const GMAC_INTERRUPT_STATUS: usize = 0x0038;
const GMAC_ADDR0_HIGH: usize = 0x0040;
const GMAC_ADDR0_LOW: usize = 0x0044;
const GMAC_RGSMII_STATUS: usize = 0x00d8;
const GMAC_MMC_INTR_MASK_RX: usize = 0x010c;
const GMAC_MMC_INTR_MASK_TX: usize = 0x0110;
const GMAC_MMC_RX_IPC_INTR_MASK: usize = 0x0200;

const DMA_BUS_MODE: usize = 0x0000;
const DMA_TX_POLL_DEMAND: usize = 0x0004;
const DMA_RX_POLL_DEMAND: usize = 0x0008;
const DMA_RX_BASE_ADDR: usize = 0x000c;
const DMA_TX_BASE_ADDR: usize = 0x0010;
const DMA_STATUS: usize = 0x0014;
const DMA_CONTROL: usize = 0x0018;
const DMA_INTERRUPT: usize = 0x001c;
const DMA_AXI_BUS_MODE: usize = 0x0028;
const GMAC_REQUIRED_MMIO_SIZE: usize = DMA_BASE_OFFSET + DMA_AXI_BUS_MODE + size_of::<u32>();

const GMII_BUSY: u32 = 1 << 0;
const GMII_CSR_CLK4: u32 = 1 << 4;
const GMII_REG_SHIFT: u32 = 6;
const GMII_REG_MASK: u32 = 0x1f << GMII_REG_SHIFT;
const GMII_DEV_SHIFT: u32 = 11;
const GMII_DEV_MASK: u32 = 0x1f << GMII_DEV_SHIFT;

const PHY_ADDR: u32 = 0;
const PHY_ID1: u32 = 2;
const PHY_ID2: u32 = 3;

const MAC_RX: u32 = 0x0000_0004;
const MAC_TX: u32 = 0x0000_0008;
const MAC_DEFERRAL_CHECK: u32 = 0x0000_0010;
const MAC_BACKOFF_LIMIT: u32 = 0x0000_0060;
const MAC_PAD_CRC_STRIP: u32 = 0x0000_0080;
const MAC_RETRY: u32 = 0x0000_0200;
const MAC_DUPLEX: u32 = 0x0000_0800;
const MAC_LOOPBACK: u32 = 0x0000_1000;
const MAC_RX_OWN: u32 = 0x0000_2000;
const MAC_SPEED_100: u32 = 0x0000_4000;
const MAC_PORT_SELECT: u32 = 0x0000_8000;
const MAC_JUMBO_FRAME: u32 = 0x0010_0000;
const MAC_FRAME_BURST: u32 = 0x0020_0000;
const MAC_JABBER: u32 = 0x0040_0000;
const MAC_WATCHDOG: u32 = 0x0080_0000;
const MAC_TX_CONFIG: u32 = 0x0100_0000;

const MAC_PROMISCUOUS_MODE: u32 = 0x0000_0001;
const MAC_UCAST_HASH_FILTER: u32 = 0x0000_0002;
const MAC_MCAST_HASH_FILTER: u32 = 0x0000_0004;
const MAC_DEST_ADDR_FILTER: u32 = 0x0000_0008;
const MAC_MULTICAST_FILTER: u32 = 0x0000_0010;
const MAC_BROADCAST: u32 = 0x0000_0020;
const MAC_PASS_CONTROL: u32 = 0x0000_00c0;
const MAC_SRC_ADDR_FILTER: u32 = 0x0000_0200;
const MAC_FILTER: u32 = 0x8000_0000;

const MAC_TX_FLOW_CONTROL: u32 = 0x0000_0002;
const MAC_RX_FLOW_CONTROL: u32 = 0x0000_0004;
const MAC_PAUSE_TIME_MASK: u32 = 0xffff_0000;

const MAC_LINK_MODE: u32 = 0x0000_0001;
const MAC_LINK_SPEED_25: u32 = 0x0000_0002;
const MAC_LINK_SPEED_125: u32 = 0x0000_0004;
const MAC_LINK_SPEED_MASK: u32 = 0x0000_0006;
const MAC_LINK_STATUS: u32 = 0x0000_0008;
const MAC_RGMII_INT_STATUS: u32 = 0x0000_0001;

const DMA_RESET_ON: u32 = 0x0000_0001;
const DMA_BURST_LENGTH32: u32 = 0x0000_2000;
const DMA_BURST_LENGTHX8: u32 = 0x0100_0000;
const DMA_MIXED_BURST_ENABLE: u32 = 0x0400_0000;

const DMA_RX_START: u32 = 0x0000_0002;
const DMA_TX_SECOND_FRAME: u32 = 0x0000_0004;
const DMA_EN_HW_FLOW_CTRL: u32 = 0x0000_0100;
const DMA_RX_FLOW_CTRL_ACT: u32 = 0x0080_0600;
const DMA_RX_FLOW_CTRL_DEACT: u32 = 0x0040_1800;
const DMA_TX_START: u32 = 0x0000_2000;
const DMA_STORE_AND_FORWARD: u32 = 0x0220_0000;

const DMA_INT_TX_COMPLETED: u32 = 0x0000_0001;
const DMA_INT_TX_STOPPED: u32 = 0x0000_0002;
const DMA_INT_TX_NO_BUFFER: u32 = 0x0000_0004;
const DMA_INT_RX_OVERFLOW: u32 = 0x0000_0010;
const DMA_INT_TX_UNDERFLOW: u32 = 0x0000_0020;
const DMA_INT_RX_COMPLETED: u32 = 0x0000_0040;
const DMA_INT_RX_NO_BUFFER: u32 = 0x0000_0080;
const DMA_INT_RX_STOPPED: u32 = 0x0000_0100;
const DMA_INT_BUS_ERROR: u32 = 0x0000_2000;
const DMA_INT_ABNORMAL: u32 = 0x0000_8000;
const DMA_INT_NORMAL: u32 = 0x0001_0000;
const GMAC_LINE_INTF_INTR: u32 = 0x0400_0000;
const GMAC_MMC_INTR: u32 = 0x0800_0000;
const GMAC_PMT_INTR: u32 = 0x1000_0000;
// DWMAC CSR5 documents only bits 0..=16 as write-one-to-clear. Process-state
// fields and MAC/MMC/PMT summary bits above this range are read-only facts.
const DMA_STATUS_W1C_MASK: u32 = 0x0001_ffff;
const DMA_STATUS_IRQ_CAUSE_MASK: u32 =
    DMA_STATUS_W1C_MASK | GMAC_LINE_INTF_INTR | GMAC_MMC_INTR | GMAC_PMT_INTR;
const GMAC_EVENT_SUBSTATUS_MASK: u64 = 0xffff;
const GMAC_EVENT_MAC_STATUS_SHIFT: u32 = 32;
const GMAC_EVENT_RGSMII_STATUS_SHIFT: u32 = 48;

const DESC_SIZE1_MASK: u32 = 0x0000_1fff;
const RX_DESC_END_OF_RING: u32 = 0x0000_8000;
const TX_DESC_END_OF_RING: u32 = 0x0020_0000;
const DESC_TX_FIRST: u32 = 0x1000_0000;
const DESC_TX_LAST: u32 = 0x2000_0000;
const DESC_TX_INT_ENABLE: u32 = 0x4000_0000;
const DESC_RX_LAST: u32 = 0x0000_0100;
const DESC_RX_FIRST: u32 = 0x0000_0200;
const DESC_ERROR: u32 = 0x0000_8000;
const DESC_FRAME_LENGTH_MASK: u32 = 0x3fff_0000;
const DESC_FRAME_LENGTH_SHIFT: u32 = 16;
const DESC_OWN_BY_DMA: u32 = 0x8000_0000;

const DMA_INT_DISABLE: u32 = 0;
const DMA_INT_ENABLE: u32 = DMA_INT_NORMAL
    | DMA_INT_ABNORMAL
    | DMA_INT_BUS_ERROR
    | DMA_INT_RX_STOPPED
    | DMA_INT_RX_NO_BUFFER
    | DMA_INT_RX_COMPLETED
    | DMA_INT_TX_UNDERFLOW
    | DMA_INT_RX_OVERFLOW
    | DMA_INT_TX_NO_BUFFER
    | DMA_INT_TX_STOPPED
    | DMA_INT_TX_COMPLETED;
const HW_DMA_MASK_32: u64 = u32::MAX as u64;
// The LS2K1000 board memory map used here stays below 4 GiB, while the TLSF
// allocator backend does not implement the special dma32 allocation path.
// Use the normal DMA allocation path and still reject addresses the GMAC cannot
// encode before writing descriptors.
const QUEUE_DMA_MASK: u64 = u64::MAX;
const INIT_POLL_INTERVAL_NS: u64 = 100_000;
const MDIO_TIMEOUT_NS: u64 = 10_000_000;
const DMA_RESET_TIMEOUT_NS: u64 = 100_000_000;
const QUEUE_ID0: usize = 0;
const RING_SIZE: usize = 128;
// rd-net keeps one descriptor free from its perspective. Start RX only after
// that first prefill has completed, matching the reference sequence where DMA
// is enabled after the RX ring is prepared.
const RX_START_THRESHOLD: usize = RING_SIZE - 1;
const BUFFER_SIZE: usize = 2048;
const BUFFER_ALIGN: usize = 64;
const EARLY_PACKET_LOG_COUNT: u64 = 4;
const PACKET_LOG_INTERVAL: u64 = 256;

crate::model_register!(
    name: "LS2K1000 GMAC",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["snps,dwmac-3.70a", "snps,arc-dwmac-3.70a"],
        on_probe: probe_fdt,
    }],
);

#[repr(C)]
#[derive(Clone, Copy)]
struct DmaDesc {
    status: u32,
    length: u32,
    buffer1: u32,
    buffer2: u32,
}

impl DmaDesc {
    const fn empty() -> Self {
        Self {
            status: 0,
            length: 0,
            buffer1: 0,
            buffer2: 0,
        }
    }

    fn owned_by_dma(self) -> bool {
        self.status & DESC_OWN_BY_DMA != 0
    }

    fn rx_valid(self) -> bool {
        self.status & DESC_ERROR == 0
            && self.status & DESC_RX_FIRST != 0
            && self.status & DESC_RX_LAST != 0
    }

    fn rx_length(self) -> usize {
        ((self.status & DESC_FRAME_LENGTH_MASK) >> DESC_FRAME_LENGTH_SHIFT) as usize
    }
}

#[repr(C, align(64))]
struct GmacRing {
    tx: [DmaDesc; RING_SIZE],
    rx: [DmaDesc; RING_SIZE],
}

impl GmacRing {
    const fn new() -> Self {
        Self {
            tx: [DmaDesc::empty(); RING_SIZE],
            rx: [DmaDesc::empty(); RING_SIZE],
        }
    }
}

struct Mmio {
    base: *mut u8,
}

impl Mmio {
    fn new(base: *mut u8) -> Self {
        Self { base }
    }

    fn read(&self, offset: usize) -> u32 {
        unsafe { self.base.add(offset).cast::<u32>().read_volatile() }
    }

    fn write(&self, offset: usize, value: u32) {
        unsafe {
            self.base.add(offset).cast::<u32>().write_volatile(value);
        }
    }

    fn set_bits(&self, offset: usize, bits: u32) {
        self.write(offset, self.read(offset) | bits);
    }

    fn clear_bits(&self, offset: usize, bits: u32) {
        self.write(offset, self.read(offset) & !bits);
    }

    fn into_raw(self) -> *mut u8 {
        self.base
    }
}

// SAFETY: ownership of an MMIO capability may move to the final CPU owner or
// IRQ action. It is deliberately not `Sync`; concurrent access is established
// only by the explicit runtime port split below.
unsafe impl Send for Mmio {}

struct GmacInitRegs {
    mac: Mmio,
    dma: Mmio,
}

impl GmacInitRegs {
    fn new(base: NonNull<u8>) -> Self {
        let base = base.as_ptr();
        Self {
            mac: Mmio::new(unsafe { base.add(MAC_BASE_OFFSET) }),
            dma: Mmio::new(unsafe { base.add(DMA_BASE_OFFSET) }),
        }
    }

    /// Consumes the initialization capability and creates the two runtime
    /// register owners. Only this transition may duplicate the raw region
    /// addresses, and each resulting type exposes a disjoint operation set.
    fn into_runtime_ports(self) -> (GmacOwnerInitRegs, GmacIrqPort) {
        let mac_base = self.mac.into_raw();
        let dma_base = self.dma.into_raw();
        (
            GmacOwnerInitRegs {
                mac: Mmio::new(mac_base),
                dma: Mmio::new(dma_base),
            },
            GmacIrqPort {
                mac: Mmio::new(mac_base),
                dma: Mmio::new(dma_base),
            },
        )
    }
}

struct GmacOwnerInitRegs {
    mac: Mmio,
    dma: Mmio,
}

impl GmacOwnerInitRegs {
    fn diagnostic_state(&self) -> (u32, u32) {
        (self.mac.read(GMAC_VERSION), self.mac.read(GMAC_CONFIG))
    }

    fn stop_tx_rx(&self) {
        self.dma
            .clear_bits(DMA_CONTROL, DMA_RX_START | DMA_TX_START);
        self.mac.clear_bits(GMAC_CONFIG, MAC_RX | MAC_TX);
        dma_barrier();
    }

    fn begin_dma_reset(&self) {
        self.dma.write(DMA_BUS_MODE, DMA_RESET_ON);
    }

    fn dma_reset_pending(&self) -> bool {
        self.dma.read(DMA_BUS_MODE) & DMA_RESET_ON != 0
    }

    fn mdio_busy(&self) -> bool {
        self.mac.read(GMAC_GMII_ADDR) & GMII_BUSY != 0
    }

    fn begin_mdio_read(&self, reg: u32) {
        let addr = ((PHY_ADDR << GMII_DEV_SHIFT) & GMII_DEV_MASK)
            | ((reg << GMII_REG_SHIFT) & GMII_REG_MASK)
            | GMII_CSR_CLK4
            | GMII_BUSY;
        self.mac.write(GMAC_GMII_ADDR, addr);
    }

    fn mdio_data(&self) -> u16 {
        (self.mac.read(GMAC_GMII_DATA) & 0xffff) as u16
    }

    fn link_state(&self) -> LinkState {
        LinkState::from_rgsmii(self.mac.read(GMAC_RGSMII_STATUS))
    }

    fn set_mac_address(&self, mac: [u8; 6]) {
        let high = ((mac[5] as u32) << 8) | mac[4] as u32;
        let low = ((mac[3] as u32) << 24)
            | ((mac[2] as u32) << 16)
            | ((mac[1] as u32) << 8)
            | mac[0] as u32;
        self.mac.write(GMAC_ADDR0_HIGH, high);
        self.mac.write(GMAC_ADDR0_LOW, low);
    }

    fn init_dma_regs(&self, tx_base: u32, rx_base: u32) {
        self.dma.write(
            DMA_BUS_MODE,
            DMA_MIXED_BURST_ENABLE | DMA_BURST_LENGTHX8 | DMA_BURST_LENGTH32,
        );
        self.dma
            .write(DMA_CONTROL, DMA_STORE_AND_FORWARD | DMA_TX_SECOND_FRAME);
        self.dma.write(DMA_AXI_BUS_MODE, 0xff | (0x77 << 16));
        self.dma.write(DMA_TX_BASE_ADDR, tx_base);
        self.dma.write(DMA_RX_BASE_ADDR, rx_base);
    }

    fn init_mac_regs(&self) {
        self.mac.set_bits(GMAC_CONFIG, MAC_TX_CONFIG);
        self.mac.clear_bits(
            GMAC_CONFIG,
            MAC_WATCHDOG
                | MAC_JABBER
                | MAC_FRAME_BURST
                | MAC_JUMBO_FRAME
                | MAC_RX_OWN
                | MAC_LOOPBACK
                | MAC_RETRY
                | MAC_PAD_CRC_STRIP
                | MAC_DEFERRAL_CHECK
                | MAC_BACKOFF_LIMIT,
        );
        self.mac.set_bits(GMAC_CONFIG, MAC_DUPLEX);

        self.mac.clear_bits(
            GMAC_FRAME_FILTER,
            MAC_SRC_ADDR_FILTER
                | MAC_BROADCAST
                | MAC_MULTICAST_FILTER
                | MAC_DEST_ADDR_FILTER
                | MAC_MCAST_HASH_FILTER
                | MAC_UCAST_HASH_FILTER
                | MAC_PROMISCUOUS_MODE
                | MAC_PASS_CONTROL,
        );
        self.mac.set_bits(GMAC_FRAME_FILTER, MAC_FILTER);

        let mut dma_ctrl = self.dma.read(DMA_CONTROL);
        dma_ctrl &= !(DMA_RX_FLOW_CTRL_ACT | DMA_RX_FLOW_CTRL_DEACT | DMA_EN_HW_FLOW_CTRL);
        self.dma.write(DMA_CONTROL, dma_ctrl);

        let mut flow_ctrl = MAC_PAUSE_TIME_MASK;
        flow_ctrl &= !(MAC_RX_FLOW_CONTROL | MAC_TX_FLOW_CONTROL);
        self.mac.write(GMAC_FLOW_CONTROL, flow_ctrl);
        self.mac.write(GMAC_MMC_INTR_MASK_TX, u32::MAX);
        self.mac.write(GMAC_MMC_INTR_MASK_RX, u32::MAX);
        self.mac.write(GMAC_MMC_RX_IPC_INTR_MASK, u32::MAX);
    }

    fn configure_link(&self, link: LinkState) {
        configure_link_registers(&self.mac, link);
    }

    fn disable_irq(&self) {
        disable_irq_register(&self.dma);
    }

    fn finish_initialization(self) -> GmacOwnerRegs {
        GmacOwnerRegs {
            mac: self.mac,
            dma: self.dma,
        }
    }
}

struct GmacOwnerRegs {
    mac: Mmio,
    dma: Mmio,
}

impl GmacOwnerRegs {
    fn configure_link(&self, link: LinkState) {
        configure_link_registers(&self.mac, link);
    }

    fn disable_irq(&self) {
        disable_irq_register(&self.dma);
    }

    fn enable_irq(&self) {
        self.dma.write(DMA_INTERRUPT, DMA_INT_ENABLE);
    }

    fn start_tx_rx(&self) {
        self.mac.set_bits(GMAC_CONFIG, MAC_RX | MAC_TX);
        self.dma.set_bits(DMA_CONTROL, DMA_RX_START | DMA_TX_START);
        dma_barrier();
        self.dma.write(DMA_RX_POLL_DEMAND, 0);
    }

    fn resume_tx(&self) {
        self.dma.write(DMA_TX_POLL_DEMAND, 0);
    }

    fn resume_rx(&self) {
        self.dma.write(DMA_RX_POLL_DEMAND, 0);
    }

    fn restart_rx(&self) {
        self.dma.set_bits(DMA_CONTROL, DMA_RX_START);
        self.resume_rx();
    }

    fn debug_state(&self) -> (u32, u32) {
        (self.mac.read(GMAC_CONFIG), self.dma.read(DMA_CONTROL))
    }
}

fn configure_link_registers(mac: &Mmio, link: LinkState) {
    let old_config = mac.read(GMAC_CONFIG);
    let mut config = old_config & !(MAC_PORT_SELECT | MAC_SPEED_100 | MAC_DUPLEX);

    if link.full_duplex {
        config |= MAC_DUPLEX;
    }
    match link.speed_mbps {
        1000 => {}
        100 => config |= MAC_PORT_SELECT | MAC_SPEED_100,
        _ => config |= MAC_PORT_SELECT,
    }

    if config != old_config {
        mac.write(GMAC_CONFIG, config);
        debug!(
            "{DEVICE_NAME}: MAC link config updated: speed={}Mbps, duplex={}, \
             config={old_config:#010x}->{config:#010x}",
            link.speed_mbps,
            if link.full_duplex { "full" } else { "half" },
        );
    }
}

fn disable_irq_register(dma: &Mmio) {
    dma.write(DMA_INTERRUPT, DMA_INT_DISABLE);
}

/// Destructive interrupt-register capability owned by the registered action.
///
/// The CPU-pinned maintenance owner accesses the remaining controller state
/// through [`GmacOwnerState`] while local IRQs are disabled. The hard handler
/// owns this port by value and therefore never takes the owner's task lock.
struct GmacIrqPort {
    mac: Mmio,
    dma: Mmio,
}

impl GmacIrqPort {
    fn capture_snapshot(&mut self) -> Option<GmacIrqSnapshot> {
        let dma_status = decode_dma_irq_status(self.dma.read(DMA_STATUS))?;

        let mac_status = if dma_status & GMAC_LINE_INTF_INTR != 0 {
            self.mac.read(GMAC_INTERRUPT_STATUS)
        } else {
            0
        };
        // Reading the RGMII child status is part of acknowledging the MAC line
        // source. Deferring it past controller EOI can leave a level source
        // asserted and repeatedly interrupt before the owner runs.
        let rgsmii_status = if mac_status & MAC_RGMII_INT_STATUS != 0 {
            self.mac.read(GMAC_RGSMII_STATUS)
        } else {
            0
        };
        let dma_ack = dma_status & DMA_STATUS_W1C_MASK;
        self.dma.write(DMA_STATUS, dma_ack);
        Some(GmacIrqSnapshot {
            dma_status,
            mac_status,
            rgsmii_status,
        })
    }

    fn disable_sources(&mut self) {
        self.dma.write(DMA_INTERRUPT, DMA_INT_DISABLE);
    }
}

fn decode_dma_irq_status(raw_status: u32) -> Option<u32> {
    if raw_status == u32::MAX {
        return None;
    }
    let causes = raw_status & DMA_STATUS_IRQ_CAUSE_MASK;
    (causes != 0).then_some(causes)
}

#[derive(Clone, Copy)]
struct GmacIrqSnapshot {
    dma_status: u32,
    mac_status: u32,
    rgsmii_status: u32,
}

impl GmacIrqSnapshot {
    fn into_event(self) -> Event {
        let mut event = Event::none();
        event.device_status = u64::from(self.dma_status)
            | ((u64::from(self.mac_status) & GMAC_EVENT_SUBSTATUS_MASK)
                << GMAC_EVENT_MAC_STATUS_SHIFT)
            | ((u64::from(self.rgsmii_status) & GMAC_EVENT_SUBSTATUS_MASK)
                << GMAC_EVENT_RGSMII_STATUS_SHIFT);
        if self.dma_status & (DMA_INT_TX_COMPLETED | DMA_INT_TX_NO_BUFFER | DMA_INT_TX_STOPPED) != 0
        {
            event.tx_queue.insert(QUEUE_ID0);
        }
        if self.dma_status & (DMA_INT_RX_COMPLETED | DMA_INT_RX_NO_BUFFER | DMA_INT_RX_OVERFLOW)
            != 0
        {
            event.rx_queue.insert(QUEUE_ID0);
        }
        event
    }

    fn from_event(event: Event) -> Self {
        Self {
            dma_status: event.device_status as u32,
            mac_status: ((event.device_status >> GMAC_EVENT_MAC_STATUS_SHIFT)
                & GMAC_EVENT_SUBSTATUS_MASK) as u32,
            rgsmii_status: ((event.device_status >> GMAC_EVENT_RGSMII_STATUS_SHIFT)
                & GMAC_EVENT_SUBSTATUS_MASK) as u32,
        }
    }
}

struct GmacIrqEpoch {
    next_generation: AtomicU64,
    active_generation: AtomicU64,
}

impl GmacIrqEpoch {
    const fn new() -> Self {
        Self {
            next_generation: AtomicU64::new(1),
            active_generation: AtomicU64::new(0),
        }
    }

    fn begin_masked_source(&self) -> Result<MaskedSource, EthernetIrqFault> {
        let active = self.active_generation.load(Ordering::Acquire);
        if active != 0 {
            return gmac_masked_source(active);
        }

        let mut generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        if generation == 0 {
            generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        }
        match self.active_generation.compare_exchange(
            0,
            generation,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => gmac_masked_source(generation),
            Err(active) => gmac_masked_source(active),
        }
    }

    fn finish_masked_source(&self, source: MaskedSource) -> Result<(), NetError> {
        let generation = source.generation().get();
        if source.bitmap().get() != u64::from(DMA_INT_ENABLE)
            || self
                .active_generation
                .compare_exchange(generation, 0, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return Err(NetError::Other(Box::new(rd_net::KError::Unknown(
                "stale LS2K GMAC IRQ source",
            ))));
        }
        Ok(())
    }

    fn is_masked(&self) -> bool {
        self.active_generation.load(Ordering::Acquire) != 0
    }
}

fn gmac_masked_source(generation: u64) -> Result<MaskedSource, EthernetIrqFault> {
    MaskedSource::try_new(generation, u64::from(DMA_INT_ENABLE))
        .map_err(|_| EthernetIrqFault::Containment)
}

#[derive(Clone, Copy)]
struct LinkState {
    raw: u32,
    up: bool,
    speed_mbps: u32,
    full_duplex: bool,
}

impl LinkState {
    fn from_rgsmii(raw: u32) -> Self {
        let speed = match raw & MAC_LINK_SPEED_MASK {
            MAC_LINK_SPEED_125 => 1000,
            MAC_LINK_SPEED_25 => 100,
            _ => 10,
        };
        Self {
            raw,
            up: raw & MAC_LINK_STATUS != 0,
            speed_mbps: speed,
            full_duplex: raw & MAC_LINK_MODE != 0,
        }
    }
}

#[derive(Debug)]
enum GmacError {
    ResetTimeout,
    MmioTooSmall { size: usize, required: usize },
    AddressTooHigh { name: &'static str, addr: u64 },
    StorageClaimed,
    BusError { status: u32 },
}

impl core::fmt::Display for GmacError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::ResetTimeout => write!(f, "DMA reset timed out"),
            Self::MmioTooSmall { size, required } => {
                write!(
                    f,
                    "GMAC register mapping is too small: {size:#x} < {required:#x}"
                )
            }
            Self::AddressTooHigh { name, addr } => {
                write!(f, "{name} above 32-bit DMA window: {addr:#x}")
            }
            Self::StorageClaimed => write!(f, "GMAC DMA storage is already claimed"),
            Self::BusError { status } => {
                write!(f, "fatal GMAC DMA bus error: status={status:#010x}")
            }
        }
    }
}

impl core::error::Error for GmacError {}

struct GmacNet {
    owner: Arc<SpinNoIrq<GmacOwnerState>>,
    irq_port: Option<GmacIrqPort>,
    irq_epoch: Arc<GmacIrqEpoch>,
    mac_address: [u8; 6],
    irq_enabled: bool,
    tx_created: bool,
    rx_created: bool,
    _register_mapping: Arc<mmio_api::Mmio>,
}

impl GmacNet {
    fn new(
        register_mapping: Arc<mmio_api::Mmio>,
        resource_addr: usize,
        mac_address: [u8; 6],
    ) -> Result<Self, GmacError> {
        if register_mapping.size() < GMAC_REQUIRED_MMIO_SIZE {
            return Err(GmacError::MmioTooSmall {
                size: register_mapping.size(),
                required: GMAC_REQUIRED_MMIO_SIZE,
            });
        }
        let mmio = register_mapping.as_nonnull_ptr();
        let register_layout = GmacInitRegs::new(mmio);
        debug!(
            "{DEVICE_NAME}: discovered resource={:#x}, vaddr={:#x}",
            resource_addr,
            mmio.as_ptr() as usize,
        );

        let dma_lease = GmacDmaLease::claim()?;
        let rings = dma_lease.rings;
        let buffers = dma_lease.buffers;
        let tx_base = dma_addr32(rings.tx.cast::<u8>(), "tx descriptor ring")?;
        let rx_base = dma_addr32(rings.rx.cast::<u8>(), "rx descriptor ring")?;
        let tx_buf0 = dma_addr32(buffer_ptr(buffers.tx, 0), "tx buffer")?;
        let rx_buf0 = dma_addr32(buffer_ptr(buffers.rx, 0), "rx buffer")?;
        let _ = dma_addr32(buffer_ptr(buffers.tx, RING_SIZE - 1), "last tx buffer")?;
        let _ = dma_addr32(buffer_ptr(buffers.rx, RING_SIZE - 1), "last rx buffer")?;

        unsafe {
            rings.tx.write_bytes(0, RING_SIZE);
            rings.rx.write_bytes(0, RING_SIZE);
            buffers.tx.write_bytes(0, RING_SIZE * BUFFER_SIZE);
            buffers.rx.write_bytes(0, RING_SIZE * BUFFER_SIZE);
        }
        init_tx_ring(rings.tx);
        init_rx_ring(rings.rx);
        dma_barrier();

        let (owner_regs, irq_port) = register_layout.into_runtime_ports();
        let irq_epoch = Arc::new(GmacIrqEpoch::new());

        debug!(
            "{DEVICE_NAME}: reserved MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            mac_address[0],
            mac_address[1],
            mac_address[2],
            mac_address[3],
            mac_address[4],
            mac_address[5],
        );
        debug!(
            "{DEVICE_NAME}: tx_desc={tx_base:#x}, rx_desc={rx_base:#x}, tx_buf0={tx_buf0:#x}, \
             rx_buf0={rx_buf0:#x}"
        );

        Ok(Self {
            owner: Arc::new(SpinNoIrq::new(GmacOwnerState::new(
                owner_regs,
                dma_lease,
                tx_base,
                rx_base,
                Arc::clone(&register_mapping),
            ))),
            irq_port: Some(irq_port),
            irq_epoch,
            mac_address,
            irq_enabled: false,
            tx_created: false,
            rx_created: false,
            _register_mapping: register_mapping,
        })
    }

    fn enable_irq_source(&mut self) -> Result<(), NetError> {
        if self.irq_enabled {
            return Ok(());
        }

        // The generation check and device rearm form one owner-local critical
        // section. Otherwise an IRQ could contain the source after the check
        // and this path would immediately reopen it without its token.
        let owner = self.owner.lock();
        if self.irq_epoch.is_masked() {
            warn!("{DEVICE_NAME}: refusing IRQ enable while a contained source is active");
            return Err(gmac_net_error(
                "contained GMAC IRQ source cannot be enabled",
            ));
        }
        if !owner.is_ready() {
            warn!("{DEVICE_NAME}: refusing IRQ enable before owner initialization");
            return Err(gmac_net_error("LS2K GMAC owner is not initialized"));
        }
        owner.ready_regs().enable_irq();
        self.irq_enabled = true;
        Ok(())
    }
}

impl DriverGeneric for GmacNet {
    fn name(&self) -> &str {
        DEVICE_NAME
    }
}

struct GmacIrqEndpoint {
    port: GmacIrqPort,
    epoch: Arc<GmacIrqEpoch>,
    _register_mapping: Arc<mmio_api::Mmio>,
}

impl InterfaceIrqEndpoint for GmacIrqEndpoint {
    type Event = Event;
    type Fault = EthernetIrqFault;

    fn capture(&mut self) -> IrqCapture<Event, EthernetIrqFault> {
        let Some(event) = capture_gmac_irq(&mut self.port) else {
            return IrqCapture::Unhandled;
        };
        IrqCapture::Captured {
            event,
            masked: None,
        }
    }

    fn contain(&mut self, _cause: ContainmentCause) -> Result<MaskedSource, EthernetIrqFault> {
        self.port.disable_sources();
        self.epoch.begin_masked_source()
    }
}

impl Interface for GmacNet {
    fn poll_owner_init(&mut self, input: OwnerInitInput) -> OwnerInitPoll {
        poll_gmac_owner_init(&mut self.owner.lock(), self.mac_address, input.now_ns)
    }

    fn mac_address(&self) -> [u8; 6] {
        self.mac_address
    }

    fn create_tx_queue(&mut self) -> Option<Box<dyn ITxQueue>> {
        if !self.owner.lock().is_ready() {
            warn!("{DEVICE_NAME}: refusing TX queue creation before owner initialization");
            return None;
        }
        if self.tx_created {
            warn!("{DEVICE_NAME}: tx queue was already created");
            return None;
        }
        self.tx_created = true;
        Some(Box::new(GmacTxQueue {
            owner: self.owner.clone(),
        }))
    }

    fn create_rx_queue(&mut self) -> Option<Box<dyn IRxQueue>> {
        if !self.owner.lock().is_ready() {
            warn!("{DEVICE_NAME}: refusing RX queue creation before owner initialization");
            return None;
        }
        if self.rx_created {
            warn!("{DEVICE_NAME}: rx queue was already created");
            return None;
        }
        self.rx_created = true;
        Some(Box::new(GmacRxQueue {
            owner: self.owner.clone(),
        }))
    }

    fn enable_irq(&mut self) -> Result<(), NetError> {
        self.enable_irq_source()
    }

    fn disable_irq(&mut self) -> Result<(), NetError> {
        let owner = self.owner.lock();
        match &owner.registers {
            GmacOwnerRegisterPort::Initializing(regs) => regs.disable_irq(),
            GmacOwnerRegisterPort::Runtime(regs) => regs.disable_irq(),
            GmacOwnerRegisterPort::Failed => {
                return Err(gmac_net_error("LS2K GMAC register state is failed"));
            }
        }
        self.irq_enabled = false;
        Ok(())
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled
    }

    fn take_irq_endpoint(&mut self) -> Option<rd_net::BIrqEndpoint> {
        let port = self.irq_port.take()?;
        Some(Box::new(GmacIrqEndpoint {
            port,
            epoch: Arc::clone(&self.irq_epoch),
            _register_mapping: Arc::clone(&self._register_mapping),
        }))
    }

    fn service_irq_event(&mut self, event: Event) -> Result<(), NetError> {
        let mut owner = self.owner.lock();
        if !owner.is_ready() {
            return Err(gmac_net_error("LS2K GMAC owner is not initialized"));
        }
        service_gmac_irq(&mut owner, event)
    }

    fn rearm_irq_source(&mut self, source: MaskedSource) -> Result<(), NetError> {
        let owner = self.owner.lock();
        if !owner.is_ready() {
            return Err(gmac_net_error("LS2K GMAC owner is not initialized"));
        }
        self.irq_epoch.finish_masked_source(source)?;
        owner.ready_regs().enable_irq();
        self.irq_enabled = true;
        Ok(())
    }
}

fn gmac_net_error(message: &'static str) -> NetError {
    NetError::Other(Box::new(rd_net::KError::Unknown(message)))
}

fn capture_gmac_irq(port: &mut GmacIrqPort) -> Option<Event> {
    port.capture_snapshot().map(GmacIrqSnapshot::into_event)
}

fn service_gmac_irq(owner: &mut GmacOwnerState, event: Event) -> Result<(), NetError> {
    let snapshot = GmacIrqSnapshot::from_event(event);
    if snapshot.dma_status & DMA_INT_BUS_ERROR != 0 {
        warn!(
            "{DEVICE_NAME}: fatal DMA bus error, status={:#010x}",
            snapshot.dma_status
        );
        return Err(NetError::Other(Box::new(GmacError::BusError {
            status: snapshot.dma_status,
        })));
    }
    if snapshot.mac_status & MAC_RGMII_INT_STATUS != 0 {
        let link = LinkState::from_rgsmii(snapshot.rgsmii_status);
        owner.link = link;
        log_link_state(link);
        owner.ready_regs().configure_link(link);
    }
    if snapshot.dma_status & DMA_INT_RX_STOPPED != 0 {
        warn!("{DEVICE_NAME}: RX process stopped, restarting");
        owner.ready_regs().restart_rx();
    }

    if snapshot.dma_status & (GMAC_MMC_INTR | GMAC_PMT_INTR) != 0 {
        debug!(
            "{DEVICE_NAME}: MAC side interrupt status={:#010x}",
            snapshot.dma_status
        );
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct RingPtrs {
    tx: *mut DmaDesc,
    rx: *mut DmaDesc,
}

// SAFETY: these pointers are minted only by the linear DMA lease and move with
// the CPU-pinned owner. Access remains serialized by `GmacOwnerState`.
unsafe impl Send for RingPtrs {}

#[derive(Clone, Copy)]
struct BufferPtrs {
    tx: *mut u8,
    rx: *mut u8,
}

// SAFETY: these pointers name the same statically reserved arena as the lease;
// moving the lease does not change their validity or create a second owner.
unsafe impl Send for BufferPtrs {}

/// Linear ownership proof for the single statically reserved GMAC DMA arena.
///
/// Before descriptor addresses are published to the controller, dropping the
/// lease releases the claim so a failed discovery may be retried. Once the
/// controller can retain those addresses, Drop deliberately keeps the global
/// claim set. A future teardown path may clear it only after explicit DMA
/// quiescence has been proven.
struct GmacDmaLease {
    rings: RingPtrs,
    buffers: BufferPtrs,
    device_owned: bool,
}

impl GmacDmaLease {
    fn claim() -> Result<Self, GmacError> {
        GMAC_DMA_CLAIMED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| GmacError::StorageClaimed)?;

        // SAFETY: the successful atomic claim proves this is the sole live
        // constructor allowed to derive pointers into the static DMA arena.
        let rings = unsafe {
            let ring = addr_of_mut!(GMAC_RING.0);
            RingPtrs {
                tx: addr_of_mut!((*ring).tx).cast::<DmaDesc>(),
                rx: addr_of_mut!((*ring).rx).cast::<DmaDesc>(),
            }
        };
        // SAFETY: the same claim covers the buffer storage for the complete
        // lease lifetime; no reference is created from these raw pointers.
        let buffers = unsafe {
            let buffers = addr_of_mut!(GMAC_BUFFERS);
            BufferPtrs {
                tx: addr_of_mut!((*buffers).tx).cast::<u8>(),
                rx: addr_of_mut!((*buffers).rx).cast::<u8>(),
            }
        };
        Ok(Self {
            rings,
            buffers,
            device_owned: false,
        })
    }

    fn mark_device_owned(&mut self) {
        self.device_owned = true;
    }
}

impl Drop for GmacDmaLease {
    fn drop(&mut self) {
        if !self.device_owned {
            let was_claimed = GMAC_DMA_CLAIMED.swap(false, Ordering::Release);
            debug_assert!(was_claimed, "dropping an unclaimed GMAC DMA lease");
        }
    }
}

#[derive(Clone, Copy)]
struct RuntimeNetBuffer {
    upper_bus_addr: u64,
    upper_virt: usize,
    len: usize,
}

#[derive(Clone, Copy)]
enum GmacOwnerInitState {
    Discovered,
    DmaResetPending { deadline_ns: u64 },
    MdioBeforeId1 { deadline_ns: u64 },
    MdioReadId1 { deadline_ns: u64 },
    MdioReadId2 { id1: u16, deadline_ns: u64 },
    Configure { phy_id: Option<u32> },
    Ready,
    Failed,
}

enum GmacOwnerRegisterPort {
    Initializing(GmacOwnerInitRegs),
    Runtime(GmacOwnerRegs),
    Failed,
}

struct GmacOwnerState {
    registers: GmacOwnerRegisterPort,
    link: LinkState,
    init_state: GmacOwnerInitState,
    tx_base: u32,
    rx_base: u32,
    dma_lease: GmacDmaLease,
    tx_bus_addrs: [Option<u64>; RING_SIZE],
    rx_buffers: [Option<RuntimeNetBuffer>; RING_SIZE],
    tx_next: usize,
    tx_busy: usize,
    rx_fill: usize,
    rx_busy: usize,
    rx_submitted: u64,
    rx_started: bool,
    tx_submitted: u64,
    tx_reclaimed: u64,
    rx_reclaimed: u64,
    rx_errors: u64,
    _register_mapping: Arc<mmio_api::Mmio>,
}

impl GmacOwnerState {
    fn new(
        regs: GmacOwnerInitRegs,
        dma_lease: GmacDmaLease,
        tx_base: u32,
        rx_base: u32,
        register_mapping: Arc<mmio_api::Mmio>,
    ) -> Self {
        Self {
            registers: GmacOwnerRegisterPort::Initializing(regs),
            link: LinkState::from_rgsmii(0),
            init_state: GmacOwnerInitState::Discovered,
            tx_base,
            rx_base,
            dma_lease,
            tx_bus_addrs: [None; RING_SIZE],
            rx_buffers: [None; RING_SIZE],
            tx_next: 0,
            tx_busy: 0,
            rx_fill: 0,
            rx_busy: 0,
            rx_submitted: 0,
            rx_started: false,
            tx_submitted: 0,
            tx_reclaimed: 0,
            rx_reclaimed: 0,
            rx_errors: 0,
            _register_mapping: register_mapping,
        }
    }

    fn is_ready(&self) -> bool {
        matches!(self.init_state, GmacOwnerInitState::Ready)
            && matches!(self.registers, GmacOwnerRegisterPort::Runtime(_))
    }

    fn initializing_regs(&self) -> &GmacOwnerInitRegs {
        match &self.registers {
            GmacOwnerRegisterPort::Initializing(regs) => regs,
            _ => panic!("GMAC owner init state lost its initialization register capability"),
        }
    }

    fn ready_regs(&self) -> &GmacOwnerRegs {
        match &self.registers {
            GmacOwnerRegisterPort::Runtime(regs) => regs,
            _ => panic!("GMAC ready state lost its runtime register capability"),
        }
    }

    fn finish_register_initialization(&mut self) -> Result<(), ()> {
        let registers = core::mem::replace(&mut self.registers, GmacOwnerRegisterPort::Failed);
        let GmacOwnerRegisterPort::Initializing(regs) = registers else {
            self.registers = registers;
            return Err(());
        };
        self.registers = GmacOwnerRegisterPort::Runtime(regs.finish_initialization());
        Ok(())
    }

    fn maybe_start_rx(&mut self) -> Option<u64> {
        if self.rx_started || self.rx_submitted < RX_START_THRESHOLD as u64 {
            return None;
        }
        self.rx_started = true;
        self.ready_regs().start_tx_rx();
        Some(self.rx_submitted)
    }
}

fn poll_gmac_owner_init(
    owner: &mut GmacOwnerState,
    mac_address: [u8; 6],
    now_ns: u64,
) -> OwnerInitPoll {
    match owner.init_state {
        GmacOwnerInitState::Discovered => {
            let regs = owner.initializing_regs();
            let (version, config) = regs.diagnostic_state();
            debug!("{DEVICE_NAME}: owner init version={version:#x}, config={config:#x}");
            regs.disable_irq();
            regs.stop_tx_rx();
            regs.begin_dma_reset();
            owner.init_state = GmacOwnerInitState::DmaResetPending {
                deadline_ns: now_ns.saturating_add(DMA_RESET_TIMEOUT_NS),
            };
            OwnerInitPoll::Pending(OwnerInitSchedule::run_again())
        }
        GmacOwnerInitState::DmaResetPending { deadline_ns } => {
            if !owner.initializing_regs().dma_reset_pending() {
                owner.init_state = GmacOwnerInitState::MdioBeforeId1 {
                    deadline_ns: now_ns.saturating_add(MDIO_TIMEOUT_NS),
                };
                return OwnerInitPoll::Pending(OwnerInitSchedule::run_again());
            }
            if now_ns >= deadline_ns {
                owner.init_state = GmacOwnerInitState::Failed;
                return OwnerInitPoll::Failed(NetError::Other(Box::new(GmacError::ResetTimeout)));
            }
            OwnerInitPoll::Pending(init_wait_schedule(now_ns, deadline_ns))
        }
        GmacOwnerInitState::MdioBeforeId1 { deadline_ns } => {
            let regs = owner.initializing_regs();
            if !regs.mdio_busy() {
                regs.begin_mdio_read(PHY_ID1);
                owner.init_state = GmacOwnerInitState::MdioReadId1 {
                    deadline_ns: now_ns.saturating_add(MDIO_TIMEOUT_NS),
                };
                return OwnerInitPoll::Pending(OwnerInitSchedule::run_again());
            }
            if now_ns >= deadline_ns {
                warn!("{DEVICE_NAME}: MDIO remained busy before PHY ID read");
                owner.init_state = GmacOwnerInitState::Configure { phy_id: None };
                return OwnerInitPoll::Pending(OwnerInitSchedule::run_again());
            }
            OwnerInitPoll::Pending(init_wait_schedule(now_ns, deadline_ns))
        }
        GmacOwnerInitState::MdioReadId1 { deadline_ns } => {
            let regs = owner.initializing_regs();
            if !regs.mdio_busy() {
                let id1 = regs.mdio_data();
                regs.begin_mdio_read(PHY_ID2);
                owner.init_state = GmacOwnerInitState::MdioReadId2 {
                    id1,
                    deadline_ns: now_ns.saturating_add(MDIO_TIMEOUT_NS),
                };
                return OwnerInitPoll::Pending(OwnerInitSchedule::run_again());
            }
            if now_ns >= deadline_ns {
                warn!("{DEVICE_NAME}: PHY ID1 read timed out");
                owner.init_state = GmacOwnerInitState::Configure { phy_id: None };
                return OwnerInitPoll::Pending(OwnerInitSchedule::run_again());
            }
            OwnerInitPoll::Pending(init_wait_schedule(now_ns, deadline_ns))
        }
        GmacOwnerInitState::MdioReadId2 { id1, deadline_ns } => {
            let regs = owner.initializing_regs();
            if !regs.mdio_busy() {
                let id2 = regs.mdio_data();
                owner.init_state = GmacOwnerInitState::Configure {
                    phy_id: Some((u32::from(id1) << 16) | u32::from(id2)),
                };
                return OwnerInitPoll::Pending(OwnerInitSchedule::run_again());
            }
            if now_ns >= deadline_ns {
                warn!("{DEVICE_NAME}: PHY ID2 read timed out");
                owner.init_state = GmacOwnerInitState::Configure { phy_id: None };
                return OwnerInitPoll::Pending(OwnerInitSchedule::run_again());
            }
            OwnerInitPoll::Pending(init_wait_schedule(now_ns, deadline_ns))
        }
        GmacOwnerInitState::Configure { phy_id } => {
            if let Some(phy_id) = phy_id {
                debug!("{DEVICE_NAME}: PHY addr={PHY_ADDR}, id={phy_id:#010x}");
            }
            let regs = owner.initializing_regs();
            let link = regs.link_state();
            log_link_state(link);
            regs.set_mac_address(mac_address);
            regs.init_dma_regs(owner.tx_base, owner.rx_base);
            regs.init_mac_regs();
            regs.configure_link(link);
            owner.dma_lease.mark_device_owned();
            if owner.finish_register_initialization().is_err() {
                owner.init_state = GmacOwnerInitState::Failed;
                return OwnerInitPoll::Failed(gmac_net_error(
                    "LS2K GMAC initialization register capability was lost",
                ));
            }
            owner.link = link;
            owner.init_state = GmacOwnerInitState::Ready;
            OwnerInitPoll::Ready
        }
        GmacOwnerInitState::Ready => OwnerInitPoll::Ready,
        GmacOwnerInitState::Failed => {
            OwnerInitPoll::Failed(NetError::Other(Box::new(GmacError::ResetTimeout)))
        }
    }
}

fn init_wait_schedule(now_ns: u64, deadline_ns: u64) -> OwnerInitSchedule {
    OwnerInitSchedule::wait_until(
        now_ns
            .saturating_add(INIT_POLL_INTERVAL_NS)
            .min(deadline_ns),
    )
}

struct GmacTxQueue {
    owner: Arc<SpinNoIrq<GmacOwnerState>>,
}

impl ITxQueue for GmacTxQueue {
    fn id(&self) -> usize {
        QUEUE_ID0
    }

    fn config(&self) -> QueueConfig {
        QueueConfig {
            dma_mask: QUEUE_DMA_MASK,
            align: BUFFER_ALIGN,
            buf_size: BUFFER_SIZE,
            ring_size: RING_SIZE,
            memory_mode: QueueMemoryMode::OwnerCopy,
        }
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), NetError> {
        if buffer.len == 0 || buffer.len > BUFFER_SIZE || buffer.len > DESC_SIZE1_MASK as usize {
            return Err(NetError::NotSupported);
        }

        let mut inner = self.owner.lock();
        if !inner.link.up {
            return Err(NetError::Retry);
        }

        let idx = inner.tx_next;
        if inner.tx_bus_addrs[idx].is_some() {
            return Err(NetError::Retry);
        }

        let desc = unsafe { inner.dma_lease.rings.tx.add(idx).read_volatile() };
        if desc.owned_by_dma() {
            return Err(NetError::Retry);
        }

        let ring_end = idx == RING_SIZE - 1;
        let tx_buf = buffer_ptr(inner.dma_lease.buffers.tx, idx);
        let tx_bus_addr = dma_addr32_net(tx_buf)?;
        unsafe {
            tx_buf.copy_from_nonoverlapping(buffer.virt.as_ptr(), buffer.len);
        }
        dma_barrier();

        let status = DESC_OWN_BY_DMA
            | DESC_TX_INT_ENABLE
            | DESC_TX_LAST
            | DESC_TX_FIRST
            | if ring_end { TX_DESC_END_OF_RING } else { 0 };
        let length = buffer.len as u32 & DESC_SIZE1_MASK;

        unsafe {
            write_desc_cpu_owned(
                inner.dma_lease.rings.tx.add(idx),
                status & !DESC_OWN_BY_DMA,
                length,
                tx_bus_addr,
            );
        }
        dma_barrier();
        unsafe {
            set_desc_status(inner.dma_lease.rings.tx.add(idx), status);
        }
        dma_barrier();

        inner.tx_bus_addrs[idx] = Some(buffer.bus_addr);
        inner.tx_next = ring_next(idx);
        inner.tx_submitted = inner.tx_submitted.saturating_add(1);
        inner.ready_regs().resume_tx();

        if inner.tx_submitted <= EARLY_PACKET_LOG_COUNT
            || inner.tx_submitted.is_multiple_of(PACKET_LOG_INTERVAL)
        {
            let (mac_config, dma_control) = inner.ready_regs().debug_state();
            debug!(
                "{DEVICE_NAME}: tx submit idx={idx}, len={}, submitted={}, reclaimed={}, \
                 mac_config={:#010x}, dma_control={:#010x}",
                buffer.len, inner.tx_submitted, inner.tx_reclaimed, mac_config, dma_control,
            );
        }

        Ok(())
    }

    fn reclaim(&mut self) -> Option<u64> {
        let mut inner = self.owner.lock();
        let idx = inner.tx_busy;
        let bus_addr = inner.tx_bus_addrs[idx]?;
        let desc = unsafe { inner.dma_lease.rings.tx.add(idx).read_volatile() };
        if desc.owned_by_dma() {
            return None;
        }

        let ring_end = idx == RING_SIZE - 1;
        unsafe {
            inner.dma_lease.rings.tx.add(idx).write_volatile(DmaDesc {
                status: if ring_end { TX_DESC_END_OF_RING } else { 0 },
                length: 0,
                buffer1: 0,
                buffer2: 0,
            });
        }
        inner.tx_bus_addrs[idx] = None;
        inner.tx_busy = ring_next(idx);
        inner.tx_reclaimed = inner.tx_reclaimed.saturating_add(1);

        if desc.status & DESC_ERROR != 0 {
            warn!(
                "{DEVICE_NAME}: tx descriptor error idx={idx}, status={:#010x}",
                desc.status,
            );
        }
        if inner.tx_reclaimed <= EARLY_PACKET_LOG_COUNT
            || inner.tx_reclaimed.is_multiple_of(PACKET_LOG_INTERVAL)
        {
            debug!(
                "{DEVICE_NAME}: tx reclaim idx={idx}, submitted={}, reclaimed={}",
                inner.tx_submitted, inner.tx_reclaimed,
            );
        }

        Some(bus_addr)
    }
}

struct GmacRxQueue {
    owner: Arc<SpinNoIrq<GmacOwnerState>>,
}

impl IRxQueue for GmacRxQueue {
    fn id(&self) -> usize {
        QUEUE_ID0
    }

    fn config(&self) -> QueueConfig {
        QueueConfig {
            dma_mask: QUEUE_DMA_MASK,
            align: BUFFER_ALIGN,
            buf_size: BUFFER_SIZE,
            ring_size: RING_SIZE,
            memory_mode: QueueMemoryMode::OwnerCopy,
        }
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), NetError> {
        if buffer.len < BUFFER_SIZE {
            warn!(
                "{DEVICE_NAME}: reject rx buffer len={}, required={BUFFER_SIZE}",
                buffer.len
            );
            return Err(NetError::NotSupported);
        }

        let mut inner = self.owner.lock();
        let idx = inner.rx_fill;
        if inner.rx_buffers[idx].is_some() {
            warn!("{DEVICE_NAME}: rx ring full at idx={idx}");
            return Err(NetError::Retry);
        }

        let desc = unsafe { inner.dma_lease.rings.rx.add(idx).read_volatile() };
        if desc.owned_by_dma() {
            warn!("{DEVICE_NAME}: rx desc {idx} is still owned by DMA");
            return Err(NetError::Retry);
        }

        let ring_end = idx == RING_SIZE - 1;
        let rx_buf = buffer_ptr(inner.dma_lease.buffers.rx, idx);
        let rx_bus_addr = dma_addr32_net(rx_buf).inspect_err(|err| {
            warn!("{DEVICE_NAME}: rx buffer {idx} is not usable by GMAC: {err:?}");
        })?;
        let length =
            (BUFFER_SIZE as u32 & DESC_SIZE1_MASK) | if ring_end { RX_DESC_END_OF_RING } else { 0 };
        unsafe {
            write_desc_cpu_owned(inner.dma_lease.rings.rx.add(idx), 0, length, rx_bus_addr);
        }
        dma_barrier();
        unsafe {
            set_desc_status(inner.dma_lease.rings.rx.add(idx), DESC_OWN_BY_DMA);
        }
        dma_barrier();

        inner.rx_buffers[idx] = Some(RuntimeNetBuffer {
            upper_bus_addr: buffer.bus_addr,
            upper_virt: buffer.virt.as_ptr() as usize,
            len: buffer.len.min(BUFFER_SIZE),
        });
        inner.rx_fill = ring_next(idx);
        inner.rx_submitted = inner.rx_submitted.saturating_add(1);
        let start_log = inner.maybe_start_rx();
        if inner.rx_started {
            inner.ready_regs().resume_rx();
        }
        drop(inner);

        if let Some(submitted) = start_log {
            debug!("{DEVICE_NAME}: DMA started after RX prefill: submitted={submitted}");
        }

        Ok(())
    }

    fn reclaim(&mut self) -> Option<(u64, usize)> {
        let mut inner = self.owner.lock();
        let idx = inner.rx_busy;
        let buffer = inner.rx_buffers[idx]?;
        let desc = unsafe { inner.dma_lease.rings.rx.add(idx).read_volatile() };
        if desc.owned_by_dma() {
            return None;
        }
        dma_barrier();

        let ring_end = idx == RING_SIZE - 1;
        unsafe {
            inner.dma_lease.rings.rx.add(idx).write_volatile(DmaDesc {
                status: 0,
                length: if ring_end { RX_DESC_END_OF_RING } else { 0 },
                buffer1: 0,
                buffer2: 0,
            });
        }
        inner.rx_buffers[idx] = None;
        inner.rx_busy = ring_next(idx);

        if !desc.rx_valid() {
            inner.rx_errors = inner.rx_errors.saturating_add(1);
            warn!(
                "{DEVICE_NAME}: rx descriptor error idx={idx}, status={:#010x}, length={:#010x}, \
                 errors={}",
                desc.status, desc.length, inner.rx_errors,
            );
            return Some((buffer.upper_bus_addr, 0));
        }

        let len = desc.rx_length().min(buffer.len);
        let rx_buf = buffer_ptr(inner.dma_lease.buffers.rx, idx);
        unsafe {
            (buffer.upper_virt as *mut u8).copy_from_nonoverlapping(rx_buf, len);
        }
        dma_barrier();
        inner.rx_reclaimed = inner.rx_reclaimed.saturating_add(1);
        if inner.rx_reclaimed <= EARLY_PACKET_LOG_COUNT
            || inner.rx_reclaimed.is_multiple_of(PACKET_LOG_INTERVAL)
        {
            debug!(
                "{DEVICE_NAME}: rx packet idx={idx}, len={len}, reclaimed={}",
                inner.rx_reclaimed,
            );
        }
        Some((buffer.upper_bus_addr, len))
    }
}

#[repr(C, align(64))]
struct AlignedGmacRing(GmacRing);

#[repr(C, align(64))]
struct GmacBuffers {
    tx: [[u8; BUFFER_SIZE]; RING_SIZE],
    rx: [[u8; BUFFER_SIZE]; RING_SIZE],
}

impl GmacBuffers {
    const fn new() -> Self {
        Self {
            tx: [[0; BUFFER_SIZE]; RING_SIZE],
            rx: [[0; BUFFER_SIZE]; RING_SIZE],
        }
    }
}

#[unsafe(link_section = ".dma_uncached")]
static mut GMAC_RING: AlignedGmacRing = AlignedGmacRing(GmacRing::new());

#[unsafe(link_section = ".dma_uncached")]
static mut GMAC_BUFFERS: GmacBuffers = GmacBuffers::new();

static GMAC_DMA_CLAIMED: AtomicBool = AtomicBool::new(false);

fn probe_fdt(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, plat_dev) = probe.into_parts();
    let reg = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other(format!("[{}] has no reg", info.node.name())))?;
    let resource_addr = reg.address as usize;
    let size = reg.size.unwrap_or(DEFAULT_MMIO_SIZE as u64) as usize;
    let register_mapping = Arc::new(axklib::mmio::ioremap(resource_addr.into(), size).map_err(
        |error| OnProbeError::other(format!("failed to map {DEVICE_NAME} registers: {error}")),
    )?);
    let resource_paddr =
        axklib::mem::virt_to_phys((register_mapping.as_nonnull_ptr().as_ptr() as usize).into())
            .as_usize();
    if resource_paddr != GMAC0_PADDR {
        warn!(
            "{DEVICE_NAME}: skip unsupported GMAC node {} at reg={resource_addr:#x}, \
             paddr={resource_paddr:#x}",
            info.node.name()
        );
        return Err(OnProbeError::NotMatch);
    }
    let vaddr = register_mapping.as_nonnull_ptr().as_ptr() as usize;
    let mac_address = mac_address_from_fdt(&info);
    let phy_mode = phy_mode_from_fdt(&info);
    let phy_mode = phy_mode.as_deref().unwrap_or("<unknown>");

    debug!(
        "probing {DEVICE_NAME}: node={}, reg={resource_addr:#x}, paddr={resource_paddr:#x}, \
         vaddr={vaddr:#x}, size={size:#x}, phy_mode={phy_mode}",
        info.node.name(),
    );

    let dev = GmacNet::new(register_mapping, resource_paddr, mac_address).map_err(|err| {
        OnProbeError::other(format!("failed to init {DEVICE_NAME} from FDT: {err}"))
    })?;
    let binding_info = gmac_binding_info(&info);
    let irq = binding_info.irq_num();
    plat_dev.register_net_with_info(DEVICE_NAME, dev, binding_info);
    info!(
        "registered {DEVICE_NAME} network device: irq={irq:?}, \
         mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac_address[0],
        mac_address[1],
        mac_address[2],
        mac_address[3],
        mac_address[4],
        mac_address[5],
    );
    Ok(())
}

fn gmac_binding_info(info: &FdtInfo<'_>) -> BindingInfo {
    binding_info_from_fdt(info).unwrap_or_else(|err| {
        warn!(
            "{DEVICE_NAME}: failed to resolve FDT IRQ for {}; continuing without IRQ: {err:?}",
            info.node.path(),
        );
        BindingInfo::empty()
    })
}

fn mac_address_from_fdt(info: &FdtInfo<'_>) -> [u8; 6] {
    for prop_name in ["local-mac-address", "mac-address"] {
        let Some(prop) = info.node.as_node().get_property(prop_name) else {
            continue;
        };
        if prop.data.len() < 6 {
            warn!(
                "{DEVICE_NAME}: ignore short {prop_name} from FDT: len={}",
                prop.data.len()
            );
            continue;
        }

        let mut mac = [0u8; 6];
        mac.copy_from_slice(&prop.data[..6]);
        if valid_unicast_mac(mac) {
            return mac;
        }

        warn!(
            "{DEVICE_NAME}: ignore invalid {prop_name} from FDT: \
             {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5],
        );
    }

    DEFAULT_MAC_ADDRESS
}

fn phy_mode_from_fdt(info: &FdtInfo<'_>) -> Option<String> {
    info.node
        .as_node()
        .get_property("phy-mode")
        .and_then(|prop| prop.as_str().map(ToString::to_string))
}

fn valid_unicast_mac(mac: [u8; 6]) -> bool {
    mac != [0; 6] && mac[0] & 1 == 0
}

fn buffer_ptr(base: *mut u8, index: usize) -> *mut u8 {
    unsafe { base.add(index * BUFFER_SIZE) }
}

fn init_tx_ring(tx: *mut DmaDesc) {
    for i in 0..RING_SIZE {
        let ring_end = i == RING_SIZE - 1;
        unsafe {
            tx.add(i).write_volatile(DmaDesc {
                status: if ring_end { TX_DESC_END_OF_RING } else { 0 },
                length: 0,
                buffer1: 0,
                buffer2: 0,
            });
        }
    }
}

fn init_rx_ring(rx: *mut DmaDesc) {
    for i in 0..RING_SIZE {
        let ring_end = i == RING_SIZE - 1;
        unsafe {
            rx.add(i).write_volatile(DmaDesc {
                status: 0,
                length: if ring_end { RX_DESC_END_OF_RING } else { 0 },
                buffer1: 0,
                buffer2: 0,
            });
        }
    }
}

unsafe fn write_desc_cpu_owned(desc: *mut DmaDesc, status: u32, length: u32, buffer1: u32) {
    unsafe {
        desc.write_volatile(DmaDesc {
            status,
            length,
            buffer1,
            buffer2: 0,
        });
    }
}

unsafe fn set_desc_status(desc: *mut DmaDesc, status: u32) {
    unsafe {
        let status_ptr = addr_of_mut!((*desc).status);
        status_ptr.write_volatile(status);
    }
}

fn dma_addr32(ptr: *const u8, name: &'static str) -> Result<u32, GmacError> {
    let paddr = dma_paddr(ptr);
    if paddr > HW_DMA_MASK_32 {
        return Err(GmacError::AddressTooHigh { name, addr: paddr });
    }
    Ok(paddr as u32)
}

fn dma_addr32_net(ptr: *const u8) -> Result<u32, NetError> {
    let paddr = dma_paddr(ptr);
    if paddr > HW_DMA_MASK_32 {
        return Err(NetError::NoMemory);
    }
    Ok(paddr as u32)
}

fn dma_paddr(ptr: *const u8) -> u64 {
    axklib::mem::virt_to_phys((ptr as usize).into()).as_usize() as u64
}

fn ring_next(index: usize) -> usize {
    (index + 1) % RING_SIZE
}

fn log_link_state(link: LinkState) {
    if link.up {
        debug!(
            "{DEVICE_NAME}: link up, speed={}Mbps, duplex={}, rgmii_status={:#x}",
            link.speed_mbps,
            if link.full_duplex { "full" } else { "half" },
            link.raw
        );
    } else {
        warn!("{DEVICE_NAME}: link down, rgmii_status={:#x}", link.raw);
    }
}

fn dma_barrier() {
    #[cfg(target_arch = "loongarch64")]
    unsafe {
        core::arch::asm!("dbar 0");
    }

    #[cfg(not(target_arch = "loongarch64"))]
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}

const _: () = {
    assert!(size_of::<DmaDesc>() == 16);
    assert!(size_of::<AlignedGmacRing>().is_multiple_of(BUFFER_ALIGN));
    assert!(size_of::<GmacBuffers>().is_multiple_of(BUFFER_ALIGN));
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_dma_lease_rejects_alias_and_releases_host_owned_failure() {
        let first = GmacDmaLease::claim().expect("first DMA arena claim must succeed");
        assert!(matches!(
            GmacDmaLease::claim(),
            Err(GmacError::StorageClaimed)
        ));
        drop(first);

        let retry = GmacDmaLease::claim().expect("host-owned failure must release its claim");
        drop(retry);
    }

    #[test]
    fn dma_process_state_does_not_claim_a_shared_irq() {
        assert_eq!(decode_dma_irq_status(1 << 20), None);
        assert_eq!(
            decode_dma_irq_status((1 << 20) | DMA_INT_RX_COMPLETED),
            Some(DMA_INT_RX_COMPLETED)
        );
        assert_eq!(decode_dma_irq_status(u32::MAX), None);
    }

    #[test]
    fn irq_snapshot_round_trip_preserves_queue_and_mac_facts() {
        let snapshot = GmacIrqSnapshot {
            dma_status: DMA_INT_TX_COMPLETED
                | DMA_INT_RX_COMPLETED
                | DMA_INT_BUS_ERROR
                | GMAC_LINE_INTF_INTR,
            mac_status: MAC_RGMII_INT_STATUS,
            rgsmii_status: MAC_LINK_STATUS | MAC_LINK_SPEED_125 | MAC_LINK_MODE,
        };

        let event = snapshot.into_event();
        assert!(event.tx_queue.contains(QUEUE_ID0));
        assert!(event.rx_queue.contains(QUEUE_ID0));

        let restored = GmacIrqSnapshot::from_event(event);
        assert_eq!(restored.dma_status, snapshot.dma_status);
        assert_eq!(restored.mac_status, snapshot.mac_status);
        assert_eq!(restored.rgsmii_status, snapshot.rgsmii_status);
    }

    #[test]
    fn irq_snapshot_does_not_report_unrelated_queues() {
        let event = GmacIrqSnapshot {
            dma_status: DMA_INT_BUS_ERROR,
            mac_status: 0,
            rgsmii_status: 0,
        }
        .into_event();

        assert!(!event.tx_queue.contains(QUEUE_ID0));
        assert!(!event.rx_queue.contains(QUEUE_ID0));
        assert_ne!(event.device_status, 0);
    }
}
