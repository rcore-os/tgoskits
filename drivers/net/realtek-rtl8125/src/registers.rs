use core::{mem::size_of, ptr::NonNull};

use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields, register_structs,
    registers::ReadWrite,
};

pub const VENDOR_ID: u16 = 0x10ec;
pub const DEVICE_ID_RTL8125: u16 = 0x8125;
pub const RTL8125_REGS_SIZE: usize = EEE_TXIDLE_TIMER_8125 + size_of::<u16>();

const MAC0_BKP: usize = 0x19e0;
const EEE_TXIDLE_TIMER_8125: usize = 0x6048;

const CONFIG_WRITE_ENABLE_LOCKED: u8 = 0;
const CONFIG_WRITE_ENABLE_UNLOCKED: u8 = 0xc0;
const CPLUS_CMD_KEEP_MASK: u16 = CPLUS_CMD::NORMAL_MODE::SET.value
    | CPLUS_CMD::RX_VLAN::SET.value
    | CPLUS_CMD::RX_CHKSUM::SET.value
    | CPLUS_CMD::INTERRUPT_TIMER_MASK.val(0x3).value;
const RX_DMA_BURST_UNLIMITED: u32 = 7;
const RX_FETCH_DFLT_8125_VALUE: u32 = 8;
const TX_DMA_BURST_UNLIMITED: u32 = 7;
const TX_INTER_FRAME_GAP_VALUE: u32 = 3;

pub const DEFAULT_IRQ_MASK: u32 = INTR::LINK_CHANGE::SET.value
    | INTR::RX_OVERFLOW::SET.value
    | INTR::TX_ERROR::SET.value
    | INTR::TX_OK::SET.value
    | INTR::RX_ERROR::SET.value
    | INTR::RX_OK::SET.value;

pub const fn phy_status_link_up(value: u8) -> bool {
    value & PHY_STATUS::LINK_UP::SET.value != 0
}

pub fn irq_has_tx_event(value: u32) -> bool {
    value
        & (INTR::TX_OK::SET.value
            | INTR::TX_ERROR::SET.value
            | INTR::TX_DESC_UNAVAILABLE::SET.value)
        != 0
}

pub fn irq_has_rx_event(value: u32) -> bool {
    value
        & (INTR::RX_OK::SET.value
            | INTR::RX_ERROR::SET.value
            | INTR::RX_FIFO_OVERFLOW::SET.value
            | INTR::RX_OVERFLOW::SET.value)
        != 0
}

pub fn irq_has_link_change(value: u32) -> bool {
    value & INTR::LINK_CHANGE::SET.value != 0
}

register_structs! {
    Registers {
        (0x0000 => mac0: ReadWrite<u32>),
        (0x0004 => mac4: ReadWrite<u16>),
        (0x0006 => _reserved0),
        (0x0008 => mar0: ReadWrite<u32>),
        (0x000c => mar4: ReadWrite<u32>),
        (0x0010 => _reserved1),
        (0x0020 => tx_desc_start_addr_low: ReadWrite<u32>),
        (0x0024 => tx_desc_start_addr_high: ReadWrite<u32>),
        (0x0028 => _reserved2),
        (0x0037 => chip_cmd: ReadWrite<u8, CHIP_CMD::Register>),
        (0x0038 => intr_mask_8125: ReadWrite<u32, INTR::Register>),
        (0x003c => intr_status_8125: ReadWrite<u32, INTR::Register>),
        (0x0040 => tx_config: ReadWrite<u32, TX_CONFIG::Register>),
        (0x0044 => rx_config: ReadWrite<u32, RX_CONFIG::Register>),
        (0x0048 => _reserved3),
        (0x0050 => cfg9346: ReadWrite<u8>),
        (0x0051 => _reserved4),
        (0x0052 => config1: ReadWrite<u8, CONFIG1::Register>),
        (0x0053 => config2: ReadWrite<u8, CONFIG2::Register>),
        (0x0054 => config3: ReadWrite<u8, CONFIG3::Register>),
        (0x0055 => _reserved5),
        (0x0056 => config5: ReadWrite<u8, CONFIG5::Register>),
        (0x0057 => _reserved6),
        (0x0064 => csidr: ReadWrite<u32>),
        (0x0068 => csiar: ReadWrite<u32, CSIAR::Register>),
        (0x006c => phy_status: ReadWrite<u8, PHY_STATUS::Register>),
        (0x006d => _reserved7),
        (0x0080 => ephyar: ReadWrite<u32, EPHYAR::Register>),
        (0x0084 => _reserved8),
        (0x0090 => tx_poll_8125: ReadWrite<u16, TX_POLL::Register>),
        (0x0092 => _reserved9),
        (0x00b0 => ocpdr: ReadWrite<u32, OCPDR::Register>),
        (0x00b4 => _reserved10),
        (0x00b8 => gphy_ocp: ReadWrite<u32, OCPDR::Register>),
        (0x00bc => _reserved11),
        (0x00d3 => mcu: ReadWrite<u8, MCU::Register>),
        (0x00d4 => _reserved12),
        (0x00da => rx_max_size: ReadWrite<u16>),
        (0x00dc => _reserved13),
        (0x00e0 => cplus_cmd: ReadWrite<u16, CPLUS_CMD::Register>),
        (0x00e2 => intr_mitigate: ReadWrite<u16>),
        (0x00e4 => rx_desc_addr_low: ReadWrite<u32>),
        (0x00e8 => rx_desc_addr_high: ReadWrite<u32>),
        (0x00ec => _reserved14),
        (0x00f0 => misc: ReadWrite<u32, MISC::Register>),
        (0x00f4 => @END),
    }
}

