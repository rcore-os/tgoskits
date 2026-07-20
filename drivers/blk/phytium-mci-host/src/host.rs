use alloc::sync::Arc;
use core::{
    num::NonZeroU64,
    ptr::NonNull,
    sync::atomic::{self, AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering},
};

use dma_api::DeviceDma;
use mmio_api::MmioRaw;
use rdif_irq::{
    ContainmentCause, FaultContainment, IrqCapture, IrqEndpoint, IrqSourceControl, MaskedSource,
};
use sdmmc_protocol::{
    error::{Error, ErrorContext, Phase},
    sdio::host::{BusWidth, SdioIrqControlError, SdioIrqSource, SignalVoltage},
};
use volatile::VolatilePtr;

use crate::{
    Event, PhytiumMciIrqControl, PhytiumMciIrqEndpoint, PhytiumMciIrqSource,
    command::CommandState,
    regs::{
        CLK_SRC_OFFSET, CType, ClockSource, RIntSts, RegisterBlock,
        RegisterBlockVolatileFieldAccess, Uhs,
    },
};

pub const DEFAULT_FIFO_OFFSET: usize = 0x200;
const DEFAULT_FIFO_WORD_DEPTH: u32 = 128;
pub(crate) const FIFO_THRESHOLD: u32 = (2 << 28) | (7 << 16) | 0x100;
pub(crate) const CARD_READ_THRESHOLD_ENABLE: u32 = 1;
pub(crate) const CARD_READ_THRESHOLD_DEPTH8: u32 = 1 << 23;
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingData {
    pub direction: sdmmc_protocol::DataDirection,
    pub block_size: u32,
    pub block_count: u32,
    pub use_idmac: bool,
}

pub(crate) struct IrqState {
    status_mailbox: AtomicU64,
    idmac_mailbox: AtomicU64,
    next_generation: AtomicU32,
    register_owner: AtomicU8,
    delivery_enabled: AtomicBool,
    source_taken: AtomicBool,
    source_holders: AtomicU8,
    source_online: AtomicBool,
    source_generation: AtomicU64,
    masked_sources: AtomicU64,
    masked_intmask: AtomicU32,
    masked_idinten: AtomicU32,
}

const REGISTER_OWNER_IDLE: u8 = 0;
const REGISTER_OWNER_TASK: u8 = 1;
const REGISTER_OWNER_IRQ: u8 = 2;
const PHYTIUM_MCI_IRQ_SOURCE_BITMAP: u64 = 1;
const CAPTURE_ENDPOINT_HOLDER: u8 = 1 << 0;
const SOURCE_CONTROL_HOLDER: u8 = 1 << 1;
const ALL_SOURCE_HOLDERS: u8 = CAPTURE_ENDPOINT_HOLDER | SOURCE_CONTROL_HOLDER;

pub(crate) struct RegisterOwner<'a> {
    owner: &'a AtomicU8,
}

impl Drop for RegisterOwner<'_> {
    fn drop(&mut self) {
        self.owner.store(REGISTER_OWNER_IDLE, Ordering::Release);
    }
}

const IRQ_GENERATION_SHIFT: u64 = 32;
const IRQ_STATUS_MASK: u64 = u32::MAX as u64;

impl IrqState {
    pub(crate) const fn new() -> Self {
        Self {
            status_mailbox: AtomicU64::new(0),
            idmac_mailbox: AtomicU64::new(0),
            next_generation: AtomicU32::new(0),
            register_owner: AtomicU8::new(REGISTER_OWNER_IDLE),
            delivery_enabled: AtomicBool::new(false),
            source_taken: AtomicBool::new(false),
            source_holders: AtomicU8::new(0),
            source_online: AtomicBool::new(false),
            source_generation: AtomicU64::new(0),
            masked_sources: AtomicU64::new(0),
            masked_intmask: AtomicU32::new(0),
            masked_idinten: AtomicU32::new(0),
        }
    }

    pub(crate) fn take_source(&self) -> bool {
        let taken = self
            .source_taken
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        if taken {
            self.source_holders
                .store(ALL_SOURCE_HOLDERS, Ordering::Release);
        }
        taken
    }

    pub(crate) fn source_ready(&self) -> bool {
        self.source_taken.load(Ordering::Acquire)
            && self.source_holders.load(Ordering::Acquire) == ALL_SOURCE_HOLDERS
    }

