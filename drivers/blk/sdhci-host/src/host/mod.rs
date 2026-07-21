//! `Sdhci` core: MMIO accessors, reset, clock and bus-width setup.

use alloc::{boxed::Box, sync::Arc};
use core::{num::NonZeroU32, ptr::NonNull};

use dma_api::DeviceDma;
use mmio_api::MmioRaw;
use sdmmc_protocol::error::{Error, ErrorContext, Phase};

use crate::{command::CommandState, regs::*};

/// Cached state for a single pending data phase, populated by the
/// data-command submit path and consumed when that path issues the command.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingData {
    pub direction: sdmmc_protocol::DataDirection,
    pub block_size: u32,
    pub block_count: u32,
    /// ADMA descriptor address published only when the command is issued.
    pub adma_descriptor: Option<u32>,
}

mod irq;
mod register_io;

pub(crate) use irq::{IrqCore, IrqSnapshot};
use register_io::{Aligned32RegisterFile, MmioWords, WordIo};

pub(crate) use crate::SDHCI_IRQ_SOURCE_BITMAP;

/// Broadcom SDHCI integration selected by firmware compatibility.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BroadcomController {
    /// BCM2835 eMMC host: missing capabilities, no 1.8 V, no usable card
    /// detect, and no writable high-speed control bit.
    Bcm2835,
    /// BCM2711 eMMC2 host: native capabilities with the 32-bit register port.
    Bcm2711,
}

enum RegisterAccess {
    Native,
    Aligned32(Aligned32RegisterFile),
}

/// Generic SD Host Controller (SDHCI) backend.
///
/// Owns the MMIO base address of one host controller instance and
/// implements [`sdmmc_protocol::sdio::SdioHost`] so that the protocol
/// driver in `sdmmc-protocol` can drive it. Data transfers can use either
/// the controller FIFO or the ADMA2 state machine.
///
/// # Safety
///
/// `new` is `unsafe` because the caller must provide a valid, exclusive
/// MMIO base address for an SDHCI v3.x compatible controller. The mapping
/// must remain valid until this object and both capabilities transferred by
/// [`Sdhci::take_irq_source`] have been retired after IRQ synchronization.
/// Concurrent use of the same controller from multiple `Sdhci` instances is
/// undefined.
pub struct Sdhci {
    pub(crate) base_addr: usize,
    register_access: RegisterAccess,
    broadcom_controller: Option<BroadcomController>,
    pub(crate) bus_clock_hz: u32,
    pub(crate) command_state: CommandState,
    pub(crate) pending_data: Option<PendingData>,
    /// When set, command submission programs the controller's transfer mode
    /// register with `DMA_ENABLE`. Set by the ADMA2 wrapper just before it
    /// fires off a command; default `false` keeps the FIFO path active.
    pub(crate) use_dma: bool,
    /// Optional CRU-side clock callback. When set, the `SdioHost::set_clock`
    /// impl will route requests to this hook (and program the controller
    /// for 1:1 passthrough) instead of using the internal 10-bit divider.
    /// Used on controllers whose internal divider is unusable.
    pub(crate) ext_clock: Option<Box<dyn HostClock>>,
    /// Optional platform hook that runs after a controller-wide reset has
    /// completed and before protocol commands are issued. DWCMSHC-style
    /// integrations use this for vendor PHY/DLL defaults that reset does not
    /// leave in a usable identification-mode state.
    pub(crate) reset_hook: Option<Box<dyn HostResetHook>>,
    /// Optional monotonic timer used by legacy asynchronous bus operations.
    pub(crate) timer: Option<&'static dyn HostTimer>,
    /// Task-owned remainder of the last complete IRQ snapshot. Keeping this
    /// outside the atomic mailbox lets command completion consume its bit
    /// while preserving a coalesced transfer-complete bit for the next FSM
    /// transition.
    pub(crate) pending_irq: IrqSnapshot,
    /// Selects explicit v0.13 ledger snapshots instead of the compatibility
    /// task-side atomic mailbox.
    pub(crate) evidence_irq: bool,
    /// Whether the platform has wired up the IO-domain regulator needed to
    /// actually run the bus at 1.8 V. Default `false` — toggling
    /// `HOST_CONTROL2.1V8_SIGNALING_ENABLE` alone changes the controller
    /// sampling behaviour without changing the IO rail, which corrupts
    /// subsequent transfers; refusing the switch lets the protocol layer
    /// fall back to a 3.3 V-compatible mode.
    pub(crate) support_1v8: bool,
    pub(crate) base_clock_override_hz: Option<NonZeroU32>,
    /// Command index for the data phase currently being drained by the
    /// submit/poll data-command state machine.
    pub(crate) active_data_cmd: u8,
    pub(crate) dma: Option<DeviceDma>,
    pub(crate) dma_mask: u64,
    pub(crate) dma_poisoned: bool,
    /// Set only after the recovery FSM observes RESET_ALL deasserted while
    /// device IRQ delivery and its OS action are drained. It lets proof-gated
    /// request reclamation return DMA ownership without running another
    /// synchronous reset sequence.
    pub(crate) recovery_quiesced: bool,
    pub(crate) irq: Arc<IrqCore>,
    pub(crate) host2_next_id: u64,
    pub(crate) host2_active_id: Option<u64>,
}

