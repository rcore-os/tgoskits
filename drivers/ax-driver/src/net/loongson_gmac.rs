use alloc::{
    boxed::Box,
    format,
    string::{String, ToString},
    sync::Arc,
};
use core::{
    mem::size_of,
    ptr::{NonNull, addr_of_mut},
};

use ax_kspin::SpinNoIrq;
use log::{debug, info, warn};
use rd_net::{DmaBuffer, Event, IRxQueue, ITxQueue, Interface, NetError, QueueConfig};
use rdrive::{
    probe::OnProbeError,
    register::{FdtInfo, ProbeFdt},
};

#[cfg(target_arch = "loongarch64")]
use crate::mmio::loongarch_uncached_addr;
use crate::{
    BindingInfo, DriverGeneric, binding_info_from_fdt,
    mmio::{firmware_reg_paddr, firmware_reg_size, iomap_firmware_reg},
    net::PlatformDeviceNet,
};

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
const GMAC_INTERRUPT_MASK: usize = 0x003c;
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
const MDIO_TIMEOUT: usize = 100_000;
const DMA_RESET_TIMEOUT: usize = 1_000_000;
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

#[derive(Clone, Copy)]
struct Mmio {
    base: *mut u8,
}

impl Mmio {
    fn new(base: *mut u8) -> Self {
        Self { base }
    }

    fn read(self, offset: usize) -> u32 {
        unsafe { self.base.add(offset).cast::<u32>().read_volatile() }
    }

    fn write(self, offset: usize, value: u32) {
        unsafe {
            self.base.add(offset).cast::<u32>().write_volatile(value);
        }
    }

    fn set_bits(self, offset: usize, bits: u32) {
        self.write(offset, self.read(offset) | bits);
    }

    fn clear_bits(self, offset: usize, bits: u32) {
        self.write(offset, self.read(offset) & !bits);
    }
}

unsafe impl Send for Mmio {}
unsafe impl Sync for Mmio {}

#[derive(Clone, Copy)]
struct GmacRegs {
    mac: Mmio,
    dma: Mmio,
}

impl GmacRegs {
    fn new(base: NonNull<u8>) -> Self {
        let base = base.as_ptr();
        Self {
            mac: Mmio::new(unsafe { base.add(MAC_BASE_OFFSET) }),
            dma: Mmio::new(unsafe { base.add(DMA_BASE_OFFSET) }),
        }
    }

    fn wait_mdio_idle(self) -> bool {
        let mut timeout = MDIO_TIMEOUT;
        while timeout > 0 {
            if self.mac.read(GMAC_GMII_ADDR) & GMII_BUSY == 0 {
                return true;
            }
            timeout -= 1;
            core::hint::spin_loop();
        }
        false
    }

    fn mdio_read(self, phy: u32, reg: u32) -> Option<u16> {
        if !self.wait_mdio_idle() {
            return None;
        }

        let addr = ((phy << GMII_DEV_SHIFT) & GMII_DEV_MASK)
            | ((reg << GMII_REG_SHIFT) & GMII_REG_MASK)
            | GMII_CSR_CLK4
            | GMII_BUSY;
        self.mac.write(GMAC_GMII_ADDR, addr);

        self.wait_mdio_idle()
            .then(|| (self.mac.read(GMAC_GMII_DATA) & 0xffff) as u16)
    }

    fn phy_id(self) -> Option<u32> {
        let id1 = self.mdio_read(PHY_ADDR, PHY_ID1)? as u32;
        let id2 = self.mdio_read(PHY_ADDR, PHY_ID2)? as u32;
        Some((id1 << 16) | id2)
    }

    fn link_state(self) -> LinkState {
        LinkState::from_rgsmii(self.mac.read(GMAC_RGSMII_STATUS))
    }

