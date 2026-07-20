extern crate alloc;

use alloc::{boxed::Box, sync::Arc};
use core::{
    mem::size_of,
    sync::atomic::{AtomicU64, Ordering},
};

use dma_api::{CoherentArray, DeviceDma, DmaOp};
use mmio_api::{Mmio, MmioAddr, MmioOp};
use rdif_eth::{
    ContainmentCause, DmaBuffer, EthernetIrqFault, Event, IRxQueue, ITxQueue, Interface,
    IrqCapture, MaskedSource, NetError, OwnerInitInput, OwnerInitPoll, OwnerInitSchedule,
    QueueConfig, QueueMemoryMode,
};

use crate::err::{Error, Result};

mod descriptor;
mod registers;

use descriptor::{RxDesc, TxDesc};
use registers::*;

const QUEUE_SIZE: usize = 256;
const QUEUE_ID0: usize = 0;
const MAX_PACKET: usize = 2048;
const RESET_POLL_INTERVAL_NS: u64 = 100_000;
const RESET_TIMEOUT_NS: u64 = 100_000_000;
const E1000_IRQ_SOURCE: u64 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum E1000InitState {
    Discovered,
    ResetPending { deadline_ns: u64 },
    Ready,
    Failed,
}

enum E1000RegisterState {
    Initializing(E1000OwnerInitRegs),
    Runtime(E1000OwnerRegs),
    Failed,
}

pub struct E1000 {
    registers: E1000RegisterState,
    irq_port: Option<E1000IrqPort>,
    irq_epoch: Arc<E1000IrqEpoch>,
    tx_regs: Option<E1000TxRegs>,
    rx_regs: Option<E1000RxRegs>,
    tx_desc: Option<CoherentArray<TxDesc>>,
    rx_desc: Option<CoherentArray<RxDesc>>,
    dma_mask: u64,
    mac: [u8; 6],
    init_state: E1000InitState,
    irq_enabled: bool,
    _mapping: Arc<Mmio>,
}

impl E1000 {
    pub fn check_vid_did(vid: u16, did: u16) -> bool {
        vid == 0x8086 && [0x100e, 0x100f].contains(&did)
    }

    /// Maps and validates resources and constructs a pending controller.
    ///
    /// Device commands are intentionally deferred until `poll_owner_init`,
    /// after the final maintenance owner has registered its IRQ action.
    pub fn new(
        bar_addr: impl Into<MmioAddr>,
        bar_size: usize,
        dma_mask: u64,
        dma_op: &'static dyn DmaOp,
        mmio_op: &'static dyn MmioOp,
    ) -> Result<Self> {
        mmio_api::init(mmio_op);
        let mapping = Arc::new(mmio_api::ioremap(bar_addr.into(), bar_size)?);
        if mapping.size() < E1000_REGS_SIZE {
            return Err(Error::MmioTooSmall {
                size: mapping.size(),
                required: E1000_REGS_SIZE,
            });
        }

        let discovery = E1000DiscoveryRegs::new(mapping.as_nonnull_ptr());
        let (owner_regs, irq_port) = discovery.split_for_irq();
        let dma = DeviceDma::new_legacy(dma_mask, dma_op);
        let tx_desc = dma.coherent_array_zero_with_align::<TxDesc>(QUEUE_SIZE, 16)?;
        let rx_desc = dma.coherent_array_zero_with_align::<RxDesc>(QUEUE_SIZE, 16)?;

        Ok(Self {
            registers: E1000RegisterState::Initializing(owner_regs),
            irq_port: Some(irq_port),
            irq_epoch: Arc::new(E1000IrqEpoch::new()),
            tx_regs: None,
            rx_regs: None,
            tx_desc: Some(tx_desc),
            rx_desc: Some(rx_desc),
            dma_mask,
            mac: [0; 6],
            init_state: E1000InitState::Discovered,
            irq_enabled: false,
            _mapping: mapping,
        })
    }

    fn initializing_regs(&self) -> Option<&E1000OwnerInitRegs> {
        match &self.registers {
            E1000RegisterState::Initializing(regs) => Some(regs),
            _ => None,
        }
    }

