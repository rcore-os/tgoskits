//! `DwMmc`: register-level driver core for the Synopsys DesignWare
//! Mobile Storage Host Controller.
//!
//! This module owns the register block and implements reset, clock
//! programming, FIFO threshold setup, and bus-width selection. Higher-
//! level command issue lives in [`crate::command`]; FIFO and IDMAC data
//! transfer state machines live in [`crate::dma`]; the [`SdioHost`] wiring
//! lives in [`crate::lib`].
//!
//! [`SdioHost`]: sdmmc_protocol::sdio::SdioHost

use alloc::{boxed::Box, sync::Arc};
use core::{
    num::NonZeroU64,
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering},
};

use dma_api::DeviceDma;
use mmio_api::MmioRaw;
use sdmmc_protocol::{
    error::{Error, ErrorContext, Phase},
    sdio::host::{ClockSpeed, HostIrqSnapshot, SignalVoltage},
};
use volatile::VolatilePtr;

use crate::{
    UhsBits,
    command::CommandState,
    regs::{BlkSiz, CType, RIntSts, RegisterBlock, RegisterBlockVolatileFieldAccess},
    uhs_bits_after_speed, uhs_bits_after_voltage,
};

/// Default FIFO offset used by Rockchip DWC_mobile_storage variants
/// (RK3399, RK356x, RK35xx). Other SoCs may differ — pass a custom
/// offset to [`DwMmc::new_with_fifo_offset`].
pub const DEFAULT_FIFO_OFFSET: usize = 0x200;
const ALL_INT_CLR: u32 = u32::MAX;
const DEFAULT_TMOUT: u32 = u32::MAX;
const DEFAULT_FIFO_DEPTH_WORDS: u32 = 0x100;
const DEFAULT_FIFOTH_MSIZE: u32 = 0x2;
const DEFAULT_FIFOTH: u32 = fifoth(
    DEFAULT_FIFOTH_MSIZE,
    DEFAULT_FIFO_DEPTH_WORDS / 2 - 1,
    DEFAULT_FIFO_DEPTH_WORDS / 2,
);
const fn fifoth(msize: u32, rx_wmark: u32, tx_wmark: u32) -> u32 {
    ((msize & 0x7) << 28) | ((rx_wmark & 0x0fff) << 16) | (tx_wmark & 0x0fff)
}

/// Cached state for a pending data phase.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingData {
    pub direction: sdmmc_protocol::DataDirection,
    pub block_size: u32,
    pub block_count: u32,
}

/// DesignWare Mobile Storage Host Controller backend.
///
/// Implements [`sdmmc_protocol::sdio::SdioHost`] using either the
/// controller FIFO or the internal DMAC (IDMAC) state machine.
///
/// # Safety
///
/// [`DwMmc::new`] is `unsafe` because the caller must hand over a
/// valid, exclusively-owned MMIO base for a DW_mshc-compatible
/// register block. Concurrent access to the same controller from
/// multiple `DwMmc` instances is undefined.
const IRQ_GENERATION_SHIFT: u64 = 32;
const IRQ_STATUS_MASK: u64 = u32::MAX as u64;
const CAPTURE_ENDPOINT_HOLDER: u8 = 1 << 0;
const SOURCE_CONTROL_HOLDER: u8 = 1 << 1;
const ALL_SOURCE_HOLDERS: u8 = CAPTURE_ENDPOINT_HOLDER | SOURCE_CONTROL_HOLDER;

pub(crate) struct IrqState {
    mailbox: AtomicU64,
    idmac_mailbox: AtomicU64,
    next_generation: AtomicU32,
    source_taken: AtomicBool,
    source_holders: AtomicU8,
    source_online: AtomicBool,
    source_generation: AtomicU64,
    desired_sources: AtomicU32,
    masked_sources: AtomicU64,
}