impl Sdhci {
    /// Construct a new Sdhci over an already-mapped MMIO register file.
    ///
    /// # Safety
    ///
    /// `base` must point to a memory-mapped SDHCI v3.x register file that the
    /// caller has exclusive access to. The mapping must outlive the host and
    /// any split IRQ capabilities transferred from it.
    pub unsafe fn new(base: NonNull<u8>) -> Self {
        unsafe { Self::new_with_access(base, RegisterAccess::Native, None) }
    }

    /// Construct a Broadcom iProc-compatible SDHCI host.
    ///
    /// Both supported variants expose only aligned 32-bit register accesses.
    /// Their capability differences remain explicit through `controller`.
    ///
    /// # Safety
    ///
    /// `base` must cover an exclusively owned register mapping for the
    /// selected controller.
    pub unsafe fn new_broadcom(base: NonNull<u8>, controller: BroadcomController) -> Self {
        unsafe {
            Self::new_with_access(
                base,
                RegisterAccess::Aligned32(Aligned32RegisterFile::new()),
                Some(controller),
            )
        }
    }

    unsafe fn new_with_access(
        base: NonNull<u8>,
        register_access: RegisterAccess,
        broadcom_controller: Option<BroadcomController>,
    ) -> Self {
        let aligned_32bit = matches!(register_access, RegisterAccess::Aligned32(_));
        Self {
            base_addr: base.as_ptr() as usize,
            register_access,
            broadcom_controller,
            bus_clock_hz: 0,
            command_state: CommandState::Idle,
            pending_data: None,
            use_dma: false,
            ext_clock: None,
            reset_hook: None,
            timer: None,
            pending_irq: IrqSnapshot::empty(),
            evidence_irq: false,
            support_1v8: false,
            base_clock_override_hz: None,
            active_data_cmd: 0,
            dma: None,
            dma_mask: u32::MAX as u64,
            dma_poisoned: false,
            recovery_quiesced: false,
            irq: Arc::new(IrqCore::new(base.as_ptr() as usize, aligned_32bit)),
            host2_next_id: 0,
            host2_active_id: None,
        }
    }

    /// Construct a new Sdhci over an already-mapped MMIO capability.
    ///
    /// The OS/platform glue still owns mapping lifetime; this helper keeps the
    /// portable driver boundary typed as `mmio-api` instead of a raw address.
    ///
    /// # Safety
    ///
    /// `mmio` must cover a valid, exclusively-owned SDHCI v3.x register file.
    pub unsafe fn new_from_mmio_raw(mmio: &MmioRaw) -> Self {
        unsafe { Self::new(mmio.as_nonnull_ptr()) }
    }