    fn reset_dma(self) -> Result<(), GmacError> {
        self.dma.write(DMA_BUS_MODE, DMA_RESET_ON);
        for _ in 0..DMA_RESET_TIMEOUT {
            if self.dma.read(DMA_BUS_MODE) & DMA_RESET_ON == 0 {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(GmacError::DmaResetTimeout)
    }

    fn set_mac_address(self, mac: [u8; 6]) {
        let high = ((mac[5] as u32) << 8) | mac[4] as u32;
        let low = ((mac[3] as u32) << 24)
            | ((mac[2] as u32) << 16)
            | ((mac[1] as u32) << 8)
            | mac[0] as u32;
        self.mac.write(GMAC_ADDR0_HIGH, high);
        self.mac.write(GMAC_ADDR0_LOW, low);
    }

    fn init_dma_regs(self, tx_base: u32, rx_base: u32) {
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

    fn init_mac_regs(self) {
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
    }

    fn configure_link(self, link: LinkState) {
        let old_config = self.mac.read(GMAC_CONFIG);
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
            self.mac.write(GMAC_CONFIG, config);
            debug!(
                "{DEVICE_NAME}: MAC link config updated: speed={}Mbps, duplex={}, \
                 config={old_config:#010x}->{config:#010x}",
                link.speed_mbps,
                if link.full_duplex { "full" } else { "half" },
            );
        }
    }

    fn disable_irq(self) {
        self.dma.write(DMA_INTERRUPT, DMA_INT_DISABLE);
    }

    fn enable_irq(self) {
        self.dma.write(DMA_INTERRUPT, DMA_INT_ENABLE);
    }

    fn clear_pending_irq(self) {
        self.mac.write(GMAC_MMC_INTR_MASK_TX, u32::MAX);
        self.mac.write(GMAC_MMC_INTR_MASK_RX, u32::MAX);
        self.mac.write(GMAC_MMC_RX_IPC_INTR_MASK, u32::MAX);
        self.dma.write(DMA_STATUS, self.dma.read(DMA_STATUS));
    }

    fn start_tx_rx(self) {
        self.mac.set_bits(GMAC_CONFIG, MAC_RX | MAC_TX);
        self.dma.set_bits(DMA_CONTROL, DMA_RX_START | DMA_TX_START);
        dma_barrier();
        self.dma.write(DMA_RX_POLL_DEMAND, 0);
    }

    fn stop_tx_rx(self) {
        self.dma
            .clear_bits(DMA_CONTROL, DMA_RX_START | DMA_TX_START);
        self.mac.clear_bits(GMAC_CONFIG, MAC_RX | MAC_TX);
        dma_barrier();
    }

    fn resume_tx(self) {
        self.dma.write(DMA_TX_POLL_DEMAND, 0);
    }

    fn resume_rx(self) {
        self.dma.write(DMA_RX_POLL_DEMAND, 0);
    }
}

unsafe impl Send for GmacRegs {}
unsafe impl Sync for GmacRegs {}

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
    DmaResetTimeout,
    DmaAddressTooHigh { name: &'static str, addr: u64 },
}

impl core::fmt::Display for GmacError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::DmaResetTimeout => write!(f, "DMA reset timed out"),
            Self::DmaAddressTooHigh { name, addr } => {
                write!(f, "{name} above 32-bit DMA window: {addr:#x}")
            }
        }
    }
}

struct GmacNet {
    inner: Arc<SpinNoIrq<GmacState>>,
    mac_address: [u8; 6],
    irq_enabled: bool,
    tx_created: bool,
    rx_created: bool,
}