impl IrqState {
    pub(crate) const fn new() -> Self {
        Self {
            mailbox: AtomicU64::new(0),
            idmac_mailbox: AtomicU64::new(0),
            next_generation: AtomicU32::new(0),
            source_taken: AtomicBool::new(false),
            source_holders: AtomicU8::new(0),
            source_online: AtomicBool::new(false),
            source_generation: AtomicU64::new(0),
            desired_sources: AtomicU32::new(0),
            masked_sources: AtomicU64::new(0),
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

    pub(crate) fn activate_source(&self, desired_sources: u32) -> NonZeroU64 {
        debug_assert_ne!(desired_sources, 0);
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
                Ok(_) => break NonZeroU64::new(next).expect("DWMMC source epoch is nonzero"),
                Err(observed) => current = observed,
            }
        };
        self.desired_sources
            .store(desired_sources, Ordering::Release);
        self.masked_sources.store(0, Ordering::Release);
        self.source_online.store(true, Ordering::Release);
        generation
    }

    pub(crate) fn deactivate_source(&self) {
        self.source_online.store(false, Ordering::Release);
        self.desired_sources.store(0, Ordering::Release);
        self.masked_sources.store(0, Ordering::Release);
    }

    pub(crate) fn source_generation(&self) -> Option<NonZeroU64> {
        NonZeroU64::new(self.source_generation.load(Ordering::Acquire))
    }

    pub(crate) fn source_online(&self) -> bool {
        self.source_online.load(Ordering::Acquire)
    }

    pub(crate) fn desired_sources(&self) -> u32 {
        self.desired_sources.load(Ordering::Acquire)
    }

    pub(crate) fn set_desired_sources(&self, sources: u32) {
        self.desired_sources.store(sources, Ordering::Release);
    }

    pub(crate) fn mark_sources_masked(&self, bitmap: u64) {
        debug_assert_ne!(bitmap, 0);
        self.masked_sources.fetch_or(bitmap, Ordering::Release);
    }

    pub(crate) fn claim_masked_sources(&self, bitmap: u64) -> bool {
        let mut current = self.masked_sources.load(Ordering::Acquire);
        loop {
            if bitmap == 0 || bitmap & !current != 0 {
                return false;
            }
            match self.masked_sources.compare_exchange_weak(
                current,
                current & !bitmap,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(observed) => current = observed,
            }
        }
    }

    pub(crate) fn begin_request(&self) {
        let generation = self.next_generation();
        self.mailbox
            .store(pack_mailbox(generation, 0), Ordering::Release);
        self.idmac_mailbox
            .store(pack_mailbox(generation, 0), Ordering::Release);
    }

    pub(crate) fn end_request(&self) {
        self.mailbox.store(0, Ordering::Release);
        self.idmac_mailbox.store(0, Ordering::Release);
    }

    pub(crate) fn cache_if_current(&self, generation: u32, status: u32) {
        cache_mailbox_if_current(&self.mailbox, generation, status);
    }

    pub(crate) fn cache_idmac_if_current(&self, generation: u32, status: u32) {
        cache_mailbox_if_current(&self.idmac_mailbox, generation, status);
    }

    pub(crate) fn generation(&self) -> u32 {
        mailbox_generation(self.mailbox.load(Ordering::Acquire))
    }

    pub(crate) fn take(&self, mask: u32) -> u32 {
        take_mailbox_status(&self.mailbox, mask)
    }

    pub(crate) fn take_idmac(&self, mask: u32) -> u32 {
        take_mailbox_status(&self.idmac_mailbox, mask)
    }

    pub(crate) fn clear(&self, mask: u32) {
        clear_mailbox_status(&self.mailbox, mask);
    }

    pub(crate) fn clear_idmac(&self, mask: u32) {
        clear_mailbox_status(&self.idmac_mailbox, mask);
    }

    pub(crate) fn clear_all(&self) {
        self.clear(u32::MAX);
        self.clear_idmac(u32::MAX);
    }

    pub(crate) fn pending(&self) -> u32 {
        mailbox_status(self.mailbox.load(Ordering::Acquire))
    }