    fn runtime_regs(&self) -> Option<&E1000OwnerRegs> {
        match &self.registers {
            E1000RegisterState::Runtime(regs) => Some(regs),
            _ => None,
        }
    }

    fn finish_initialization(&mut self) -> Result<()> {
        let mac = self
            .initializing_regs()
            .ok_or(Error::Other("E1000 initialization register state lost"))?
            .mac_address();
        if !valid_unicast_mac(mac) {
            return Err(Error::InvalidMacAddress(mac));
        }

        let tx_desc = self
            .tx_desc
            .as_ref()
            .ok_or(Error::Other("E1000 TX descriptors missing"))?;
        let rx_desc = self
            .rx_desc
            .as_ref()
            .ok_or(Error::Other("E1000 RX descriptors missing"))?;
        self.initializing_regs()
            .ok_or(Error::Other("E1000 initialization register state lost"))?
            .program_queues(
                tx_desc.dma_addr().as_u64(),
                (QUEUE_SIZE * size_of::<TxDesc>()) as u32,
                rx_desc.dma_addr().as_u64(),
                (QUEUE_SIZE * size_of::<RxDesc>()) as u32,
            );

        let registers = core::mem::replace(&mut self.registers, E1000RegisterState::Failed);
        let E1000RegisterState::Initializing(registers) = registers else {
            return Err(Error::Other("E1000 initialization transition repeated"));
        };
        let (owner, tx, rx) = registers.into_runtime_ports();
        self.registers = E1000RegisterState::Runtime(owner);
        self.tx_regs = Some(tx);
        self.rx_regs = Some(rx);
        self.mac = mac;
        self.init_state = E1000InitState::Ready;
        Ok(())
    }

    fn fail_initialization(&mut self, error: Error) -> OwnerInitPoll {
        self.init_state = E1000InitState::Failed;
        self.registers = E1000RegisterState::Failed;
        OwnerInitPoll::Failed(NetError::Other(Box::new(error)))
    }
}

impl rdif_eth::DriverGeneric for E1000 {
    fn name(&self) -> &str {
        "eth-intel-e1000"
    }
}

impl Interface for E1000 {
    fn poll_owner_init(&mut self, input: OwnerInitInput) -> OwnerInitPoll {
        match self.init_state {
            E1000InitState::Discovered => {
                let Some(regs) = self.initializing_regs() else {
                    return self
                        .fail_initialization(Error::Other("E1000 discovery register state lost"));
                };
                regs.mask_interrupts();
                regs.begin_reset();
                let deadline_ns = input.now_ns.saturating_add(RESET_TIMEOUT_NS);
                self.init_state = E1000InitState::ResetPending { deadline_ns };
                OwnerInitPoll::Pending(OwnerInitSchedule::wait_until(
                    input
                        .now_ns
                        .saturating_add(RESET_POLL_INTERVAL_NS)
                        .min(deadline_ns),
                ))
            }
            E1000InitState::ResetPending { deadline_ns } => {
                let Some(regs) = self.initializing_regs() else {
                    return self
                        .fail_initialization(Error::Other("E1000 reset register state lost"));
                };
                if regs.reset_pending() {
                    if input.now_ns >= deadline_ns {
                        return self.fail_initialization(Error::Timeout);
                    }
                    return OwnerInitPoll::Pending(OwnerInitSchedule::wait_until(
                        input
                            .now_ns
                            .saturating_add(RESET_POLL_INTERVAL_NS)
                            .min(deadline_ns),
                    ));
                }
                regs.mask_interrupts();
                regs.set_link_up();
                match self.finish_initialization() {
                    Ok(()) => OwnerInitPoll::Ready,
                    Err(error) => self.fail_initialization(error),
                }
            }
            E1000InitState::Ready => OwnerInitPoll::Ready,
            E1000InitState::Failed => OwnerInitPoll::Failed(NetError::Other(Box::new(
                Error::Other("E1000 initialization previously failed"),
            ))),
        }
    }

    fn mac_address(&self) -> [u8; 6] {
        self.mac
    }