impl GmacNet {
    fn new(mmio: NonNull<u8>, paddr: usize, mac_address: [u8; 6]) -> Result<Self, GmacError> {
        let regs = GmacRegs::new(mmio);
        let version = regs.mac.read(GMAC_VERSION);
        let config = regs.mac.read(GMAC_CONFIG);

        debug!(
            "{DEVICE_NAME}: probe paddr={:#x}, vaddr={:#x}, version={:#x}, config={:#x}",
            paddr,
            mmio.as_ptr() as usize,
            version,
            config
        );

        match regs.phy_id() {
            Some(phy_id) => debug!("{DEVICE_NAME}: PHY addr={PHY_ADDR}, id={phy_id:#010x}"),
            None => warn!("{DEVICE_NAME}: failed to read PHY id via MDIO"),
        }

        let link = regs.link_state();
        log_link_state(link);

        regs.stop_tx_rx();
        regs.disable_irq();
        regs.reset_dma()?;
        regs.set_mac_address(mac_address);

        let rings = ring_ptrs();
        let buffers = buffer_ptrs();
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

        regs.init_dma_regs(tx_base, rx_base);
        regs.init_mac_regs();
        regs.configure_link(link);
        regs.clear_pending_irq();

        debug!(
            "{DEVICE_NAME}: registered MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
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
            inner: Arc::new(SpinNoIrq::new(GmacState::new(regs, rings, buffers))),
            mac_address,
            irq_enabled: false,
            tx_created: false,
            rx_created: false,
        })
    }

    fn enable_irq_source(&mut self) {
        if self.irq_enabled {
            return;
        }

        let regs = self.inner.lock().regs;
        regs.clear_pending_irq();
        regs.enable_irq();
        self.irq_enabled = true;
    }
}

impl DriverGeneric for GmacNet {
    fn name(&self) -> &str {
        DEVICE_NAME
    }
}

struct GmacIrqHandler {
    inner: Arc<SpinNoIrq<GmacState>>,
}

impl rd_net::InterfaceIrqHandler for GmacIrqHandler {
    fn handle_irq(&mut self) -> Event {
        let Some(mut inner) = self.inner.try_lock() else {
            return Event::none();
        };
        handle_gmac_irq(&mut inner)
    }
}

impl Interface for GmacNet {
    fn mac_address(&self) -> [u8; 6] {
        self.mac_address
    }

    fn create_tx_queue(&mut self) -> Option<Box<dyn ITxQueue>> {
        if self.tx_created {
            warn!("{DEVICE_NAME}: tx queue was already created");
            return None;
        }
        self.tx_created = true;
        Some(Box::new(GmacTxQueue {
            inner: self.inner.clone(),
        }))
    }

    fn create_rx_queue(&mut self) -> Option<Box<dyn IRxQueue>> {
        if self.rx_created {
            warn!("{DEVICE_NAME}: rx queue was already created");
            return None;
        }
        self.rx_created = true;
        Some(Box::new(GmacRxQueue {
            inner: self.inner.clone(),
        }))
    }

    fn enable_irq(&mut self) {
        self.enable_irq_source();
    }

    fn disable_irq(&mut self) {
        self.inner.lock().regs.disable_irq();
        self.irq_enabled = false;
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled
    }

    fn handle_irq(&mut self) -> Event {
        let mut inner = self.inner.lock();
        handle_gmac_irq(&mut inner)
    }

    fn take_irq_handler(&mut self) -> Option<rd_net::BIrqHandler> {
        Some(Box::new(GmacIrqHandler {
            inner: self.inner.clone(),
        }))
    }
}