    pub(crate) fn release_capture_endpoint(&self) {
        self.release_source_holder(CAPTURE_ENDPOINT_HOLDER);
    }

    pub(crate) fn release_source_control(&self) {
        self.release_source_holder(SOURCE_CONTROL_HOLDER);
    }

    pub(crate) fn activate_source(&self) -> NonZeroU64 {
        let mut current = self.source_generation.load(Ordering::Acquire);
        let generation = loop {
            let mut next = current.wrapping_add(1);
            if next == 0 {
                next = 1;
            }
            match self.source_generation.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break NonZeroU64::new(next).expect("IRQ source epoch is nonzero"),
                Err(observed) => current = observed,
            }
        };
        self.masked_sources.store(0, Ordering::Release);
        self.source_online.store(true, Ordering::Release);
        generation
    }

    pub(crate) fn deactivate_source(&self) {
        self.source_online.store(false, Ordering::Release);
        self.masked_sources.store(0, Ordering::Release);
    }

    pub(crate) fn source_generation(&self) -> Option<NonZeroU64> {
        NonZeroU64::new(self.source_generation.load(Ordering::Acquire))
    }

    pub(crate) fn source_online(&self) -> bool {
        self.source_online.load(Ordering::Acquire)
    }

    pub(crate) fn mark_source_masked(&self, intmask: u32, idinten: u32) {
        if self.masked_sources.load(Ordering::Acquire) & PHYTIUM_MCI_IRQ_SOURCE_BITMAP == 0 {
            self.masked_intmask.store(intmask, Ordering::Relaxed);
            self.masked_idinten.store(idinten, Ordering::Relaxed);
            self.masked_sources
                .fetch_or(PHYTIUM_MCI_IRQ_SOURCE_BITMAP, Ordering::Release);
        }
    }

    pub(crate) fn claim_masked_source(&self, bitmap: u64) -> Option<(u32, u32)> {
        let mut current = self.masked_sources.load(Ordering::Acquire);
        loop {
            if bitmap == 0 || bitmap & !current != 0 {
                return None;
            }
            match self.masked_sources.compare_exchange_weak(
                current,
                current & !bitmap,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some((
                        self.masked_intmask.load(Ordering::Acquire),
                        self.masked_idinten.load(Ordering::Acquire),
                    ));
                }
                Err(observed) => current = observed,
            }
        }
    }

    pub(crate) fn set_delivery_enabled(&self, enabled: bool) {
        self.delivery_enabled.store(enabled, Ordering::Release);
    }

    pub(crate) fn delivery_enabled(&self) -> bool {
        self.delivery_enabled.load(Ordering::Acquire)
    }

    pub(crate) fn try_begin_task_update(&self) -> Option<RegisterOwner<'_>> {
        self.register_owner
            .compare_exchange(
                REGISTER_OWNER_IDLE,
                REGISTER_OWNER_TASK,
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .ok()
            .map(|_| RegisterOwner {
                owner: &self.register_owner,
            })
    }

    pub(crate) fn try_begin_irq_snapshot(&self) -> Option<RegisterOwner<'_>> {
        self.register_owner
            .compare_exchange(
                REGISTER_OWNER_IDLE,
                REGISTER_OWNER_IRQ,
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .ok()
            .map(|_| RegisterOwner {
                owner: &self.register_owner,
            })
    }

    pub(crate) fn begin_request(&self) {
        let generation = self.next_generation();
        let clean = pack_mailbox(generation, 0);
        self.idmac_mailbox.store(clean, Ordering::Release);
        self.status_mailbox.store(clean, Ordering::Release);
    }

    pub(crate) fn end_request(&self) {
        self.status_mailbox.store(0, Ordering::Release);
        self.idmac_mailbox.store(0, Ordering::Release);
    }

    pub(crate) fn cache_if_current(&self, generation: u32, status: u32, idmac_status: u32) {
        if generation == 0 {
            return;
        }
        if status != 0 {
            cache_mailbox_if_current(&self.status_mailbox, generation, status);
        }
        if idmac_status != 0 {
            cache_mailbox_if_current(&self.idmac_mailbox, generation, idmac_status);
        }
    }

    pub(crate) fn generation(&self) -> u32 {
        mailbox_generation(self.status_mailbox.load(Ordering::Acquire))
    }

    pub(crate) fn take_status(&self, mask: u32) -> u32 {
        take_mailbox_bits(&self.status_mailbox, mask)
    }

    pub(crate) fn take_idmac_status(&self, mask: u32) -> u32 {
        take_mailbox_bits(&self.idmac_mailbox, mask)
    }

    pub(crate) fn clear_status(&self, mask: u32) {
        clear_mailbox_bits(&self.status_mailbox, mask);
    }

    pub(crate) fn clear_all(&self) {
        clear_mailbox_bits(&self.status_mailbox, u32::MAX);
        clear_mailbox_bits(&self.idmac_mailbox, u32::MAX);
    }

    #[cfg(test)]
    pub(crate) fn pending_status(&self) -> u32 {
        mailbox_status(self.status_mailbox.load(Ordering::Acquire))
    }

    #[cfg(test)]
    pub(crate) fn pending_idmac_status(&self) -> u32 {
        mailbox_status(self.idmac_mailbox.load(Ordering::Acquire))
    }

    fn next_generation(&self) -> u32 {
        let mut cur = self.next_generation.load(Ordering::Acquire);
        loop {
            let mut next = cur.wrapping_add(1);
            if next == 0 {
                next = 1;
            }
            match self.next_generation.compare_exchange_weak(
                cur,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return next,
                Err(observed) => cur = observed,
            }
        }
    }

    fn release_source_holder(&self, holder: u8) {
        let previous = self.source_holders.fetch_and(!holder, Ordering::AcqRel);
        debug_assert_ne!(
            previous & holder,
            0,
            "Phytium MCI IRQ source capability released more than once"
        );
        if previous == holder {
            // Drop retires only a synchronized software capability. Hardware
            // containment and IRQ-action synchronization are explicit steps.
            self.source_taken.store(false, Ordering::Release);
        }
    }
}