register_bitfields! {u8,
    CHIP_CMD [
        RESET OFFSET(4) NUMBITS(1) [],
        RX_ENABLE OFFSET(3) NUMBITS(1) [],
        TX_ENABLE OFFSET(2) NUMBITS(1) []
    ],
    CONFIG1 [
        SPEED_DOWN OFFSET(4) NUMBITS(1) []
    ],
    CONFIG2 [
        CLK_REQ_ENABLE OFFSET(7) NUMBITS(1) []
    ],
    CONFIG3 [
        READY_TO_L23 OFFSET(1) NUMBITS(1) []
    ],
    CONFIG5 [
        ASPM_ENABLE OFFSET(0) NUMBITS(1) []
    ],
    PHY_STATUS [
        LINK_UP OFFSET(1) NUMBITS(1) []
    ],
    MCU [
        NOW_IS_OOB OFFSET(7) NUMBITS(1) [],
        TX_EMPTY OFFSET(5) NUMBITS(1) [],
        RX_EMPTY OFFSET(4) NUMBITS(1) [],
        LINK_LIST_READY OFFSET(1) NUMBITS(1) []
    ]
}

register_bitfields! {u16,
    CPLUS_CMD [
        NORMAL_MODE OFFSET(13) NUMBITS(1) [],
        RX_VLAN OFFSET(6) NUMBITS(1) [],
        RX_CHKSUM OFFSET(5) NUMBITS(1) [],
        PCI_DAC OFFSET(4) NUMBITS(1) [],
        PCI_MULTIPLE_RW OFFSET(3) NUMBITS(1) [],
        INTERRUPT_TIMER_MASK OFFSET(0) NUMBITS(2) []
    ],
    TX_POLL [
        NORMAL_PRIORITY OFFSET(0) NUMBITS(1) []
    ]
}

register_bitfields! {u32,
    CSIAR [
        FLAG OFFSET(31) NUMBITS(1) [],
        BYTE_ENABLE OFFSET(12) NUMBITS(4) [],
        FUNCTION OFFSET(16) NUMBITS(3) [],
        ADDR OFFSET(0) NUMBITS(12) []
    ],
    INTR [
        TX_DESC_UNAVAILABLE OFFSET(7) NUMBITS(1) [],
        RX_FIFO_OVERFLOW OFFSET(6) NUMBITS(1) [],
        LINK_CHANGE OFFSET(5) NUMBITS(1) [],
        RX_OVERFLOW OFFSET(4) NUMBITS(1) [],
        TX_ERROR OFFSET(3) NUMBITS(1) [],
        TX_OK OFFSET(2) NUMBITS(1) [],
        RX_ERROR OFFSET(1) NUMBITS(1) [],
        RX_OK OFFSET(0) NUMBITS(1) []
    ],
    TX_CONFIG [
        INTER_FRAME_GAP OFFSET(24) NUMBITS(2) [],
        DMA_BURST OFFSET(8) NUMBITS(3) []
    ],
    RX_CONFIG [
        FETCH_DFLT OFFSET(27) NUMBITS(4) [],
        PAUSE_SLOT_ON OFFSET(11) NUMBITS(1) [],
        DMA_BURST OFFSET(8) NUMBITS(3) [],
        ACCEPT_BROADCAST OFFSET(3) NUMBITS(1) [],
        ACCEPT_MULTICAST OFFSET(2) NUMBITS(1) [],
        ACCEPT_MY_PHYS OFFSET(1) NUMBITS(1) [],
        ACCEPT_ALL_PHYS OFFSET(0) NUMBITS(1) []
    ],
    EPHYAR [
        READY OFFSET(31) NUMBITS(1) [],
        REG OFFSET(16) NUMBITS(5) [],
        DATA OFFSET(0) NUMBITS(16) []
    ],
    OCPDR [
        BUSY OFFSET(31) NUMBITS(1) [],
        DATA OFFSET(0) NUMBITS(16) []
    ],
    MISC [
        RXDV_GATED_ENABLE OFFSET(19) NUMBITS(1) []
    ]
}