fn handle_gmac_irq(inner: &mut GmacState) -> Event {
    let status = inner.regs.dma.read(DMA_STATUS);
    if status == 0 || status == u32::MAX {
        return Event::none();
    }

    inner.regs.dma.write(DMA_STATUS, status);
    if status & GMAC_LINE_INTF_INTR != 0 {
        let _ = inner.regs.mac.read(GMAC_INTERRUPT_STATUS);
        let _ = inner.regs.mac.read(GMAC_INTERRUPT_MASK);
        if inner.regs.mac.read(GMAC_INTERRUPT_STATUS) & MAC_RGMII_INT_STATUS != 0 {
            let link = inner.regs.link_state();
            log_link_state(link);
            inner.regs.configure_link(link);
        }
    }
    if status & DMA_INT_BUS_ERROR != 0 {
        warn!("{DEVICE_NAME}: fatal DMA bus error, status={status:#010x}");
    }
    if status & DMA_INT_RX_STOPPED != 0 {
        warn!("{DEVICE_NAME}: RX process stopped, restarting");
        inner.regs.dma.set_bits(DMA_CONTROL, DMA_RX_START);
        inner.regs.resume_rx();
    }

    let mut event = Event::none();
    if status & (DMA_INT_TX_COMPLETED | DMA_INT_TX_NO_BUFFER | DMA_INT_TX_STOPPED) != 0 {
        event.tx_queue.insert(QUEUE_ID0);
    }
    if status & (DMA_INT_RX_COMPLETED | DMA_INT_RX_NO_BUFFER | DMA_INT_RX_OVERFLOW) != 0 {
        event.rx_queue.insert(QUEUE_ID0);
    }
    if status & (GMAC_MMC_INTR | GMAC_PMT_INTR) != 0 {
        debug!("{DEVICE_NAME}: MAC side interrupt status={status:#010x}");
    }
    event
}

#[derive(Clone, Copy)]
struct RingPtrs {
    tx: *mut DmaDesc,
    rx: *mut DmaDesc,
}

unsafe impl Send for RingPtrs {}
unsafe impl Sync for RingPtrs {}

#[derive(Clone, Copy)]
struct BufferPtrs {
    tx: *mut u8,
    rx: *mut u8,
}

unsafe impl Send for BufferPtrs {}
unsafe impl Sync for BufferPtrs {}

#[derive(Clone, Copy)]
struct RuntimeNetBuffer {
    upper_bus_addr: u64,
    upper_virt: usize,
    len: usize,
}

struct GmacState {
    regs: GmacRegs,
    rings: RingPtrs,
    buffers: BufferPtrs,
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
}

impl GmacState {
    fn new(regs: GmacRegs, rings: RingPtrs, buffers: BufferPtrs) -> Self {
        Self {
            regs,
            rings,
            buffers,
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
        }
    }

    fn maybe_start_rx(&mut self) -> Option<(u64, u32)> {
        if self.rx_started || self.rx_submitted < RX_START_THRESHOLD as u64 {
            return None;
        }
        self.rx_started = true;
        self.regs.start_tx_rx();
        let status = self.regs.dma.read(DMA_STATUS);
        Some((self.rx_submitted, status))
    }
}