fn pack_mailbox(generation: u32, status: u32) -> u64 {
    ((generation as u64) << IRQ_GENERATION_SHIFT) | status as u64
}

fn mailbox_generation(value: u64) -> u32 {
    (value >> IRQ_GENERATION_SHIFT) as u32
}

fn mailbox_status(value: u64) -> u32 {
    (value & IRQ_STATUS_MASK) as u32
}

fn cache_mailbox_if_current(mailbox: &AtomicU64, generation: u32, status: u32) {
    let mut cur = mailbox.load(Ordering::Acquire);
    loop {
        if mailbox_generation(cur) != generation {
            return;
        }
        let next = pack_mailbox(generation, mailbox_status(cur) | status);
        match mailbox.compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return,
            Err(observed) => cur = observed,
        }
    }
}

fn take_mailbox_bits(mailbox: &AtomicU64, mask: u32) -> u32 {
    let mut cur = mailbox.load(Ordering::Acquire);
    loop {
        let status = mailbox_status(cur);
        let taken = status & mask;
        if taken == 0 {
            return 0;
        }
        let next = pack_mailbox(mailbox_generation(cur), status & !mask);
        match mailbox.compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return taken,
            Err(observed) => cur = observed,
        }
    }
}

fn clear_mailbox_bits(mailbox: &AtomicU64, mask: u32) {
    let mut cur = mailbox.load(Ordering::Acquire);
    loop {
        let next = pack_mailbox(mailbox_generation(cur), mailbox_status(cur) & !mask);
        match mailbox.compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return,
            Err(observed) => cur = observed,
        }
    }
}

pub(crate) struct IrqCore {
    pub(crate) regs: VolatilePtr<'static, RegisterBlock>,
    pub(crate) state: IrqState,
}

// SAFETY: `IrqCore` is shared only between bounded task-side event service and
// the IRQ top-half. MMIO accesses are volatile and snapshots cross through
// atomics.
unsafe impl Send for IrqCore {}
// SAFETY: See the `Send` impl.
unsafe impl Sync for IrqCore {}

impl IrqCore {
    fn new(regs: VolatilePtr<'static, RegisterBlock>) -> Self {
        Self {
            regs,
            state: IrqState::new(),
        }
    }
}