struct RegisterPort {
    base: NonNull<Registers>,
}

impl RegisterPort {
    const fn new(base: NonNull<u8>) -> Self {
        Self { base: base.cast() }
    }

    fn regs(&self) -> &Registers {
        unsafe {
            // SAFETY: discovery validated the complete BAR mapping and every
            // role retains the shared owning mapping lease.
            self.base.as_ref()
        }
    }

    fn raw_base(&self) -> *mut u8 {
        self.base.as_ptr().cast()
    }

    fn into_raw(self) -> NonNull<u8> {
        self.base.cast()
    }

    fn read8_at(&self, offset: usize) -> u8 {
        unsafe {
            // SAFETY: all offsets are bounded by `RTL8125_REGS_SIZE`.
            self.raw_base().add(offset).cast::<u8>().read_volatile()
        }
    }

    fn write8_at(&self, offset: usize, value: u8) {
        unsafe {
            // SAFETY: role APIs below partition access to validated offsets.
            self.raw_base()
                .add(offset)
                .cast::<u8>()
                .write_volatile(value);
        }
    }

    fn read16_at(&self, offset: usize) -> u16 {
        unsafe {
            // SAFETY: all offsets are bounded by `RTL8125_REGS_SIZE`.
            self.raw_base().add(offset).cast::<u16>().read_volatile()
        }
    }

    fn write16_at(&self, offset: usize, value: u16) {
        unsafe {
            // SAFETY: role APIs below partition access to validated offsets.
            self.raw_base()
                .add(offset)
                .cast::<u16>()
                .write_volatile(value);
        }
    }

    fn write32_at(&self, offset: usize, value: u32) {
        unsafe {
            // SAFETY: role APIs below partition access to validated offsets.
            self.raw_base()
                .add(offset)
                .cast::<u32>()
                .write_volatile(value);
        }
    }
}

// SAFETY: a move-only port may move to its final maintenance owner or IRQ
// action. It is deliberately not `Sync` and cannot be copied.
unsafe impl Send for RegisterPort {}

pub struct Rtl8125DiscoveryRegs {
    port: RegisterPort,
}

impl Rtl8125DiscoveryRegs {
    pub const fn new(base: NonNull<u8>) -> Self {
        Self {
            port: RegisterPort::new(base),
        }
    }

    /// The only raw-address duplication point. The resulting role types have
    /// disjoint public operation sets and are all move-only.
    pub fn split_for_irq(self) -> (Rtl8125OwnerInitRegs, Rtl8125IrqPort) {
        let base = self.port.into_raw();
        (
            Rtl8125OwnerInitRegs {
                port: RegisterPort::new(base),
            },
            Rtl8125IrqPort {
                port: RegisterPort::new(base),
            },
        )
    }
}

pub struct Rtl8125OwnerInitRegs {
    port: RegisterPort,
}

impl Rtl8125OwnerInitRegs {
    pub fn mask_interrupts(&self) {
        self.port.regs().intr_mask_8125.set(0);
    }

    pub fn request_reset(&self) {
        self.port.regs().chip_cmd.modify(CHIP_CMD::RESET::SET);
    }

    pub fn reset_pending(&self) -> bool {
        self.port.regs().chip_cmd.is_set(CHIP_CMD::RESET)
    }

    pub fn read_tx_config(&self) -> u32 {
        self.port.regs().tx_config.get()
    }

