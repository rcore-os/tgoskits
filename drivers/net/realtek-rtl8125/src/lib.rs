#![no_std]

extern crate alloc;

use alloc::{boxed::Box, sync::Arc};
use core::{
    mem::size_of,
    sync::atomic::{AtomicU64, Ordering},
};

use descriptor::{RING_END, RxDesc, TxDesc};
use dma_api::{CoherentArray, DeviceDma, DmaOp};
use log::info;
use mmio_api::{Mmio, MmioAddr, MmioOp};
use queue::{Rtl8125RxQueue, Rtl8125TxQueue};
use rdif_eth::{
    ContainmentCause, EthernetIrqFault, Event, IRxQueue, ITxQueue, Interface, IrqCapture,
    MaskedSource, NetError, OwnerInitInput, OwnerInitPoll,
};
use registers::*;

use crate::hw::{Rtl8125InitMachine, Rtl8125InitProgress};

mod descriptor;
mod hw;
mod queue;
mod registers;

const DRIVER_NAME: &str = "realtek-rtl8125";
const QUEUE_ID0: usize = 0;
const QUEUE_SIZE: usize = 256;
const RX_QUEUE_CONFIG_SIZE: usize = QUEUE_SIZE + 1;
const RX_START_THRESHOLD: usize = QUEUE_SIZE;
const MAX_PACKET: usize = 2048;
const RX_BUF_SIZE: usize = 2048;
const DMA_ALIGN: usize = 256;
const LINK_DOWN_DROP_LOG_INTERVAL: u64 = 64;
const EARLY_PACKET_LOG_COUNT: u64 = 8;
const TX_SUBMIT_LOG_INTERVAL: u64 = 16;
const TX_RECLAIM_LOG_INTERVAL: u64 = 64;
const RX_RECLAIM_LOG_INTERVAL: u64 = 64;
const TX_LINK_SAMPLE_INTERVAL: u64 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipVersion {
    Rtl8125A,
    Rtl8125B,
    Unknown(u16),
}

#[derive(Debug, Clone, Copy)]
pub struct Rtl8125Status {
    pub phy_status: u8,
    pub chip_cmd: u8,
    pub mcu: u8,
    pub rx_config: u32,
    pub tx_config: u32,
    pub cplus_cmd: u16,
    pub rx_desc_base: u64,
}