pub struct PhytiumMci {
    pub(crate) regs: VolatilePtr<'static, RegisterBlock>,
    pub(crate) base_addr: usize,
    pub(crate) fifo_offset: usize,
    pub(crate) command_state: CommandState,
    pub(crate) pending_data: Option<PendingData>,
    pub(crate) data_cmd_index: u8,
    pub(crate) data_blocks_remaining: u32,
    pub(crate) dma: Option<DeviceDma>,
    pub(crate) dma_mask: u64,
    pub(crate) dma_poisoned: bool,
    /// Set only after reset completion proves every FIFO/IDMAC engine idle.
    pub(crate) recovery_quiesced: bool,
    pub(crate) use_hold_reg: bool,
    pub(crate) irq: Arc<IrqCore>,
    pub(crate) host2_next_id: u64,
    pub(crate) host2_active_id: Option<u64>,
}

impl PhytiumMci {
    pub unsafe fn new(base: NonNull<u8>) -> Self {
        unsafe { Self::new_with_fifo_offset(base, DEFAULT_FIFO_OFFSET) }
    }

    pub unsafe fn new_with_fifo_offset(base: NonNull<u8>, fifo_offset: usize) -> Self {
        let regs = unsafe { VolatilePtr::new(base.cast()) };
        Self {
            regs,
            base_addr: base.as_ptr() as usize,
            fifo_offset,
            command_state: CommandState::Idle,
            pending_data: None,
            data_cmd_index: 0,
            data_blocks_remaining: 0,
            dma: None,
            dma_mask: u32::MAX as u64,
            dma_poisoned: false,
            recovery_quiesced: false,
            use_hold_reg: true,
            irq: Arc::new(IrqCore::new(regs)),
            host2_next_id: 0,
            host2_active_id: None,
        }
    }

    pub unsafe fn new_from_mmio_raw(mmio: &MmioRaw) -> Self {
        unsafe { Self::new(mmio.as_nonnull_ptr()) }
    }

    pub unsafe fn new_from_addr(base_addr: usize) -> Self {
        let base = NonNull::new(base_addr as *mut u8).expect("MMIO base address must be non-null");
        unsafe { Self::new(base) }
    }

    /// Install a DMA capability used by high-level data-transfer hooks.
    ///
    /// Once installed, `SdioHost` and `sdio_host2::SdioHost` data transactions
    /// try the internal IDMAC first for 512-byte block I/O and fall back to the
    /// FIFO state machine when the DMA path is not applicable.
    pub fn set_dma(&mut self, dma: DeviceDma) {
        self.dma_mask = dma.dma_mask();
        self.dma = Some(dma);
    }

    pub(crate) fn check_not_poisoned(&self) -> Result<(), Error> {
        if self.dma_poisoned {
            Err(Error::BusError(ErrorContext::new(Phase::DataRead)))
        } else {
            Ok(())
        }
    }

    pub(crate) fn poison_dma(&mut self) {
        self.dma_poisoned = true;
    }

    pub(crate) fn clear_all_int_status(&self) {
        let cur = self.regs.rintsts().read();
        self.regs.rintsts().write(cur);
    }

    pub(crate) fn enable_completion_irq(&mut self) {
        let _ = self.irq.state.activate_source();
        self.regs.intmask().write(
            crate::MCI_INT_COMMAND_DONE
                | crate::MCI_INT_DATA_TRANSFER_OVER
                | crate::MCI_INT_RXDR
                | crate::MCI_INT_TXDR
                | crate::MCI_INT_ERROR_MASK,
        );
        self.regs.ctrl().update(|r| r.with_int_enable(true));
        self.irq.state.set_delivery_enabled(true);
    }

    pub(crate) fn disable_completion_irq(&mut self) {
        self.regs.intmask().write(0);
        self.regs.idinten().write(0);
        self.regs.ctrl().update(|r| r.with_int_enable(false));
        self.irq.state.set_delivery_enabled(false);
        self.irq.state.deactivate_source();
    }

    pub(crate) fn clear_completion_irq_enabled(&self) {
        self.irq.state.set_delivery_enabled(false);
        self.irq.state.deactivate_source();
    }

    pub fn completion_irq_enabled(&self) -> bool {
        self.irq.state.delivery_enabled()
    }