    /// Construct a new Sdhci from a raw mapped MMIO address.
    ///
    /// Prefer [`Sdhci::new`] when OS glue already tracks the mapping as a
    /// non-null pointer. This helper keeps legacy bring-up code explicit
    /// about where the raw address crosses into the portable driver core.
    ///
    /// # Safety
    ///
    /// `base_addr` must be non-zero and point to a memory-mapped SDHCI v3.x
    /// register file that the caller has exclusive access to.
    pub unsafe fn new_from_addr(base_addr: usize) -> Self {
        let base = NonNull::new(base_addr as *mut u8).expect("MMIO base address must be non-null");
        unsafe { Self::new(base) }
    }

    /// Return the mapped MMIO base address owned by this driver instance.
    pub fn mmio_base(&self) -> usize {
        self.base_addr
    }

    /// Returns the effective SD bus clock proven stable by the latest
    /// completed clock state transition.
    ///
    /// External-clock integrations report the platform hook's effective
    /// (possibly quantized) rate rather than the protocol's requested rate.
    /// Zero means no running clock has been proven yet.
    pub const fn active_bus_clock_hz(&self) -> u32 {
        self.bus_clock_hz
    }

    /// Install a CRU-side clock callback so subsequent `set_clock` calls
    /// retune the platform's reference clock instead of using the SDHCI
    /// internal divider. The callback receives the desired SD bus
    /// frequency in Hz; on success it must guarantee the controller's
    /// input reference clock equals that value before returning.
    ///
    /// After installing the callback, the host runs in "external clock"
    /// mode: the SDHCI internal divider stays at 1:1, all rate control
    /// is delegated to the platform.
    pub fn set_external_clock<C>(&mut self, clock: C)
    where
        C: HostClock + 'static,
    {
        self.ext_clock = Some(Box::new(clock));
    }

    /// Remove the platform clock callback once the caller no longer wants
    /// the host to borrow the probe-time clock device.
    pub fn clear_external_clock(&mut self) {
        self.ext_clock = None;
    }

    /// Install a platform post-reset hook for the native absolute-time
    /// `sdio-host2` bus-operation state machine.
    pub fn set_reset_hook<H>(&mut self, hook: H)
    where
        H: HostResetHook + 'static,
    {
        self.reset_hook = Some(Box::new(hook));
    }

    pub(crate) fn begin_before_reset_all_hook(
        &mut self,
        now_ns: u64,
    ) -> Result<ResetHookPoll, Error> {
        let Some(mut hook) = self.reset_hook.take() else {
            return Ok(ResetHookPoll::Ready);
        };
        let mode = hook.recovery_mode();
        let progress = match mode {
            ResetHookRecoveryMode::Unsupported => Err(Error::UnsupportedCommand),
            ResetHookRecoveryMode::BoundedCallbacks => {
                hook.before_reset_all(self).map(|()| ResetHookPoll::Ready)
            }
            ResetHookRecoveryMode::Scheduled => hook.begin_before_reset_all(self, now_ns),
        };
        let mut result = validate_reset_hook_poll(progress, now_ns);
        if result.is_err()
            && mode == ResetHookRecoveryMode::Scheduled
            && let Err(cancel_error) = hook.cancel_before_reset_all(self)
        {
            result = Err(cancel_error);
        }
        self.reset_hook = Some(hook);
        result
    }

    pub(crate) fn poll_before_reset_all_hook(
        &mut self,
        now_ns: u64,
    ) -> Result<ResetHookPoll, Error> {
        let Some(mut hook) = self.reset_hook.take() else {
            return Err(Error::InvalidArgument);
        };
        let mode = hook.recovery_mode();
        let progress = if mode == ResetHookRecoveryMode::Scheduled {
            hook.poll_before_reset_all(self, now_ns)
        } else {
            Err(Error::InvalidArgument)
        };
        let mut result = validate_reset_hook_poll(progress, now_ns);
        if result.is_err()
            && mode == ResetHookRecoveryMode::Scheduled
            && let Err(cancel_error) = hook.cancel_before_reset_all(self)
        {
            result = Err(cancel_error);
        }
        self.reset_hook = Some(hook);
        result
    }