    pub fn read_mac(&self) -> [u8; 6] {
        let low = self.port.regs().mac0.get().to_le_bytes();
        let high = self.port.regs().mac4.get().to_le_bytes();
        [low[0], low[1], low[2], low[3], high[0], high[1]]
    }

    pub fn read_backup_mac(&self) -> [u8; 6] {
        [
            self.port.read8_at(MAC0_BKP),
            self.port.read8_at(MAC0_BKP + 1),
            self.port.read8_at(MAC0_BKP + 2),
            self.port.read8_at(MAC0_BKP + 3),
            self.port.read8_at(MAC0_BKP + 4),
            self.port.read8_at(MAC0_BKP + 5),
        ]
    }

    pub fn write_mac(&self, mac: [u8; 6]) {
        self.port.regs().cfg9346.set(CONFIG_WRITE_ENABLE_UNLOCKED);
        self.port
            .regs()
            .mac4
            .set(u16::from_le_bytes([mac[4], mac[5]]));
        self.commit();
        self.port
            .regs()
            .mac0
            .set(u32::from_le_bytes([mac[0], mac[1], mac[2], mac[3]]));
        self.commit();
        self.port.regs().cfg9346.set(CONFIG_WRITE_ENABLE_LOCKED);
    }

    pub fn commit(&self) {
        let _ = self.port.regs().chip_cmd.get();
    }

    pub fn enable_rxdv_gate(&self) {
        self.port.regs().misc.modify(MISC::RXDV_GATED_ENABLE::SET);
    }

    pub fn disable_rxdv_gate(&self) {
        self.port.regs().misc.modify(MISC::RXDV_GATED_ENABLE::CLEAR);
    }

    pub fn disable_tx_rx(&self) {
        self.port
            .regs()
            .chip_cmd
            .modify(CHIP_CMD::TX_ENABLE::CLEAR + CHIP_CMD::RX_ENABLE::CLEAR);
    }

    pub fn rxtx_empty(&self) -> bool {
        self.port
            .regs()
            .mcu
            .matches_all(MCU::TX_EMPTY::SET + MCU::RX_EMPTY::SET)
    }

    pub fn clear_now_is_oob(&self) {
        self.port.regs().mcu.modify(MCU::NOW_IS_OOB::CLEAR);
    }

    pub fn link_list_ready(&self) -> bool {
        self.port.regs().mcu.is_set(MCU::LINK_LIST_READY)
    }

    pub fn clear_ready_to_l23(&self) {
        self.port
            .regs()
            .config3
            .modify(CONFIG3::READY_TO_L23::CLEAR);
    }

    pub fn clear_speed_down(&self) {
        self.port.regs().config1.modify(CONFIG1::SPEED_DOWN::CLEAR);
    }

    pub fn set_aspm_clkreq(&self, enable: bool) {
        if enable {
            self.port.regs().config5.modify(CONFIG5::ASPM_ENABLE::SET);
            self.port
                .regs()
                .config2
                .modify(CONFIG2::CLK_REQ_ENABLE::SET);
        } else {
            self.port
                .regs()
                .config2
                .modify(CONFIG2::CLK_REQ_ENABLE::CLEAR);
            self.port.regs().config5.modify(CONFIG5::ASPM_ENABLE::CLEAR);
        }
    }

    pub fn start_csi_read(&self, addr: u32) {
        self.port.regs().csiar.write(
            CSIAR::ADDR.val(addr & 0x0fff) + CSIAR::FUNCTION.val(0) + CSIAR::BYTE_ENABLE.val(0x0f),
        );
    }

    pub fn csi_read_ready(&self) -> bool {
        self.port.regs().csiar.is_set(CSIAR::FLAG)
    }

    pub fn csi_read_data(&self) -> u32 {
        self.port.regs().csidr.get()
    }

    pub fn start_csi_write(&self, addr: u32, value: u32) {
        self.port.regs().csidr.set(value);
        self.port.regs().csiar.write(
            CSIAR::FLAG::SET
                + CSIAR::ADDR.val(addr & 0x0fff)
                + CSIAR::FUNCTION.val(0)
                + CSIAR::BYTE_ENABLE.val(0x0f),
        );
    }

    pub fn csi_write_ready(&self) -> bool {
        !self.port.regs().csiar.is_set(CSIAR::FLAG)
    }