    /// Acquires the controller's unique live capture/control source lease.
    ///
    /// The maintenance owner must register `endpoint` on its pinned CPU while
    /// controller delivery is disabled. It retains `control` and may enable
    /// completion delivery only after registration succeeds. A later
    /// activation may acquire a new lease after both synchronized halves retire.
    pub fn take_irq_source(&mut self) -> Option<PhytiumMciIrqSource> {
        self.irq.state.take_source().then(|| {
            SdioIrqSource::new(
                PhytiumMciIrqEndpoint {
                    irq: Arc::clone(&self.irq),
                },
                PhytiumMciIrqControl {
                    irq: Arc::clone(&self.irq),
                },
            )
        })
    }

    pub(crate) fn event_from_raw_irq(raw: u32, idsts: u32) -> Event {
        if raw & crate::MCI_INT_ERROR_MASK != 0 {
            Event::Error { raw_status: raw }
        } else if idsts & crate::MCI_IDSTS_ERROR_MASK != 0 {
            Event::DmaError { raw_status: idsts }
        } else if raw & crate::MCI_INT_DATA_TRANSFER_OVER != 0 {
            Event::TransferComplete
        } else if idsts & (crate::MCI_IDSTS_RECEIVE | crate::MCI_IDSTS_TRANSMIT) != 0 {
            Event::DmaComplete
        } else if raw & crate::MCI_INT_COMMAND_DONE != 0 {
            Event::CommandComplete
        } else if raw & crate::MCI_INT_RXDR != 0 {
            Event::ReceiveReady
        } else if raw & crate::MCI_INT_TXDR != 0 {
            Event::TransmitReady
        } else if raw != 0 || idsts != 0 {
            Event::Other {
                raw_status: raw | idsts,
            }
        } else {
            Event::None
        }
    }

    pub(crate) fn set_bus_width(&mut self, width: BusWidth) {
        let ctype = match width {
            BusWidth::Bit1 => CType::new(),
            BusWidth::Bit4 => CType::new().with_width4(1),
            BusWidth::Bit8 => CType::new().with_width8(1),
            // Future BusWidth variants: fall back to 1-bit (no width bits set).
            _ => CType::new(),
        };
        self.regs.ctype().write(ctype);
    }

    pub(crate) fn program_data_phase(&self, block_size: u32, block_count: u32) {
        self.regs.blksiz().write(block_size);
        self.regs.bytcnt().write(block_size * block_count);
    }

    pub(crate) fn translate_int_error(&self, ints: RIntSts, phase: Phase, cmd_index: u8) -> Error {
        let ctx = ErrorContext::for_cmd(phase, cmd_index);
        if ints.response_timeout() || ints.data_read_timeout() || ints.host_timeout() {
            Error::Timeout(ctx)
        } else if ints.response_crc_error() || ints.data_crc_error() {
            Error::Crc(ctx)
        } else if ints.response_error() {
            Error::BadResponse(ctx)
        } else if matches!(phase, Phase::DataRead) {
            Error::ReadError(ctx)
        } else if matches!(phase, Phase::DataWrite) {
            Error::WriteError(ctx)
        } else {
            Error::BusError(ctx)
        }
    }

    pub(crate) fn fifo_word_depth(&self) -> u32 {
        DEFAULT_FIFO_WORD_DEPTH
    }

    pub(crate) fn fifo_ptr(&self) -> *mut u32 {
        (self.base_addr + self.fifo_offset) as *mut u32
    }

    pub(crate) fn write_ext_reg(&self, offset: usize, value: u32) {
        let ptr = (self.base_addr + offset) as *mut u32;
        unsafe {
            ptr.write_volatile(value);
        }
        atomic::fence(atomic::Ordering::SeqCst);
    }

    pub(crate) fn read_clock_source_raw(&self) -> u32 {
        let ptr = (self.base_addr + CLK_SRC_OFFSET) as *const u32;
        unsafe { ptr.read_volatile() }
    }

    #[allow(dead_code)]
    fn read_clock_source(&self) -> ClockSource {
        ClockSource::from_bits(self.read_clock_source_raw())
    }
}

impl IrqEndpoint for PhytiumMciIrqEndpoint {
    type Event = Event;
    type Fault = Error;

    fn capture(&mut self) -> IrqCapture<Self::Event, Self::Fault> {
        capture_irq_core(&self.irq)
    }