    pub(crate) fn pending_idmac(&self) -> u32 {
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
            "DWMMC IRQ source capability released more than once"
        );
        if previous == holder {
            // Drop retires only a synchronized software capability. It never
            // masks, rearms, acknowledges, or otherwise advances hardware.
            self.source_taken.store(false, Ordering::Release);
        }
    }
}

fn cache_mailbox_if_current(mailbox: &AtomicU64, generation: u32, status: u32) {
    if generation == 0 || status == 0 {
        return;
    }
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

fn take_mailbox_status(mailbox: &AtomicU64, mask: u32) -> u32 {
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

fn clear_mailbox_status(mailbox: &AtomicU64, mask: u32) {
    let mut cur = mailbox.load(Ordering::Acquire);
    loop {
        let next = pack_mailbox(mailbox_generation(cur), mailbox_status(cur) & !mask);
        match mailbox.compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return,
            Err(observed) => cur = observed,
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

pub(crate) struct IrqCore {
    pub(crate) regs: VolatilePtr<'static, RegisterBlock>,
    pub(crate) state: IrqState,
}

// SAFETY: OS glue moves the endpoint into the local IRQ action and retains the
// host/control endpoint in the same CPU-pinned maintenance domain. Task-side
// register transitions and source rearm exclude that local action; destructive
// status is published through the atomic mailboxes. The MMIO mapping outlives
// the host and both split IRQ capabilities.
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

pub struct DwMmc {
    pub(crate) regs: VolatilePtr<'static, RegisterBlock>,
    pub(crate) base_addr: usize,
    pub(crate) fifo_offset: usize,
    pub(crate) ref_clock_hz: u32,
    pub(crate) card_detect: CardDetect,
    pub(crate) ext_clock: Option<Box<dyn HostClock>>,
    pub(crate) pending_data: Option<PendingData>,
    pub(crate) command_state: CommandState,
    /// Task-owned remainder of the exact v0.13 controller snapshot.
    pub(crate) evidence_status: u32,
    /// Task-owned remainder of the exact v0.13 IDMAC snapshot.
    pub(crate) evidence_idmac_status: u32,
    pub(crate) evidence_irq: bool,
    pub(crate) data_blocks_remaining: u32,
    pub(crate) data_cmd_index: u8,
    pub(crate) dma: Option<DeviceDma>,
    pub(crate) dma_mask: u64,
    pub(crate) dma_poisoned: bool,
    /// True only after the lifecycle FSM has observed controller/FIFO/DMA
    /// reset completion with IRQ delivery drained.
    pub(crate) recovery_quiesced: bool,
    pub(crate) irq: Arc<IrqCore>,
    pub(crate) completion_irq_enabled: AtomicBool,
    pub(crate) host2_next_id: u64,
    pub(crate) host2_active_id: Option<u64>,
}

impl DwMmc {
    /// Construct a `DwMmc` over an already-mapped MMIO register file, using the default
    /// FIFO offset (`0x200`).
    ///
    /// # Safety
    ///
    /// `base` must point to a memory-mapped DW_mshc register file
    /// the caller has exclusive access to.
    pub unsafe fn new(base: NonNull<u8>) -> Self {
        unsafe { Self::new_with_fifo_offset(base, DEFAULT_FIFO_OFFSET) }
    }

    /// Construct a `DwMmc` with an explicit FIFO offset.
    ///
    /// Use this when porting to an SoC whose FIFO sits at a different
    /// offset than the default `0x200` (e.g. older Allwinner variants
    /// at `0x100`).
    ///
    /// # Safety
    ///
    /// Same contract as [`DwMmc::new`]; `fifo_offset` must match the
    /// hardware.
    pub unsafe fn new_with_fifo_offset(base: NonNull<u8>, fifo_offset: usize) -> Self {
        let regs = unsafe { VolatilePtr::new(base.cast()) };
        // Discovery may inspect an already-running firmware controller. Mask
        // delivery without acknowledging status or issuing reset/clock/card
        // commands; the staged initializer owns those later transitions.
        regs.intmask().write(0);
        regs.ctrl().update(|control| control.with_int_enable(false));
        Self {
            regs,
            base_addr: base.as_ptr() as usize,
            fifo_offset,
            ref_clock_hz: 0,
            card_detect: CardDetect::ControllerActiveLow,
            ext_clock: None,
            pending_data: None,
            command_state: CommandState::Idle,
            evidence_status: 0,
            evidence_idmac_status: 0,
            evidence_irq: false,
            data_blocks_remaining: 0,
            data_cmd_index: 0,
            dma: None,
            dma_mask: u32::MAX as u64,
            dma_poisoned: false,
            recovery_quiesced: false,
            irq: Arc::new(IrqCore::new(regs)),
            completion_irq_enabled: AtomicBool::new(false),
            host2_next_id: 0,
            host2_active_id: None,
        }
    }

    /// Construct a `DwMmc` over an already-mapped MMIO capability.
    ///
    /// The OS/platform glue still owns mapping lifetime; this helper keeps the
    /// portable driver boundary typed as `mmio-api` instead of a raw address.
    ///
    /// # Safety
    ///
    /// `mmio` must cover a valid, exclusively-owned DW_mshc register file.
    pub unsafe fn new_from_mmio_raw(mmio: &MmioRaw) -> Self {
        unsafe { Self::new(mmio.as_nonnull_ptr()) }
    }

    /// Construct a `DwMmc` over an already-mapped MMIO capability and explicit
    /// FIFO offset.
    ///
    /// # Safety
    ///
    /// Same contract as [`DwMmc::new_from_mmio_raw`]; `fifo_offset` must match
    /// the hardware integration.
    pub unsafe fn new_from_mmio_raw_with_fifo_offset(mmio: &MmioRaw, fifo_offset: usize) -> Self {
        unsafe { Self::new_with_fifo_offset(mmio.as_nonnull_ptr(), fifo_offset) }
    }

    /// Construct a `DwMmc` from a raw mapped MMIO address.
    ///
    /// Prefer [`DwMmc::new`] when OS glue already tracks the mapping as a
    /// non-null pointer. This helper keeps legacy bring-up code explicit
    /// about where the raw address crosses into the portable driver core.
    ///
    /// # Safety
    ///
    /// `base_addr` must be non-zero and point to a memory-mapped DW_mshc
    /// register file that the caller has exclusive access to.
    pub unsafe fn new_from_addr(base_addr: usize) -> Self {
        let base = NonNull::new(base_addr as *mut u8).expect("MMIO base address must be non-null");
        unsafe { Self::new(base) }
    }

    /// Construct a `DwMmc` from a raw mapped MMIO address and explicit FIFO offset.
    ///
    /// # Safety
    ///
    /// Same contract as [`DwMmc::new_from_addr`]; `fifo_offset` must match the
    /// hardware.
    pub unsafe fn new_from_addr_with_fifo_offset(base_addr: usize, fifo_offset: usize) -> Self {
        let base = NonNull::new(base_addr as *mut u8).expect("MMIO base address must be non-null");
        unsafe { Self::new_with_fifo_offset(base, fifo_offset) }
    }

    /// Tell the driver the reference clock fed to the controller, in Hz.
    ///
    /// The clock divider in [`set_clock`](sdmmc_protocol::sdio::SdioHost::set_clock)
    /// is computed from this value: `divider = ceil(ref_clock_hz /
    /// (2 * target_hz))`. If the reference is left at `0` the driver
    /// falls back to a 1:1 passthrough (CLKDIV = 0) and assumes the
    /// platform CRU is doing all the rate scaling.
    pub fn set_reference_clock(&mut self, ref_clock_hz: u32) {
        self.ref_clock_hz = ref_clock_hz;
    }

    /// Current controller reference clock used by the DWMMC divider logic.
    pub fn reference_clock(&self) -> u32 {
        self.ref_clock_hz
    }

    /// Configure how the host interprets its card-detect input.
    ///
    /// The DesignWare controller's onboard CDETECT bit follows the Linux
    /// `dw_mci_get_cd()` convention for removable slots: bit 0 clear means
    /// card present, bit 0 set means no card.
    pub fn set_card_detect(&mut self, detect: CardDetect) {
        self.card_detect = detect;
    }

    /// Return whether slot 0 currently reports a card present.
    pub fn card_present(&self) -> bool {
        match self.card_detect {
            CardDetect::ControllerActiveLow => self.regs.cdetect().read() & 1 == 0,
            CardDetect::ControllerActiveHigh => self.regs.cdetect().read() & 1 != 0,
            CardDetect::AlwaysPresent => true,
        }
    }

    /// Install a platform clock callback for DWMMC integrations where the
    /// controller input clock is controlled outside the DW_mshc register file.
    ///
    /// Rockchip DWMMC follows the Linux `dw_mci_rk3288_set_ios()` model: the
    /// platform `ciu` clock is retuned on each bus-speed change and the
    /// controller divider is then programmed relative to the effective bus
    /// clock.
    pub fn set_external_clock<C>(&mut self, clock: C)
    where
        C: HostClock + 'static,
    {
        self.ext_clock = Some(Box::new(clock));
    }

    /// Remove a previously installed platform clock callback.
    pub fn clear_external_clock(&mut self) {
        self.ext_clock = None;
    }

    /// Install a DMA capability used by high-level data-transfer hooks.
    ///
    /// Once installed, `SdioHost::submit_read_data` and
    /// `SdioHost::submit_write_data` try the internal IDMAC first for
    /// 512-byte block I/O and fall back to the FIFO state machine if it cannot
    /// be used.
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

    /// Clear every bit in RINTSTS by writing it back (write-1-to-clear).
    pub(crate) fn clear_all_int_status(&self) {
        self.regs.rintsts().write(RIntSts::from_bits(ALL_INT_CLR));
    }

    /// Discard IDMAC causes while the controller IRQ action is masked and
    /// synchronized during initialization or recovery.
    pub(crate) fn clear_all_idmac_status(&self) {
        self.regs
            .idsts()
            .write(crate::event::DWMMC_IDMAC_INT_ENABLE_MASK);
    }

    pub(crate) fn take_task_irq_status(&mut self, mask: u32) -> u32 {
        // The IRQ endpoint is the sole owner of destructive RINTSTS reads and
        // W1C acknowledgement. An empty mailbox means no acknowledged event,
        // never permission to inspect hardware from task context.
        if self.evidence_irq {
            let status = self.evidence_status & mask;
            self.evidence_status &= !mask;
            status
        } else {
            self.irq.state.take(mask)
        }
    }

    pub(crate) fn take_task_idmac_status(&mut self, mask: u32) -> u32 {
        if self.evidence_irq {
            let status = self.evidence_idmac_status & mask;
            self.evidence_idmac_status &= !mask;
            status
        } else {
            self.irq.state.take_idmac(mask)
        }
    }

    pub(crate) fn install_evidence_snapshot(
        &mut self,
        snapshot: HostIrqSnapshot,
    ) -> Result<(), Error> {
        if !self.evidence_irq {
            return Err(Error::InvalidArgument);
        }
        self.evidence_status |= snapshot.stable_status;
        self.evidence_idmac_status |= snapshot.dma_status;
        Ok(())
    }

    pub(crate) fn begin_request_irq_epoch(&mut self) {
        self.evidence_status = 0;
        self.evidence_idmac_status = 0;
        self.irq.state.begin_request();
    }

    pub(crate) fn end_request_irq_epoch(&mut self) {
        self.evidence_status = 0;
        self.evidence_idmac_status = 0;
        self.irq.state.end_request();
    }

    pub(crate) fn clear_task_irq_evidence(&mut self) {
        self.evidence_status = 0;
        self.evidence_idmac_status = 0;
        self.irq.state.clear_all();
    }

    pub(crate) fn program_linux_init_baseline(&self) {
        self.regs.tmout().write(DEFAULT_TMOUT);
        self.regs.fifoth().write(DEFAULT_FIFOTH);
        self.regs.clksrc().write(0);
    }

    /// Unmasks runtime command/data delivery after OS glue has registered the
    /// unique capture endpoint on this maintenance owner's CPU.
    pub(crate) fn enable_completion_irq(&mut self) {
        let sources = crate::DWMMC_INT_DATA_TRANSFER_OVER
            | crate::DWMMC_INT_COMMAND_DONE
            | crate::DWMMC_INT_ERROR_MASK;
        let _ = self.irq.state.activate_source(sources);
        self.completion_irq_enabled.store(true, Ordering::Release);
        self.regs.intmask().write(sources);
        self.regs.ctrl().update(|r| r.with_int_enable(true));
    }

    /// Programs the FIFO-ready sources before an owner-thread FIFO request.
    ///
    /// The maintenance runtime must exclude this controller's local IRQ
    /// action while invoking task-side register transitions. Once capture
    /// masks RXDR/TXDR, only [`crate::DwMmcIrqControl`] may rearm the token.
    pub(crate) fn program_fifo_interrupt_mask(&self) {
        if self.completion_irq_enabled() {
            let sources = crate::DWMMC_INT_DATA_TRANSFER_OVER
                | crate::DWMMC_INT_COMMAND_DONE
                | crate::DWMMC_INT_RXDR
                | crate::DWMMC_INT_TXDR
                | crate::DWMMC_INT_ERROR_MASK;
            self.irq.state.set_desired_sources(sources);
            self.regs.intmask().write(sources);
        }
    }

    pub(crate) fn disable_completion_irq(&mut self) {
        self.completion_irq_enabled.store(false, Ordering::Release);
        self.regs.intmask().write(0);
        self.regs.ctrl().update(|r| r.with_int_enable(false));
        self.irq.state.deactivate_source();
    }

    pub fn completion_irq_enabled(&self) -> bool {
        self.completion_irq_enabled.load(Ordering::Acquire)
    }

    /// Set bus width. DW_mshc encodes width in CTYPE: bit 0 of `width4`
    /// = 4-bit, bit 0 of `width8` = 8-bit; both clear = 1-bit.
    pub(crate) fn set_card_type(&mut self, width: sdmmc_protocol::sdio::host::BusWidth) {
        use sdmmc_protocol::sdio::host::BusWidth;
        let ct = match width {
            BusWidth::Bit1 => CType::new(),
            BusWidth::Bit4 => CType::new().with_width4(1),
            BusWidth::Bit8 => CType::new().with_width8(1),
            // Future BusWidth variants: fall back to 1-bit (no width bits set).
            _ => CType::new(),
        };
        self.regs.ctype().write(ct);
    }

    /// Program DW_mshc UHS timing bits for card 0. The generic DW_mshc
    /// UHS register exposes DDR and signaling-voltage selectors; SoC-specific
    /// sample/drive delay lines remain platform glue responsibility.
    pub(crate) fn set_uhs_timing(&mut self, speed: ClockSpeed) {
        let cur = self.uhs_bits();
        self.write_uhs_bits(uhs_bits_after_speed(cur, speed));
    }

    /// Program DW_mshc signaling-voltage bit for card 0.
    pub(crate) fn set_signal_voltage(&mut self, voltage: SignalVoltage) -> Result<(), Error> {
        let cur = self.uhs_bits();
        self.write_uhs_bits(uhs_bits_after_voltage(cur, voltage)?);
        Ok(())
    }

    fn uhs_bits(&self) -> UhsBits {
        let uhs = self.regs.uhs().read();
        UhsBits {
            ddr: uhs.ddr(),
            volt: uhs.volt(),
        }
    }

    fn write_uhs_bits(&self, bits: UhsBits) {
        self.regs.uhs().write(
            crate::regs::UHS::new()
                .with_ddr(bits.ddr)
                .with_volt(bits.volt),
        );
    }

    /// Program block size + total byte count for the next data phase.
    pub(crate) fn program_data_phase(&self, block_size: u32, block_count: u32) {
        self.regs
            .blksiz()
            .write(BlkSiz::new().with_block_size(block_size as u16));
        self.regs.bytcnt().write(block_size * block_count);
    }

    /// Translate a non-zero `RIntSts.error()` into our protocol error
    /// type. `phase` and `cmd_index` give the caller's pipeline
    /// context.
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

    /// Raw pointer at `base + fifo_offset`, used for FIFO data accesses.
    pub(crate) fn fifo_ptr(&self) -> *mut u32 {
        (self.base_addr + self.fifo_offset) as *mut u32
    }
}

unsafe impl Send for DwMmc {}

/// Platform clock capability for DWMMC hosts with a SoC-side CIU clock.
pub trait HostClock: Send {
    /// Retune the platform clock for a requested SD/MMC bus clock and return
    /// the effective controller bus clock used by the DWMMC divider logic.
    fn set_clock(&self, target_hz: u32) -> Result<u32, Error>;
}

/// Card-detect policy for DWMMC slot 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardDetect {
    /// Controller CDETECT bit is active-low: 0 means present, 1 means absent.
    ControllerActiveLow,
    /// Controller CDETECT bit is active-high: 1 means present, 0 means absent.
    ControllerActiveHigh,
    /// Treat the card as fixed/non-removable.
    AlwaysPresent,
}

#[cfg(test)]
mod tests {
    use core::ptr::NonNull;

    use super::*;

    #[test]
    fn constructs_from_mapped_mmio_pointer() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let host = unsafe { DwMmc::new(base) };

        assert_eq!(host.base_addr, base.as_ptr() as usize);
    }

    #[test]
    fn discovery_masks_device_irq_without_issuing_a_command() {
        const CTRL_WORD: usize = 0;
        const INTMASK_WORD: usize = 9;
        const CMD_WORD: usize = 11;
        let mut mmio = [0u32; 256];
        mmio[CTRL_WORD] = crate::regs::Ctrl::new().with_int_enable(true).into_bits();
        mmio[INTMASK_WORD] = u32::MAX;
        mmio[CMD_WORD] = 0x5a5a_a5a5;
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();

        let host = unsafe { DwMmc::new(base) };

        assert_eq!(mmio[INTMASK_WORD], 0);
        assert!(!crate::regs::Ctrl::from_bits(mmio[CTRL_WORD]).int_enable());
        assert_eq!(mmio[CMD_WORD], 0x5a5a_a5a5);
        assert!(!host.completion_irq_enabled());
    }

    #[test]
    fn legacy_addr_constructor_keeps_raw_mmio_boundary_explicit() {
        let mut mmio = [0u32; 256];
        let base_addr = mmio.as_mut_ptr() as usize;
        let host = unsafe { DwMmc::new_from_addr(base_addr) };

        assert_eq!(host.base_addr, base_addr);
    }

    #[test]
    fn external_clock_can_be_scoped_and_cleared() {
        struct Clock;

        impl HostClock for Clock {
            fn set_clock(&self, target_hz: u32) -> Result<u32, Error> {
                Ok(target_hz)
            }
        }

        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };

        host.set_external_clock(Clock);
        assert!(host.ext_clock.is_some());

        host.clear_external_clock();
        assert!(host.ext_clock.is_none());
    }

    #[test]
    fn controller_card_detect_defaults_to_linux_active_low() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        const CDETECT_WORD: usize = 20;

        unsafe {
            mmio.as_mut_ptr().add(CDETECT_WORD).write_volatile(0);
        }
        assert!(host.card_present());

        unsafe {
            mmio.as_mut_ptr().add(CDETECT_WORD).write_volatile(1);
        }
        assert!(!host.card_present());

        host.set_card_detect(CardDetect::AlwaysPresent);
        assert!(host.card_present());
    }
}