    pub fn start_mac_ocp_read(&self, reg: u32) {
        self.port.regs().ocpdr.set(reg << 15);
    }

    pub fn start_mac_ocp_write(&self, reg: u32, data: u16) {
        self.port
            .regs()
            .ocpdr
            .set(OCPDR::BUSY::SET.value | (reg << 15) | u32::from(data));
    }

    pub fn read_mac_ocp_data(&self) -> u16 {
        self.port.regs().ocpdr.read(OCPDR::DATA) as u16
    }

    pub fn start_ephy_read(&self, reg: u32) {
        self.port.regs().ephyar.write(EPHYAR::REG.val(reg & 0x1f));
    }

    pub fn start_ephy_write(&self, reg: u32, data: u16) {
        self.port.regs().ephyar.write(
            EPHYAR::READY::SET + EPHYAR::REG.val(reg & 0x1f) + EPHYAR::DATA.val(u32::from(data)),
        );
    }

    pub fn ephy_read_ready(&self) -> bool {
        self.port.regs().ephyar.is_set(EPHYAR::READY)
    }

    pub fn ephy_write_ready(&self) -> bool {
        !self.port.regs().ephyar.is_set(EPHYAR::READY)
    }

    pub fn read_ephy_data(&self) -> u16 {
        self.port.regs().ephyar.read(EPHYAR::DATA) as u16
    }

    pub fn start_phy_ocp_read(&self, reg: u32) {
        self.port.regs().gphy_ocp.set(reg << 15);
    }

    pub fn start_phy_ocp_write(&self, reg: u32, data: u16) {
        self.port
            .regs()
            .gphy_ocp
            .set(OCPDR::BUSY::SET.value | (reg << 15) | u32::from(data));
    }

    pub fn phy_ocp_read_ready(&self) -> bool {
        self.port.regs().gphy_ocp.is_set(OCPDR::BUSY)
    }

    pub fn phy_ocp_write_ready(&self) -> bool {
        !self.port.regs().gphy_ocp.is_set(OCPDR::BUSY)
    }

    pub fn read_phy_ocp_data(&self) -> u16 {
        self.port.regs().gphy_ocp.read(OCPDR::DATA) as u16
    }

    pub fn mac_ocp_write(&self, reg: u32, data: u16) {
        self.start_mac_ocp_write(reg, data);
    }

    pub fn mac_ocp_modify(&self, reg: u32, mask: u16, set: u16) {
        self.start_mac_ocp_read(reg);
        let value = (self.read_mac_ocp_data() & !mask) | set;
        self.start_mac_ocp_write(reg, value);
    }

    pub fn write_vendor_u8(&self, offset: usize, value: u8) {
        self.port.write8_at(offset, value);
    }

    pub fn write_vendor_u16(&self, offset: usize, value: u16) {
        self.port.write16_at(offset, value);
    }

    pub fn write_vendor_u32(&self, offset: usize, value: u32) {
        self.port.write32_at(offset, value);
    }

    pub fn clear_vendor_u16_bits(&self, offset: usize, mask: u16) {
        self.port
            .write16_at(offset, self.port.read16_at(offset) & !mask);
    }

    pub fn write_eee_txidle_timer(&self, value: u16) {
        self.port.write16_at(EEE_TXIDLE_TIMER_8125, value);
    }

    pub fn program_controller(&self, dma_mask: u64, tx_base: u64, rx_base: u64) {
        let mut cplus = (self.port.regs().cplus_cmd.get() & CPLUS_CMD_KEEP_MASK)
            | CPLUS_CMD::PCI_MULTIPLE_RW::SET.value;
        if dma_mask > u32::MAX as u64 {
            cplus |= CPLUS_CMD::PCI_DAC::SET.value;
        }
        self.port.regs().cplus_cmd.set(cplus);
        self.port.regs().rx_config.write(
            RX_CONFIG::FETCH_DFLT.val(RX_FETCH_DFLT_8125_VALUE)
                + RX_CONFIG::PAUSE_SLOT_ON::SET
                + RX_CONFIG::DMA_BURST.val(RX_DMA_BURST_UNLIMITED),
        );
        self.port.regs().tx_config.write(
            TX_CONFIG::DMA_BURST.val(TX_DMA_BURST_UNLIMITED)
                + TX_CONFIG::INTER_FRAME_GAP.val(TX_INTER_FRAME_GAP_VALUE),
        );
        self.port.regs().rx_max_size.set(2049);
        self.port.regs().intr_mitigate.set(0);
        self.port.regs().cfg9346.set(CONFIG_WRITE_ENABLE_UNLOCKED);
        self.port
            .regs()
            .tx_desc_start_addr_high
            .set((tx_base >> 32) as u32);
        self.port.regs().tx_desc_start_addr_low.set(tx_base as u32);
        self.port
            .regs()
            .rx_desc_addr_high
            .set((rx_base >> 32) as u32);
        self.port.regs().rx_desc_addr_low.set(rx_base as u32);
        self.port.regs().cfg9346.set(CONFIG_WRITE_ENABLE_LOCKED);
        self.commit();
    }