    fn contain(&mut self, _cause: ContainmentCause) -> Result<MaskedSource, Self::Fault> {
        mask_irq_delivery(&self.irq);
        let generation = self
            .irq
            .state
            .source_generation()
            .ok_or(Error::InvalidArgument)?;
        Ok(MaskedSource::new(
            generation,
            NonZeroU64::new(PHYTIUM_MCI_IRQ_SOURCE_BITMAP)
                .expect("Phytium MCI source bitmap is nonzero"),
        ))
    }
}

impl Drop for PhytiumMciIrqEndpoint {
    fn drop(&mut self) {
        self.irq.state.release_capture_endpoint();
    }
}

impl IrqSourceControl for PhytiumMciIrqControl {
    type Error = SdioIrqControlError;

    fn rearm(&mut self, source: MaskedSource) -> Result<(), Self::Error> {
        let expected = self
            .irq
            .state
            .source_generation()
            .ok_or(SdioIrqControlError::Offline)?;
        let actual = source.generation();
        if actual != expected {
            return Err(SdioIrqControlError::StaleGeneration {
                expected: expected.get(),
                actual: actual.get(),
            });
        }
        let bitmap = source.bitmap().get();
        if bitmap != PHYTIUM_MCI_IRQ_SOURCE_BITMAP {
            return Err(SdioIrqControlError::SourceNotMasked { bitmap });
        }
        if !self.irq.state.source_online() {
            return Err(SdioIrqControlError::Offline);
        }
        let Some((intmask, idinten)) = self.irq.state.claim_masked_source(bitmap) else {
            return Err(SdioIrqControlError::SourceNotMasked { bitmap });
        };
        self.irq.regs.intmask().write(intmask);
        self.irq.regs.idinten().write(idinten);
        self.irq
            .regs
            .ctrl()
            .update(|control| control.with_int_enable(true));
        self.irq.state.set_delivery_enabled(true);
        Ok(())
    }
}

impl Drop for PhytiumMciIrqControl {
    fn drop(&mut self) {
        self.irq.state.release_source_control();
    }
}

fn capture_irq_core(irq: &IrqCore) -> IrqCapture<Event, Error> {
    let Some(_register_owner) = irq.state.try_begin_irq_snapshot() else {
        // The source must be bound to the same CPU as the maintenance owner,
        // which excludes local delivery while task-side registers are being
        // published. A conflict is therefore a binding invariant violation,
        // not a retryable device event. The runtime must mask the parent IRQ
        // action before recovery because touching device masks here could race
        // the interrupted register update.
        return IrqCapture::Fault {
            reason: Error::Busy,
            containment: FaultContainment::Uncontained,
        };
    };
    let generation = irq.state.generation();
    let raw = irq.regs.rintsts().read().into_bits();
    let idsts = irq.regs.idsts().read();
    if raw != 0 {
        irq.regs.rintsts().write(RIntSts::from_bits(raw));
    }
    if idsts != 0 {
        irq.regs.idsts().write(idsts);
    }
    irq.state.cache_if_current(generation, raw, idsts);

    let event = PhytiumMci::event_from_raw_irq(raw, idsts);
    if matches!(event, Event::None) {
        IrqCapture::Unhandled
    } else {
        IrqCapture::Captured {
            event,
            masked: None,
        }
    }
}

fn mask_irq_delivery(irq: &IrqCore) {
    let intmask = irq.regs.intmask().read();
    let idinten = irq.regs.idinten().read();
    irq.regs.intmask().write(0);
    irq.regs.idinten().write(0);
    irq.regs
        .ctrl()
        .update(|control| control.with_int_enable(false));
    irq.state.mark_source_masked(intmask, idinten);
    irq.state.set_delivery_enabled(false);
}

pub(crate) fn uhs_bits_after_voltage(bits: Uhs, voltage: SignalVoltage) -> Result<Uhs, Error> {
    match voltage {
        SignalVoltage::V330 => Ok(bits.with_volt(0)),
        SignalVoltage::V180 => Ok(bits.with_volt(1)),
        SignalVoltage::V120 => Err(Error::UnsupportedCommand),
        // Future SignalVoltage variants are not supported by this controller.
        _ => Err(Error::UnsupportedCommand),
    }
}

unsafe impl Send for PhytiumMci {}

#[cfg(test)]
mod tests {
    use core::ptr::NonNull;

    use sdmmc_protocol::sdio::host::{HostEvent, HostEventKind};

    use super::*;