    pub(crate) fn cancel_before_reset_all_hook(&mut self) -> Result<(), Error> {
        let Some(mut hook) = self.reset_hook.take() else {
            return Ok(());
        };
        let result = if hook.recovery_mode() == ResetHookRecoveryMode::Scheduled {
            hook.cancel_before_reset_all(self)
        } else {
            Ok(())
        };
        self.reset_hook = Some(hook);
        result
    }

    pub(crate) fn call_after_reset_hook(&mut self) -> Result<(), Error> {
        if self
            .reset_hook
            .as_ref()
            .is_some_and(|hook| hook.recovery_mode() == ResetHookRecoveryMode::Unsupported)
        {
            return Err(Error::UnsupportedCommand);
        }
        let Some(hook) = self.reset_hook.take() else {
            return Ok(());
        };
        let result = hook.after_reset(self);
        self.reset_hook = Some(hook);
        result
    }

    /// Install a platform monotonic timer in milliseconds.
    pub fn set_timer<T>(&mut self, timer: &'static T)
    where
        T: HostTimer + 'static,
    {
        self.timer = Some(timer);
    }

    /// Declare that the platform can switch the SD/eMMC IO rail to 1.8 V.
    ///
    /// Until this is called, [`SdioHost::switch_voltage`] refuses
    /// [`SignalVoltage::V180`], which steers the protocol layer away from
    /// UHS-I / HS200 / HS400. Platforms that wire up the regulator (PMIC
    /// or per-domain LDO) should call this after construction so that
    /// `switch_voltage(V180)` is allowed to drive
    /// `HOST_CONTROL2.1V8_SIGNALING_ENABLE`.
    pub fn enable_1v8_signaling(&mut self) {
        self.support_1v8 = true;
    }

    /// Install a DMA capability used by the high-level data-transfer hooks.
    ///
    /// Once installed, `SdioHost::submit_read_data` and
    /// `SdioHost::submit_write_data` try ADMA2 first for 512-byte block I/O
    /// and fall back to the FIFO state machine if ADMA2 cannot be used.
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

    pub(crate) fn start_passthrough_clock(&mut self, target_hz: u32) {
        self.write_u16(REG_CLOCK_CONTROL, 0);
        if target_hz != 0 {
            self.write_u16(REG_CLOCK_CONTROL, CLOCK_INTERNAL_ENABLE);
        }
    }

    /// Disable the SD clock without reprogramming the divider. Use this
    /// before reprogramming the external (CRU) clock so glitches don't
    /// reach the card.
    pub fn disable_sd_clock(&mut self) {
        let cur = self.read_u16(REG_CLOCK_CONTROL);
        self.write_u16(REG_CLOCK_CONTROL, cur & !CLOCK_SD_ENABLE);
    }

    /// Set bus power (e.g. 3.3 V) and the global power-on bit.
    pub fn set_power(&mut self, power_byte: u8) {
        self.write_u8(REG_POWER_CONTROL, power_byte | POWER_ON);
    }

    /// Prepare task-owned interrupt status for initialization or diagnostics.
    ///
    /// This mode permits the state-machine owner to read and acknowledge
    /// status. It cannot be entered after runtime IRQ ownership has been
    /// published; runtime recovery transfers ownership only after OS glue has
    /// masked and synchronized the registered IRQ action.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Busy`] if the IRQ endpoint already owns runtime
    /// completion status.
    pub fn enable_initialization_status(&mut self) -> Result<(), Error> {
        if self.runtime_irq_status_owned() {
            return Err(Error::Busy);
        }
        self.enter_initialization_status_mode();
        Ok(())
    }

    pub(crate) fn enter_initialization_status_mode(&mut self) {
        debug_assert!(
            !self.runtime_irq_status_owned(),
            "runtime IRQ ownership must transfer through the recovery lifecycle"
        );
        self.write_u16(REG_NORMAL_INT_STATUS_ENABLE, NORMAL_INT_CLEAR_ALL);
        self.write_u16(REG_ERROR_INT_STATUS_ENABLE, ERROR_INT_CLEAR_ALL);
        self.write_u16(REG_NORMAL_INT_SIGNAL_ENABLE, 0);
        self.write_u16(REG_ERROR_INT_SIGNAL_ENABLE, 0);
        self.irq.state.set_delivery_enabled(false);
        self.irq.state.enter_initialization_status_mode();
    }

    pub(crate) fn take_recovery_status_ownership(&mut self) {
        debug_assert!(
            !self.completion_irq_enabled(),
            "recovery status ownership requires masked IRQ delivery"
        );
        self.irq.state.enter_initialization_status_mode();
    }

    /// Route command/data-completion and error status to the host CPU IRQ line.
    ///
    /// The public protocol boundary verifies that the unique split IRQ source
    /// has already transferred to OS glue. This register operation remains
    /// crate-private so callers cannot enable delivery before registration.
    pub(crate) fn enable_completion_irq(&mut self) {
        // Publish ownership before unmasking delivery. From this point task
        // context must consume only snapshots acknowledged by the IRQ endpoint.
        self.write_u16(REG_NORMAL_INT_STATUS_ENABLE, NORMAL_INT_CLEAR_ALL);
        self.write_u16(REG_ERROR_INT_STATUS_ENABLE, ERROR_INT_CLEAR_ALL);
        self.irq.state.enter_runtime_irq_status_mode();
        let _ = self.irq.state.activate_source();
        self.write_u16(
            REG_NORMAL_INT_SIGNAL_ENABLE,
            NORMAL_INT_COMPLETION_SIGNAL_MASK,
        );
        self.write_u16(
            REG_ERROR_INT_SIGNAL_ENABLE,
            ERROR_INT_COMPLETION_SIGNAL_MASK,
        );
        self.irq.state.set_delivery_enabled(true);
    }

    /// Mask host CPU IRQ delivery without transferring status ownership.
    ///
    /// The IRQ endpoint remains the only W1C owner. This is suitable for a
    /// short quiesce around reset or detach. Call
    /// [`Self::take_recovery_status_ownership`] only after the OS has
    /// synchronized the IRQ endpoint when deliberately transferring ownership
    /// to an initialization or recovery state machine.
    pub(crate) fn disable_completion_irq(&mut self) {
        self.write_u16(REG_NORMAL_INT_SIGNAL_ENABLE, 0);
        self.write_u16(REG_ERROR_INT_SIGNAL_ENABLE, 0);
        self.irq.state.set_delivery_enabled(false);
        self.irq.state.deactivate_source();
    }

    pub fn completion_irq_enabled(&self) -> bool {
        self.irq.state.delivery_enabled()
    }

    pub(crate) fn runtime_irq_status_owned(&self) -> bool {
        self.irq.state.runtime_irq_owned()
    }

    pub(crate) fn initialization_status_owned(&self) -> bool {
        self.irq.state.initialization_owned()
    }

    /// Read the controller's base reference clock from Capabilities (Hz).
    pub fn base_clock_hz(&self) -> u32 {
        if let Some(clock_hz) = self.base_clock_override_hz {
            return clock_hz.get();
        }
        let caps_low = self.read_u32(REG_CAPABILITIES_LOW);
        // SDHCI v3: bits 15..8 contain "Base Clock Frequency" in MHz.
        // SDHCI v2: bits 13..8 contain it. Use the wider mask; QEMU
        // sdhci-pci reports a v2 layout but the result is still right.
        let mhz = (caps_low >> 8) & 0xFF;
        mhz.saturating_mul(1_000_000)
    }

    /// Override a missing or inaccurate SDHCI capability clock.
    pub fn set_base_clock_hz(&mut self, clock_hz: NonZeroU32) {
        self.base_clock_override_hz = Some(clock_hz);
    }

    /// Whether the controller advertises ADMA2 in the capabilities register.
    pub fn supports_adma2(&self) -> bool {
        if matches!(self.broadcom_controller, Some(BroadcomController::Bcm2835)) {
            return false;
        }
        self.read_u32(REG_CAPABILITIES_LOW) & CAPS_LOW_ADMA2_SUPPORTED != 0
    }

    /// Program the ADMA system address registers with the bus address of
    /// the descriptor table. 32-bit ADMA2 only; the high half is zeroed
    /// because controllers that don't implement v4 64-bit addressing
    /// alias the high register to RO-zero anyway.
    pub(crate) fn write_adma_addr(&self, addr: u32) {
        self.write_u32(REG_ADMA_SYS_ADDR_LOW, addr);
        self.write_u32(REG_ADMA_SYS_ADDR_HIGH, 0);
    }

    /// Pick 32-bit ADMA2 in HOST_CONTROL1's DMA select field.
    pub(crate) fn select_adma2_32(&mut self) {
        let mut ctrl = self.read_u8(REG_HOST_CONTROL1);
        ctrl = (ctrl & !HOST_CTRL1_DMA_SEL_MASK) | HOST_CTRL1_DMA_SEL_ADMA2_32;
        self.write_u8(REG_HOST_CONTROL1, ctrl);
    }

    /// Read raw 32-bit response slot.
    pub(crate) fn response32(&self, slot: usize) -> u32 {
        let off = REG_RESPONSE0 + slot * 4;
        self.read_u32(off)
    }

    pub(crate) fn read_u32(&self, off: usize) -> u32 {
        MmioWords::new(self.base_addr).read_u32(off)
    }

    pub(crate) fn write_u32(&self, off: usize, val: u32) {
        MmioWords::new(self.base_addr).write_u32(off, val);
    }

    pub(crate) fn read_u16(&self, off: usize) -> u16 {
        match &self.register_access {
            RegisterAccess::Native => unsafe {
                core::ptr::read_volatile((self.base_addr + off) as *const u16)
            },
            RegisterAccess::Aligned32(registers) => {
                registers.read_u16(&MmioWords::new(self.base_addr), off)
            }
        }
    }

    pub(crate) fn write_u16(&self, off: usize, val: u16) {
        match &self.register_access {
            RegisterAccess::Native => unsafe {
                core::ptr::write_volatile((self.base_addr + off) as *mut u16, val);
            },
            RegisterAccess::Aligned32(registers) => {
                registers.write_u16(&MmioWords::new(self.base_addr), off, val);
            }
        }
    }

    pub(crate) fn read_u8(&self, off: usize) -> u8 {
        match &self.register_access {
            RegisterAccess::Native => unsafe {
                core::ptr::read_volatile((self.base_addr + off) as *const u8)
            },
            RegisterAccess::Aligned32(registers) => {
                registers.read_u8(&MmioWords::new(self.base_addr), off)
            }
        }
    }

    pub(crate) fn write_u8(&self, off: usize, val: u8) {
        match &self.register_access {
            RegisterAccess::Native => unsafe {
                core::ptr::write_volatile((self.base_addr + off) as *mut u8, val);
            },
            RegisterAccess::Aligned32(registers) => {
                registers.write_u8(&MmioWords::new(self.base_addr), off, val);
            }
        }
    }

    pub(crate) fn ack_irq_status(&self, normal: u16, error: u16) {
        match &self.register_access {
            RegisterAccess::Native => {
                if normal != 0 {
                    self.write_u16(REG_NORMAL_INT_STATUS, normal);
                }
                if error != 0 {
                    self.write_u16(REG_ERROR_INT_STATUS, error);
                }
            }
            RegisterAccess::Aligned32(registers) => {
                registers.ack_irq_status(&MmioWords::new(self.base_addr), normal, error);
            }
        }
    }

    pub(crate) fn controls_high_speed_bit(&self) -> bool {
        !matches!(self.broadcom_controller, Some(BroadcomController::Bcm2835))
    }

    pub(crate) fn aligned_32bit_registers(&self) -> bool {
        matches!(self.register_access, RegisterAccess::Aligned32(_))
    }

    pub(crate) fn flush_aligned_block_shadow(&self) -> bool {
        match &self.register_access {
            RegisterAccess::Native => false,
            RegisterAccess::Aligned32(registers) => {
                registers.flush_block_shadow(&MmioWords::new(self.base_addr))
            }
        }
    }
}

/// Platform clock capability for hosts whose controller divider is unusable.
///
/// OS glue implements this boundary and installs it with
/// [`Sdhci::set_external_clock`]. The driver core only knows that the
/// callback retunes the controller input clock to the requested SD bus rate.
pub trait HostClock: Send {
    fn set_clock(&self, target_hz: u32) -> Result<(), Error>;