    pub fn into_runtime_ports(self) -> (Rtl8125OwnerRegs, Rtl8125TxRegs, Rtl8125RxRegs) {
        let base = self.port.into_raw();
        (
            Rtl8125OwnerRegs {
                port: RegisterPort::new(base),
            },
            Rtl8125TxRegs {
                port: RegisterPort::new(base),
            },
            Rtl8125RxRegs {
                port: RegisterPort::new(base),
            },
        )
    }
}

pub struct Rtl8125OwnerRegs {
    port: RegisterPort,
}

impl Rtl8125OwnerRegs {
    pub fn mask_interrupts(&self) {
        self.port.regs().intr_mask_8125.set(0);
    }

    pub fn enable_interrupts(&self) {
        self.port.regs().intr_mask_8125.set(DEFAULT_IRQ_MASK);
    }

    pub fn read_phy_status(&self) -> u8 {
        self.port.regs().phy_status.get()
    }

    pub fn read_chip_cmd(&self) -> u8 {
        self.port.regs().chip_cmd.get()
    }

    pub fn read_mcu(&self) -> u8 {
        self.port.regs().mcu.get()
    }

    pub fn read_rx_config(&self) -> u32 {
        self.port.regs().rx_config.get()
    }

    pub fn read_tx_config(&self) -> u32 {
        self.port.regs().tx_config.get()
    }

    pub fn read_cplus_cmd(&self) -> u16 {
        self.port.regs().cplus_cmd.get()
    }

    pub fn read_rx_desc_base(&self) -> u64 {
        (u64::from(self.port.regs().rx_desc_addr_high.get()) << 32)
            | u64::from(self.port.regs().rx_desc_addr_low.get())
    }
}

pub struct Rtl8125TxRegs {
    port: RegisterPort,
}

impl Rtl8125TxRegs {
    pub fn poll_tx(&self) {
        self.port
            .regs()
            .tx_poll_8125
            .write(TX_POLL::NORMAL_PRIORITY::SET);
    }

    pub fn link_up(&self) -> bool {
        self.port.regs().phy_status.is_set(PHY_STATUS::LINK_UP)
    }
}

pub struct Rtl8125RxRegs {
    port: RegisterPort,
}

impl Rtl8125RxRegs {
    pub fn start_queues(&self) {
        self.port.regs().mar4.set(u32::MAX);
        self.port.regs().mar0.set(u32::MAX);
        self.port.regs().rx_config.modify(
            RX_CONFIG::ACCEPT_ALL_PHYS::CLEAR
                + RX_CONFIG::ACCEPT_MY_PHYS::SET
                + RX_CONFIG::ACCEPT_MULTICAST::SET
                + RX_CONFIG::ACCEPT_BROADCAST::SET,
        );
        self.port
            .regs()
            .chip_cmd
            .write(CHIP_CMD::TX_ENABLE::SET + CHIP_CMD::RX_ENABLE::SET);
        let _ = self.port.regs().chip_cmd.get();
    }
}

pub struct Rtl8125IrqPort {
    port: RegisterPort,
}

impl Rtl8125IrqPort {
    pub fn read_interrupt_status(&self) -> u32 {
        self.port.regs().intr_status_8125.get()
    }

    pub fn write_interrupt_status(&self, bits: u32) {
        self.port.regs().intr_status_8125.set(bits);
    }

    pub fn capture_status(&self) -> Option<u32> {
        let status = self.read_interrupt_status();
        if status == 0 || status == u32::MAX {
            return None;
        }
        self.write_interrupt_status(status);
        Some(status)
    }

    pub fn mask_interrupts(&self) {
        self.port.regs().intr_mask_8125.set(0);
    }
}