    #[test]
    fn constructs_from_mapped_mmio_pointer() {
        let base = NonNull::new(0x2800_0000 as *mut u8).unwrap();
        let host = unsafe { PhytiumMci::new(base) };

        assert_eq!(host.base_addr, 0x2800_0000);
        assert_eq!(host.fifo_offset, DEFAULT_FIFO_OFFSET);
    }

    #[test]
    fn discovery_constructor_does_not_reset_issue_or_ack_the_controller() {
        let mut mmio = [0u32; 256];
        mmio[0] = 0xa5a5_5a5a;
        mmio[11] = 0x1357_2468;
        mmio[17] = crate::MCI_INT_COMMAND_DONE;
        mmio[36] = crate::MCI_IDSTS_RECEIVE;
        let before = mmio;
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();

        let _host = unsafe { PhytiumMci::new(base) };

        assert_eq!(mmio, before);
    }

    #[test]
    fn explicit_fifo_offset_is_kept() {
        let base = NonNull::new(0x2800_0000 as *mut u8).unwrap();
        let host = unsafe { PhytiumMci::new_with_fifo_offset(base, 0x400) };

        assert_eq!(host.fifo_offset, 0x400);
    }

    #[test]
    fn handle_irq_wakes_on_idmac_receive_done() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { PhytiumMci::new(base) };
        let (mut endpoint, _control) = host.take_irq_source().unwrap().into_parts();
        host.irq.state.begin_request();
        let old_generation = host.irq.state.generation();
        const IDSTS_WORD: usize = 36;
        const IDSTS_RECEIVE: u32 = 1 << 1;

        unsafe {
            mmio.as_mut_ptr()
                .add(IDSTS_WORD)
                .write_volatile(IDSTS_RECEIVE)
        };

        let IrqCapture::Captured { event, masked } = endpoint.capture() else {
            panic!("asserted IDMAC status must be captured");
        };
        assert!(masked.is_none());
        assert_eq!(event, crate::Event::DmaComplete);
        assert_eq!(event.kind(), HostEventKind::Other);
        assert_eq!(host.irq.state.pending_idmac_status(), IDSTS_RECEIVE);
        assert_eq!(host.irq.state.pending_status(), 0);