    /// Effective bus clock to request from the platform for a protocol speed.
    ///
    /// Platforms may quantize requested rates before the clock controller sees
    /// them. RK35xx, for example, uses 375 kHz for identification mode.
    fn effective_clock_hz(&self, target_hz: u32) -> u32 {
        target_hz
    }

    /// Configure host-controller side clock glue after the platform input
    /// clock has been retuned and while SD clock output is still gated off.
    ///
    /// DWCMSHC-style integrations use this for vendor DLL/bypass registers.
    /// Plain SDHCI hosts can rely on the default no-op implementation.
    fn prepare_host_clock(&self, _host: &mut Sdhci, _target_hz: u32) -> Result<(), Error> {
        Ok(())
    }
}

/// Whether a platform reset hook may run inside the bounded recovery FSM.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ResetHookRecoveryMode {
    /// The hook has not proven that both callbacks return without sleeping or
    /// busy-waiting. Runtime recovery must fail closed.
    #[default]
    Unsupported,
    /// Both callbacks perform only bounded register/capability operations.
    BoundedCallbacks,
    /// Reset preparation is an explicit absolute-time state machine.
    Scheduled,
}

/// Progress returned by an eventless platform ResetAll preparation hook.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResetHookPoll {
    Ready,
    Pending { wake_at_ns: u64 },
}