    fn create_tx_queue(&mut self) -> Option<Box<dyn ITxQueue>> {
        if self.init_state != E1000InitState::Ready {
            return None;
        }
        let regs = self.tx_regs.take()?;
        let desc = self.tx_desc.take()?;
        Some(Box::new(E1000TxQueue {
            regs,
            desc,
            dma_mask: self.dma_mask,
            bus_addrs: [None; QUEUE_SIZE],
            next_submit: 0,
            next_reclaim: 0,
            _mapping: Arc::clone(&self._mapping),
        }))
    }

    fn create_rx_queue(&mut self) -> Option<Box<dyn IRxQueue>> {
        if self.init_state != E1000InitState::Ready {
            return None;
        }
        let regs = self.rx_regs.take()?;
        let desc = self.rx_desc.take()?;
        Some(Box::new(E1000RxQueue {
            regs,
            desc,
            dma_mask: self.dma_mask,
            bus_addrs: [None; QUEUE_SIZE],
            next_submit: 0,
            next_reclaim: 0,
            receiver_enabled: false,
            _mapping: Arc::clone(&self._mapping),
        }))
    }

    fn enable_irq(&mut self) -> core::result::Result<(), NetError> {
        if self.init_state != E1000InitState::Ready {
            return Err(e1000_not_ready());
        }
        let regs = self.runtime_regs().ok_or_else(e1000_not_ready)?;
        // Enable first and recheck the containment epoch. If hard IRQ
        // containment interrupted this sequence, the final mask wins.
        regs.enable_default_interrupts();
        if self.irq_epoch.is_masked() {
            regs.mask_interrupts();
            self.irq_enabled = false;
            return Err(NetError::Other(Box::new(Error::Other(
                "contained E1000 IRQ source cannot be enabled",
            ))));
        } else {
            self.irq_enabled = true;
        }
        Ok(())
    }

    fn disable_irq(&mut self) -> core::result::Result<(), NetError> {
        match &self.registers {
            E1000RegisterState::Initializing(regs) => regs.mask_interrupts(),
            E1000RegisterState::Runtime(regs) => regs.mask_interrupts(),
            E1000RegisterState::Failed => return Err(e1000_not_ready()),
        }
        self.irq_enabled = false;
        Ok(())
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled && !self.irq_epoch.is_masked()
    }

    fn take_irq_endpoint(&mut self) -> Option<rdif_eth::BIrqEndpoint> {
        let port = self.irq_port.take()?;
        Some(Box::new(E1000IrqEndpoint {
            port,
            epoch: Arc::clone(&self.irq_epoch),
            _mapping: Arc::clone(&self._mapping),
        }))
    }

    fn rearm_irq_source(&mut self, source: MaskedSource) -> core::result::Result<(), NetError> {
        self.irq_epoch.finish_masked_source(source)?;
        let regs = self.runtime_regs().ok_or_else(e1000_not_ready)?;
        regs.enable_default_interrupts();
        self.irq_enabled = true;
        Ok(())
    }
}

struct E1000IrqEndpoint {
    port: E1000IrqPort,
    epoch: Arc<E1000IrqEpoch>,
    _mapping: Arc<Mmio>,
}

impl rdif_eth::IrqEndpoint for E1000IrqEndpoint {
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
            event: e1000_irq_event(status),
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

struct E1000IrqEpoch {
    next_generation: AtomicU64,
    active_generation: AtomicU64,
}

impl E1000IrqEpoch {
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
            return MaskedSource::try_new(active, E1000_IRQ_SOURCE)
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
        MaskedSource::try_new(generation, E1000_IRQ_SOURCE)
            .map_err(|_| EthernetIrqFault::Containment)
    }

    fn finish_masked_source(&self, source: MaskedSource) -> core::result::Result<(), NetError> {
        let generation = source.generation().get();
        if source.bitmap().get() != E1000_IRQ_SOURCE
            || self
                .active_generation
                .compare_exchange(generation, 0, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return Err(NetError::Other(Box::new(Error::Other(
                "stale E1000 IRQ source",
            ))));
        }
        Ok(())
    }
}

fn e1000_not_ready() -> NetError {
    NetError::Other(Box::new(Error::Other("E1000 owner is not initialized")))
}

fn valid_unicast_mac(mac: [u8; 6]) -> bool {
    mac != [0; 6] && mac != [u8::MAX; 6] && mac[0] & 1 == 0
}