        let _ = host.irq.state.take_idmac_status(IDSTS_RECEIVE);
        host.irq.state.end_request();
        host.irq.state.begin_request();
        assert_ne!(host.irq.state.generation(), old_generation);
        host.irq
            .state
            .cache_if_current(old_generation, 0, IDSTS_RECEIVE);
        assert_eq!(host.irq.state.pending_idmac_status(), 0);
    }

    #[test]
    fn idmac_abnormal_summary_wins_over_combined_transfer_completion() {
        const IDSTS_ABNORMAL_SUMMARY: u32 = 1 << 9;

        assert!(matches!(
            PhytiumMci::event_from_raw_irq(
                crate::MCI_INT_DATA_TRANSFER_OVER,
                IDSTS_ABNORMAL_SUMMARY,
            ),
            crate::Event::DmaError { .. }
        ));
    }

    #[test]
    fn register_ownership_conflict_is_a_fail_closed_fault_not_deferred_work() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { PhytiumMci::new(base) };
        let (mut endpoint, _control) = host.take_irq_source().unwrap().into_parts();
        host.irq.state.begin_request();
        let irq_core = host.irq.clone();
        let task_owner = irq_core
            .state
            .try_begin_task_update()
            .expect("idle register gate must admit task setup");
        const IDSTS_WORD: usize = 36;
        const IDSTS_RECEIVE: u32 = 1 << 1;
        unsafe {
            mmio.as_mut_ptr()
                .add(IDSTS_WORD)
                .write_volatile(IDSTS_RECEIVE);
        }

        assert!(matches!(
            endpoint.capture(),
            IrqCapture::Fault {
                reason: Error::Busy,
                containment: FaultContainment::Uncontained,
            }
        ));
        assert_eq!(host.irq.state.pending_idmac_status(), 0);
        assert_eq!(
            unsafe { mmio.as_ptr().add(IDSTS_WORD).read_volatile() },
            IDSTS_RECEIVE
        );

        drop(task_owner);
        assert!(matches!(
            endpoint.capture(),
            IrqCapture::Captured {
                event: crate::Event::DmaComplete,
                masked: None,
            }
        ));
        assert_eq!(host.irq.state.pending_idmac_status(), IDSTS_RECEIVE);
    }

    #[test]
    fn task_register_update_never_spins_behind_irq_snapshot() {
        let state = IrqState::new();
        let irq_owner = state
            .try_begin_irq_snapshot()
            .expect("idle register gate must admit the IRQ snapshot");

        assert!(state.try_begin_task_update().is_none());

        drop(irq_owner);
        assert!(state.try_begin_task_update().is_some());
    }

    #[test]
    fn disabling_completion_delivery_masks_controller_and_idmac_sources() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { PhytiumMci::new(base) };
        host.enable_completion_irq();
        host.regs.idinten().write(u32::MAX);

        host.disable_completion_irq();

        assert_eq!(host.regs.intmask().read(), 0);
        assert_eq!(host.regs.idinten().read(), 0);
        assert!(!host.regs.ctrl().read().int_enable());
    }

    #[test]
    fn protocol_irq_enable_requires_source_transfer() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { PhytiumMci::new(base) };

        assert_eq!(
            sdmmc_protocol::sdio::host::SdioHost::enable_completion_irq(&mut host),
            Err(Error::InvalidArgument)
        );
        let _source = host.take_irq_source().unwrap();
        assert!(host.take_irq_source().is_none());
        sdmmc_protocol::sdio::host::SdioHost::enable_completion_irq(&mut host).unwrap();
    }

    #[test]
    fn irq_source_can_be_reacquired_only_after_both_capabilities_are_released() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { PhytiumMci::new(base) };
        let (endpoint, control) = host.take_irq_source().unwrap().into_parts();

        drop(endpoint);
        assert!(host.take_irq_source().is_none());
        drop(control);

        let (endpoint, control) = host
            .take_irq_source()
            .expect("the source lease must return after both halves retire")
            .into_parts();
        drop(control);
        assert!(host.take_irq_source().is_none());
        drop(endpoint);
        assert!(host.take_irq_source().is_some());
    }

    #[test]
    fn reacquired_source_advances_generation_and_rejects_stale_tokens() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { PhytiumMci::new(base) };
        let (mut endpoint, control) = host.take_irq_source().unwrap().into_parts();
        sdmmc_protocol::sdio::host::SdioHost::enable_completion_irq(&mut host).unwrap();
        let stale = endpoint
            .contain(ContainmentCause::PublicationClosed)
            .unwrap();
        let first_generation = stale.generation();

        sdmmc_protocol::sdio::host::SdioHost::disable_completion_irq(&mut host).unwrap();
        drop(endpoint);
        drop(control);
        let (endpoint, mut control) = host
            .take_irq_source()
            .expect("a synchronized source must be reusable for runtime")
            .into_parts();
        sdmmc_protocol::sdio::host::SdioHost::enable_completion_irq(&mut host).unwrap();
        let second_generation = host.irq.state.source_generation().unwrap();

        assert!(second_generation.get() > first_generation.get());
        assert!(matches!(
            control.rearm(stale),
            Err(SdioIrqControlError::StaleGeneration { actual, expected })
                if actual == stale.generation().get() && expected != actual
        ));
        drop(endpoint);
    }

    #[test]
    fn containment_preserves_exact_masks_until_generation_checked_rearm() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { PhytiumMci::new(base) };
        let (mut endpoint, mut control) = host.take_irq_source().unwrap().into_parts();
        sdmmc_protocol::sdio::host::SdioHost::enable_completion_irq(&mut host).unwrap();
        host.regs.idinten().write(0x35);
        let intmask = host.regs.intmask().read();

        let token = endpoint.contain(ContainmentCause::PublicationFull).unwrap();
        let duplicate = endpoint
            .contain(ContainmentCause::PublicationClosed)
            .unwrap();
        assert_eq!(duplicate, token);
        assert_eq!(host.regs.intmask().read(), 0);
        assert_eq!(host.regs.idinten().read(), 0);
        assert!(!host.completion_irq_enabled());

        control.rearm(token).unwrap();
        assert_eq!(host.regs.intmask().read(), intmask);
        assert_eq!(host.regs.idinten().read(), 0x35);
        assert!(host.completion_irq_enabled());
        assert!(matches!(
            control.rearm(token),
            Err(SdioIrqControlError::SourceNotMasked { bitmap: 1 })
        ));
    }
}