impl Rtl8125Status {
    pub const fn link_up(&self) -> bool {
        phy_status_link_up(self.phy_status)
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("unsupported PCI id {vendor:#06x}:{device:#06x}")]
    UnsupportedPciId { vendor: u16, device: u16 },
    #[error("MMIO map failed")]
    MmioMap(#[from] mmio_api::MapError),
    #[error("MMIO mapping is too small: {size:#x} < {required:#x}")]
    MmioTooSmall { size: usize, required: usize },
    #[error("DMA allocation failed")]
    Dma(#[from] dma_api::DmaError),
    #[error("invalid MAC address")]
    InvalidMacAddress,
    #[error("hardware reset timed out")]
    ResetTimeout,
    #[error("wait for {operation} timed out")]
    HardwareTimeout { operation: &'static str },
    #[error("invalid OCP register address {reg:#x}")]
    InvalidOcpAddress { reg: u32 },
    #[error("invalid RTL8125 lifecycle state: {0}")]
    InvalidState(&'static str),
}

pub type Result<T> = core::result::Result<T, Error>;

enum Rtl8125RegisterState {
    Initializing(Rtl8125OwnerInitRegs),
    Runtime(Rtl8125OwnerRegs),
    Failed,
}

pub struct Rtl8125 {
    registers: Rtl8125RegisterState,
    irq_port: Option<Rtl8125IrqPort>,
    irq_epoch: Arc<Rtl8125IrqEpoch>,
    init_machine: Rtl8125InitMachine,
    tx_regs: Option<Rtl8125TxRegs>,
    rx_regs: Option<Rtl8125RxRegs>,
    tx_desc: Option<CoherentArray<TxDesc>>,
    rx_desc: Option<CoherentArray<RxDesc>>,
    dma_mask: u64,
    mac: [u8; 6],
    chip: ChipVersion,
    irq_enabled: bool,
    _mapping: Arc<Mmio>,
}

impl Rtl8125 {
    pub fn check_vid_did(vendor: u16, device: u16) -> bool {
        vendor == VENDOR_ID && device == DEVICE_ID_RTL8125
    }

    /// Constructs a pending controller without reading or programming it.
    /// Hardware initialization starts only on the final maintenance owner.
    pub fn new(
        bar_addr: impl Into<MmioAddr>,
        bar_size: usize,
        dma_mask: u64,
        dma_op: &'static dyn DmaOp,
        mmio_op: &'static dyn MmioOp,
    ) -> Result<Self> {
        mmio_api::init(mmio_op);
        let mapping = Arc::new(mmio_api::ioremap(bar_addr.into(), bar_size)?);
        if mapping.size() < RTL8125_REGS_SIZE {
            return Err(Error::MmioTooSmall {
                size: mapping.size(),
                required: RTL8125_REGS_SIZE,
            });
        }

        let discovery = Rtl8125DiscoveryRegs::new(mapping.as_nonnull_ptr());
        let (owner_regs, irq_port) = discovery.split_for_irq();
        let dma = DeviceDma::new_legacy(dma_mask, dma_op);
        let mut tx_desc = dma.coherent_array_zero_with_align::<TxDesc>(QUEUE_SIZE, DMA_ALIGN)?;
        tx_desc.set_cpu(
            QUEUE_SIZE - 1,
            TxDesc {
                opts1: RING_END,
                opts2: 0,
                addr: 0,
            },
        );
        let rx_desc = dma.coherent_array_zero_with_align::<RxDesc>(QUEUE_SIZE, DMA_ALIGN)?;

        Ok(Self {
            registers: Rtl8125RegisterState::Initializing(owner_regs),
            irq_port: Some(irq_port),
            irq_epoch: Arc::new(Rtl8125IrqEpoch::new()),
            init_machine: Rtl8125InitMachine::new(),
            tx_regs: None,
            rx_regs: None,
            tx_desc: Some(tx_desc),
            rx_desc: Some(rx_desc),
            dma_mask,
            mac: [0; 6],
            chip: ChipVersion::Unknown(0),
            irq_enabled: false,
            _mapping: mapping,
        })
    }

    pub fn mac_address(&self) -> [u8; 6] {
        self.mac
    }

    pub fn chip_version(&self) -> ChipVersion {
        self.chip
    }

    pub fn poll_link(&self) -> bool {
        self.status().is_some_and(|status| status.link_up())
    }

    pub fn status(&self) -> Option<Rtl8125Status> {
        let regs = self.runtime_regs()?;
        Some(Rtl8125Status {
            phy_status: regs.read_phy_status(),
            chip_cmd: regs.read_chip_cmd(),
            mcu: regs.read_mcu(),
            rx_config: regs.read_rx_config(),
            tx_config: regs.read_tx_config(),
            cplus_cmd: regs.read_cplus_cmd(),
            rx_desc_base: regs.read_rx_desc_base(),
        })
    }

    fn runtime_regs(&self) -> Option<&Rtl8125OwnerRegs> {
        match &self.registers {
            Rtl8125RegisterState::Runtime(regs) => Some(regs),
            _ => None,
        }
    }

    fn finish_initialization(&mut self, mac: [u8; 6], chip: ChipVersion) -> Result<()> {
        let registers = core::mem::replace(&mut self.registers, Rtl8125RegisterState::Failed);
        let Rtl8125RegisterState::Initializing(registers) = registers else {
            return Err(Error::InvalidState(
                "owner initialization transition repeated",
            ));
        };
        let (owner, tx, rx) = registers.into_runtime_ports();
        self.registers = Rtl8125RegisterState::Runtime(owner);
        self.tx_regs = Some(tx);
        self.rx_regs = Some(rx);
        self.mac = mac;
        self.chip = chip;
        Ok(())
    }

    fn fail_initialization(&mut self, error: Error) -> OwnerInitPoll {
        self.registers = Rtl8125RegisterState::Failed;
        OwnerInitPoll::Failed(NetError::Other(Box::new(error)))
    }
}

impl rdif_eth::DriverGeneric for Rtl8125 {
    fn name(&self) -> &str {
        DRIVER_NAME
    }
}

impl Interface for Rtl8125 {
    fn poll_owner_init(&mut self, input: OwnerInitInput) -> OwnerInitPoll {
        let (tx_base, rx_base) = match (&self.tx_desc, &self.rx_desc) {
            (Some(tx), Some(rx)) => (tx.dma_addr().as_u64(), rx.dma_addr().as_u64()),
            _ => {
                return self
                    .fail_initialization(Error::InvalidState("pending queue descriptors missing"));
            }
        };
        let progress = match &self.registers {
            Rtl8125RegisterState::Initializing(regs) => {
                self.init_machine
                    .poll(regs, input, self.dma_mask, tx_base, rx_base)
            }
            Rtl8125RegisterState::Runtime(_) => return OwnerInitPoll::Ready,
            Rtl8125RegisterState::Failed => {
                return OwnerInitPoll::Failed(NetError::Other(Box::new(Error::InvalidState(
                    "initialization previously failed",
                ))));
            }
        };
        match progress {
            Rtl8125InitProgress::Pending(schedule) => OwnerInitPoll::Pending(schedule),
            Rtl8125InitProgress::Failed(error) => self.fail_initialization(error),
            Rtl8125InitProgress::Ready(ready) => {
                match self.finish_initialization(ready.mac, ready.chip) {
                    Ok(()) => {
                        info!(
                            "RTL8125 owner initialization complete: chip={:?}, mac={:02x?}",
                            self.chip, self.mac,
                        );
                        OwnerInitPoll::Ready
                    }
                    Err(error) => self.fail_initialization(error),
                }
            }
        }
    }

    fn mac_address(&self) -> [u8; 6] {
        self.mac
    }

    fn create_tx_queue(&mut self) -> Option<Box<dyn ITxQueue>> {
        let regs = self.tx_regs.take()?;
        let desc = self.tx_desc.take()?;
        Some(queue::boxed_tx(Rtl8125TxQueue {
            regs,
            desc,
            dma_mask: self.dma_mask,
            bus_addrs: [None; QUEUE_SIZE],
            next_submit: 0,
            next_reclaim: 0,
            link_up: None,
            link_down_drops: 0,
            submitted: 0,
            reclaimed: 0,
            _mapping: Arc::clone(&self._mapping),
        }))
    }

    fn create_rx_queue(&mut self) -> Option<Box<dyn IRxQueue>> {
        let regs = self.rx_regs.take()?;
        let desc = self.rx_desc.take()?;
        Some(queue::boxed_rx(Rtl8125RxQueue {
            regs,
            desc,
            dma_mask: self.dma_mask,
            bus_addrs: [None; QUEUE_SIZE],
            next_submit: 0,
            next_reclaim: 0,
            primed: 0,
            started: false,
            reclaimed: 0,
            rx_errors: 0,
            _mapping: Arc::clone(&self._mapping),
        }))
    }

    fn enable_irq(&mut self) -> core::result::Result<(), NetError> {
        let regs = self.runtime_regs().ok_or_else(rtl8125_not_ready)?;
        regs.enable_interrupts();
        if self.irq_epoch.is_masked() {
            regs.mask_interrupts();
            self.irq_enabled = false;
            return Err(NetError::Other(Box::new(Error::InvalidState(
                "contained IRQ source cannot be enabled",
            ))));
        }
        self.irq_enabled = true;
        Ok(())
    }

    fn disable_irq(&mut self) -> core::result::Result<(), NetError> {
        match &self.registers {
            Rtl8125RegisterState::Initializing(regs) => regs.mask_interrupts(),
            Rtl8125RegisterState::Runtime(regs) => regs.mask_interrupts(),
            Rtl8125RegisterState::Failed => return Err(rtl8125_not_ready()),
        }
        self.irq_enabled = false;
        Ok(())
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled && !self.irq_epoch.is_masked()
    }

    fn take_irq_endpoint(&mut self) -> Option<rdif_eth::BIrqEndpoint> {
        let port = self.irq_port.take()?;
        Some(Box::new(Rtl8125IrqEndpoint {
            port,
            epoch: Arc::clone(&self.irq_epoch),
            _mapping: Arc::clone(&self._mapping),
        }))
    }

    fn service_irq_event(&mut self, event: Event) -> core::result::Result<(), NetError> {
        if irq_has_link_change(event.device_status as u32) {
            let status = self.status().ok_or_else(rtl8125_not_ready)?;
            info!("RTL8125 link change: status={status:?}");
        }
        Ok(())
    }

    fn rearm_irq_source(&mut self, source: MaskedSource) -> core::result::Result<(), NetError> {
        self.irq_epoch.finish_masked_source(source)?;
        let regs = self.runtime_regs().ok_or_else(rtl8125_not_ready)?;
        regs.enable_interrupts();
        self.irq_enabled = true;
        Ok(())
    }
}

struct Rtl8125IrqEndpoint {
    port: Rtl8125IrqPort,
    epoch: Arc<Rtl8125IrqEpoch>,
    _mapping: Arc<Mmio>,
}

impl rdif_eth::IrqEndpoint for Rtl8125IrqEndpoint {
    type Event = Event;
    type Fault = EthernetIrqFault;

    fn capture(&mut self) -> IrqCapture<Event, EthernetIrqFault> {
        if self.epoch.is_masked() {
            return IrqCapture::Unhandled;
        }
        let Some(status) = self.port.capture_status() else {
            return IrqCapture::Unhandled;
        };
        IrqCapture::Captured {
            event: rtl8125_irq_event(status),
            masked: None,
        }
    }

    fn contain(
        &mut self,
        _cause: ContainmentCause,
    ) -> core::result::Result<MaskedSource, EthernetIrqFault> {
        self.port.mask_interrupts();
        self.epoch.begin_masked_source()
    }
}

struct Rtl8125IrqEpoch {
    next_generation: AtomicU64,
    active_generation: AtomicU64,
}

impl Rtl8125IrqEpoch {
    const fn new() -> Self {
        Self {
            next_generation: AtomicU64::new(1),
            active_generation: AtomicU64::new(0),
        }
    }

    fn is_masked(&self) -> bool {
        self.active_generation.load(Ordering::Acquire) != 0
    }

    fn begin_masked_source(&self) -> core::result::Result<MaskedSource, EthernetIrqFault> {
        let active = self.active_generation.load(Ordering::Acquire);
        if active != 0 {
            return MaskedSource::try_new(active, u64::from(DEFAULT_IRQ_MASK))
                .map_err(|_| EthernetIrqFault::Containment);
        }
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed).max(1);
        let generation = match self.active_generation.compare_exchange(
            0,
            generation,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => generation,
            Err(existing) => existing,
        };
        MaskedSource::try_new(generation, u64::from(DEFAULT_IRQ_MASK))
            .map_err(|_| EthernetIrqFault::Containment)
    }

    fn finish_masked_source(&self, source: MaskedSource) -> core::result::Result<(), NetError> {
        let generation = source.generation().get();
        if source.bitmap().get() != u64::from(DEFAULT_IRQ_MASK)
            || self
                .active_generation
                .compare_exchange(generation, 0, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return Err(NetError::Other(Box::new(Error::InvalidState(
                "stale RTL8125 IRQ source",
            ))));
        }
        Ok(())
    }
}

fn rtl8125_not_ready() -> NetError {
    NetError::Other(Box::new(Error::InvalidState(
        "RTL8125 owner is not initialized",
    )))
}

fn rtl8125_irq_event(status: u32) -> Event {
    let mut event = Event::none();
    event.device_status = u64::from(status);
    if irq_has_tx_event(status) {
        event.tx_queue.insert(QUEUE_ID0);
    }
    if irq_has_rx_event(status) {
        event.rx_queue.insert(QUEUE_ID0);
    }
    event
}

const _: () = {
    assert!(size_of::<TxDesc>() == 16);
    assert!(size_of::<RxDesc>() == 16);
};