/// Platform hook for SDHCI integrations that need vendor register setup after
/// controller ResetAll has completed.
pub trait HostResetHook: Send + Sync {
    /// Declare whether this hook can be called by the non-blocking recovery
    /// lifecycle. The default is deliberately fail-closed.
    fn recovery_mode(&self) -> ResetHookRecoveryMode {
        ResetHookRecoveryMode::Unsupported
    }

    fn before_reset_all(&self, _host: &mut Sdhci) -> Result<(), Error> {
        Ok(())
    }

    /// Begin platform preparation without sleeping or busy-waiting.
    ///
    /// The default adapts only a proven immediate callback. Scheduled hooks
    /// must override this method and return their absolute next activation.
    fn begin_before_reset_all(
        &mut self,
        host: &mut Sdhci,
        _now_ns: u64,
    ) -> Result<ResetHookPoll, Error> {
        if self.recovery_mode() != ResetHookRecoveryMode::BoundedCallbacks {
            return Err(Error::UnsupportedCommand);
        }
        self.before_reset_all(host)?;
        Ok(ResetHookPoll::Ready)
    }

    /// Continue a scheduled ResetAll preparation after its requested wake.
    fn poll_before_reset_all(
        &mut self,
        _host: &mut Sdhci,
        _now_ns: u64,
    ) -> Result<ResetHookPoll, Error> {
        Err(Error::UnsupportedCommand)
    }

    /// Undo a scheduled preparation if its owning request is aborted.
    fn cancel_before_reset_all(&mut self, _host: &mut Sdhci) -> Result<(), Error> {
        if self.recovery_mode() == ResetHookRecoveryMode::Scheduled {
            Err(Error::UnsupportedCommand)
        } else {
            Ok(())
        }
    }

    fn after_reset(&self, host: &mut Sdhci) -> Result<(), Error>;
}

fn validate_reset_hook_poll(
    progress: Result<ResetHookPoll, Error>,
    now_ns: u64,
) -> Result<ResetHookPoll, Error> {
    match progress? {
        ResetHookPoll::Ready => Ok(ResetHookPoll::Ready),
        ResetHookPoll::Pending { wake_at_ns } if wake_at_ns > now_ns => {
            Ok(ResetHookPoll::Pending { wake_at_ns })
        }
        ResetHookPoll::Pending { .. } => Err(Error::InvalidArgument),
    }
}

/// Platform monotonic-time capability used for delays and request deadlines.
pub trait HostTimer: Sync {
    fn now_ms(&self) -> u64;
}

#[cfg(test)]
mod tests;