fn e1000_irq_event(icr: u32) -> Event {
    let mut event = Event::none();
    event.device_status = u64::from(icr);
    if icr & (1 << 0) != 0 {
        event.tx_queue.insert(QUEUE_ID0);
    }
    if icr & (1 << 7) != 0 {
        event.rx_queue.insert(QUEUE_ID0);
    }
    event
}

struct E1000TxQueue {
    regs: E1000TxRegs,
    desc: CoherentArray<TxDesc>,
    dma_mask: u64,
    bus_addrs: [Option<u64>; QUEUE_SIZE],
    next_submit: usize,
    next_reclaim: usize,
    _mapping: Arc<Mmio>,
}

impl ITxQueue for E1000TxQueue {
    fn id(&self) -> usize {
        QUEUE_ID0
    }

    fn config(&self) -> QueueConfig {
        QueueConfig {
            dma_mask: self.dma_mask,
            align: 16,
            buf_size: MAX_PACKET,
            ring_size: QUEUE_SIZE,
            memory_mode: QueueMemoryMode::DirectDma,
        }
    }

    fn submit(&mut self, buffer: DmaBuffer) -> core::result::Result<(), NetError> {
        if buffer.len > MAX_PACKET {
            return Err(NetError::Other(Box::new(Error::InvalidArgument(
                "tx packet too large",
            ))));
        }

        let idx = self.next_submit;
        let next = (idx + 1) % QUEUE_SIZE;
        if next == self.regs.head() {
            return Err(NetError::Retry);
        }

        self.desc
            .set_cpu(idx, TxDesc::new(buffer.bus_addr, buffer.len as u16));
        self.bus_addrs[idx] = Some(buffer.bus_addr);
        self.next_submit = next;
        self.regs.publish_tail(next);
        Ok(())
    }

    fn reclaim(&mut self) -> Option<u64> {
        let idx = self.next_reclaim;
        let desc = self.desc.read_cpu(idx)?;
        if !desc.is_done() {
            return None;
        }

        self.next_reclaim = (idx + 1) % QUEUE_SIZE;
        self.bus_addrs[idx].take()
    }
}

struct E1000RxQueue {
    regs: E1000RxRegs,
    desc: CoherentArray<RxDesc>,
    dma_mask: u64,
    bus_addrs: [Option<u64>; QUEUE_SIZE],
    next_submit: usize,
    next_reclaim: usize,
    receiver_enabled: bool,
    _mapping: Arc<Mmio>,
}

impl IRxQueue for E1000RxQueue {
    fn id(&self) -> usize {
        QUEUE_ID0
    }

    fn config(&self) -> QueueConfig {
        QueueConfig {
            dma_mask: self.dma_mask,
            align: 16,
            buf_size: MAX_PACKET,
            ring_size: QUEUE_SIZE,
            memory_mode: QueueMemoryMode::DirectDma,
        }
    }

    fn submit(&mut self, buffer: DmaBuffer) -> core::result::Result<(), NetError> {
        if buffer.len > MAX_PACKET {
            return Err(NetError::Other(Box::new(Error::InvalidArgument(
                "rx buffer too large",
            ))));
        }

        let idx = self.next_submit;
        let next = (idx + 1) % QUEUE_SIZE;
        if next == self.regs.head() {
            return Err(NetError::Retry);
        }

        self.desc.set_cpu(idx, RxDesc::new(buffer.bus_addr));
        self.bus_addrs[idx] = Some(buffer.bus_addr);
        self.next_submit = next;
        self.regs.publish_tail(next);
        if !self.receiver_enabled {
            self.regs.enable_receiver();
            self.receiver_enabled = true;
        }
        Ok(())
    }

    fn reclaim(&mut self) -> Option<(u64, usize)> {
        let idx = self.next_reclaim;
        let desc = self.desc.read_cpu(idx)?;
        if !desc.is_done() {
            return None;
        }

        self.next_reclaim = (idx + 1) % QUEUE_SIZE;
        self.bus_addrs[idx]
            .take()
            .map(|bus_addr| (bus_addr, desc.length as usize))
    }
}