struct GmacTxQueue {
    inner: Arc<SpinNoIrq<GmacState>>,
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
        }
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), NetError> {
        if buffer.len == 0 || buffer.len > BUFFER_SIZE || buffer.len > DESC_SIZE1_MASK as usize {
            return Err(NetError::NotSupported);
        }

        let mut inner = self.inner.lock();
        if !inner.regs.link_state().up {
            return Err(NetError::Retry);
        }

        let idx = inner.tx_next;
        if inner.tx_bus_addrs[idx].is_some() {
            return Err(NetError::Retry);
        }

        let desc = unsafe { inner.rings.tx.add(idx).read_volatile() };
        if desc.owned_by_dma() {
            return Err(NetError::Retry);
        }

        let ring_end = idx == RING_SIZE - 1;
        let tx_buf = buffer_ptr(inner.buffers.tx, idx);
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
                inner.rings.tx.add(idx),
                status & !DESC_OWN_BY_DMA,
                length,
                tx_bus_addr,
            );
        }
        dma_barrier();
        unsafe {
            set_desc_status(inner.rings.tx.add(idx), status);
        }
        dma_barrier();

        inner.tx_bus_addrs[idx] = Some(buffer.bus_addr);
        inner.tx_next = ring_next(idx);
        inner.tx_submitted = inner.tx_submitted.saturating_add(1);
        inner.regs.resume_tx();

        if inner.tx_submitted <= EARLY_PACKET_LOG_COUNT
            || inner.tx_submitted.is_multiple_of(PACKET_LOG_INTERVAL)
        {
            debug!(
                "{DEVICE_NAME}: tx submit idx={idx}, len={}, submitted={}, reclaimed={}, \
                 dma_status={:#010x}, mac_config={:#010x}, dma_control={:#010x}",
                buffer.len,
                inner.tx_submitted,
                inner.tx_reclaimed,
                inner.regs.dma.read(DMA_STATUS),
                inner.regs.mac.read(GMAC_CONFIG),
                inner.regs.dma.read(DMA_CONTROL),
            );
        }

        Ok(())
    }

    fn reclaim(&mut self) -> Option<u64> {
        let mut inner = self.inner.lock();
        let idx = inner.tx_busy;
        let bus_addr = inner.tx_bus_addrs[idx]?;
        let desc = unsafe { inner.rings.tx.add(idx).read_volatile() };
        if desc.owned_by_dma() {
            return None;
        }

        let ring_end = idx == RING_SIZE - 1;
        unsafe {
            inner.rings.tx.add(idx).write_volatile(DmaDesc {
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
                "{DEVICE_NAME}: tx descriptor error idx={idx}, status={:#010x}, \
                 dma_status={:#010x}",
                desc.status,
                inner.regs.dma.read(DMA_STATUS),
            );
        }
        if inner.tx_reclaimed <= EARLY_PACKET_LOG_COUNT
            || inner.tx_reclaimed.is_multiple_of(PACKET_LOG_INTERVAL)
        {
            debug!(
                "{DEVICE_NAME}: tx reclaim idx={idx}, submitted={}, reclaimed={}, \
                 dma_status={:#010x}",
                inner.tx_submitted,
                inner.tx_reclaimed,
                inner.regs.dma.read(DMA_STATUS),
            );
        }

        Some(bus_addr)
    }
}

struct GmacRxQueue {
    inner: Arc<SpinNoIrq<GmacState>>,
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

        let mut inner = self.inner.lock();
        let idx = inner.rx_fill;
        if inner.rx_buffers[idx].is_some() {
            warn!("{DEVICE_NAME}: rx ring full at idx={idx}");
            return Err(NetError::Retry);
        }

        let desc = unsafe { inner.rings.rx.add(idx).read_volatile() };
        if desc.owned_by_dma() {
            warn!("{DEVICE_NAME}: rx desc {idx} is still owned by DMA");
            return Err(NetError::Retry);
        }

        let ring_end = idx == RING_SIZE - 1;
        let rx_buf = buffer_ptr(inner.buffers.rx, idx);
        let rx_bus_addr = dma_addr32_net(rx_buf).inspect_err(|err| {
            warn!("{DEVICE_NAME}: rx buffer {idx} is not usable by GMAC: {err:?}");
        })?;
        let length =
            (BUFFER_SIZE as u32 & DESC_SIZE1_MASK) | if ring_end { RX_DESC_END_OF_RING } else { 0 };
        unsafe {
            write_desc_cpu_owned(inner.rings.rx.add(idx), 0, length, rx_bus_addr);
        }
        dma_barrier();
        unsafe {
            set_desc_status(inner.rings.rx.add(idx), DESC_OWN_BY_DMA);
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
            inner.regs.resume_rx();
        }
        drop(inner);

        if let Some((submitted, status)) = start_log {
            debug!(
                "{DEVICE_NAME}: DMA started after RX prefill: submitted={submitted}, \
                 status={status:#010x}"
            );
        }

        Ok(())
    }

    fn reclaim(&mut self) -> Option<(u64, usize)> {
        let mut inner = self.inner.lock();
        let idx = inner.rx_busy;
        let buffer = inner.rx_buffers[idx]?;
        let desc = unsafe { inner.rings.rx.add(idx).read_volatile() };
        if desc.owned_by_dma() {
            return None;
        }
        dma_barrier();

        let ring_end = idx == RING_SIZE - 1;
        unsafe {
            inner.rings.rx.add(idx).write_volatile(DmaDesc {
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
                 errors={}, dma_status={:#010x}",
                desc.status,
                desc.length,
                inner.rx_errors,
                inner.regs.dma.read(DMA_STATUS),
            );
            return Some((buffer.upper_bus_addr, 0));
        }

        let len = desc.rx_length().min(buffer.len);
        let rx_buf = buffer_ptr(inner.buffers.rx, idx);
        unsafe {
            (buffer.upper_virt as *mut u8).copy_from_nonoverlapping(rx_buf, len);
        }
        dma_barrier();
        inner.rx_reclaimed = inner.rx_reclaimed.saturating_add(1);
        if inner.rx_reclaimed <= EARLY_PACKET_LOG_COUNT
            || inner.rx_reclaimed.is_multiple_of(PACKET_LOG_INTERVAL)
        {
            debug!(
                "{DEVICE_NAME}: rx packet idx={idx}, len={len}, reclaimed={}, dma_status={:#010x}",
                inner.rx_reclaimed,
                inner.regs.dma.read(DMA_STATUS),
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

fn probe_fdt(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, plat_dev) = probe.into_parts();
    let reg = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other(format!("[{}] has no reg", info.node.name())))?;
    let fw_addr = reg.address as usize;
    let paddr = firmware_reg_paddr(reg.address);
    if paddr != GMAC0_PADDR {
        warn!(
            "{DEVICE_NAME}: skip unsupported GMAC node {} at paddr={paddr:#x}",
            info.node.name()
        );
        return Err(OnProbeError::NotMatch);
    }
    let size = firmware_reg_size(reg.size, DEFAULT_MMIO_SIZE);
    let mmio = iomap_firmware_reg(DEVICE_NAME, reg.address, reg.size, DEFAULT_MMIO_SIZE)?;
    let vaddr = mmio.as_ptr() as usize;
    let mac_address = mac_address_from_fdt(&info);
    let phy_mode = phy_mode_from_fdt(&info);
    let phy_mode = phy_mode.as_deref().unwrap_or("<unknown>");

    debug!(
        "probing {DEVICE_NAME}: node={}, reg={fw_addr:#x}, paddr={paddr:#x}, vaddr={vaddr:#x}, \
         size={size:#x}, phy_mode={phy_mode}",
        info.node.name(),
    );

    let dev = GmacNet::new(mmio, paddr, mac_address).map_err(|err| {
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

fn ring_ptrs() -> RingPtrs {
    unsafe {
        let ring = addr_of_mut!(GMAC_RING.0);
        let tx_cached = addr_of_mut!((*ring).tx).cast::<DmaDesc>();
        let rx_cached = addr_of_mut!((*ring).rx).cast::<DmaDesc>();
        RingPtrs {
            tx: uncached_alias(tx_cached),
            rx: uncached_alias(rx_cached),
        }
    }
}

fn buffer_ptrs() -> BufferPtrs {
    unsafe {
        let buffers = addr_of_mut!(GMAC_BUFFERS);
        let tx_cached = addr_of_mut!((*buffers).tx).cast::<u8>();
        let rx_cached = addr_of_mut!((*buffers).rx).cast::<u8>();
        BufferPtrs {
            tx: uncached_alias(tx_cached),
            rx: uncached_alias(rx_cached),
        }
    }
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

#[cfg(target_arch = "loongarch64")]
fn uncached_alias<T>(ptr: *mut T) -> *mut T {
    let paddr = axklib::mem::virt_to_phys((ptr as usize).into()).as_usize();
    loongarch_uncached_addr(paddr) as *mut T
}

#[cfg(not(target_arch = "loongarch64"))]
fn uncached_alias<T>(ptr: *mut T) -> *mut T {
    ptr
}

fn dma_addr32(ptr: *const u8, name: &'static str) -> Result<u32, GmacError> {
    let paddr = dma_paddr(ptr);
    if paddr > HW_DMA_MASK_32 {
        return Err(GmacError::DmaAddressTooHigh { name, addr: paddr });
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
