//! `Sdhci` core: MMIO accessors, reset, clock and bus-width setup.

use alloc::{boxed::Box, sync::Arc};
use core::{
    ptr::NonNull,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
};

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
/// MMIO base address for an SDHCI v3.x compatible controller. Concurrent
/// use of the same controller from multiple `Sdhci` instances is undefined.
const IRQ_GENERATION_SHIFT: u64 = 32;
const IRQ_NORMAL_MASK: u64 = 0xffff;
const IRQ_ERROR_SHIFT: u64 = 16;

pub(crate) struct IrqState {
    mailbox: AtomicU64,
    next_generation: AtomicU32,
}

impl IrqState {
    const fn new() -> Self {
        Self {
            mailbox: AtomicU64::new(0),
            next_generation: AtomicU32::new(0),
        }
    }

    pub(crate) fn begin_request(&self) {
        let generation = self.next_generation();
        self.mailbox
            .store(pack_mailbox(generation, 0, 0), Ordering::Release);
    }

    pub(crate) fn end_request(&self) {
        self.mailbox.store(0, Ordering::Release);
    }

    pub(crate) fn cache_if_current(&self, generation: u32, normal: u16, error: u16) {
        if generation == 0 || (normal == 0 && error == 0) {
            return;
        }
        let mut cur = self.mailbox.load(Ordering::Acquire);
        loop {
            if mailbox_generation(cur) != generation {
                return;
            }
            let next = pack_mailbox(
                generation,
                mailbox_normal(cur) | normal,
                mailbox_error(cur) | error,
            );
            match self
                .mailbox
                .compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return,
                Err(observed) => cur = observed,
            }
        }
    }

    pub(crate) fn generation(&self) -> u32 {
        mailbox_generation(self.mailbox.load(Ordering::Acquire))
    }

    pub(crate) fn take_normal(&self, mask: u16) -> u16 {
        let mut cur = self.mailbox.load(Ordering::Acquire);
        loop {
            let normal = mailbox_normal(cur);
            let taken = normal & mask;
            if taken == 0 {
                return 0;
            }
            let next = pack_mailbox(mailbox_generation(cur), normal & !mask, mailbox_error(cur));
            match self
                .mailbox
                .compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return taken,
                Err(observed) => cur = observed,
            }
        }
    }

    pub(crate) fn take_error_all(&self) -> u16 {
        let mut cur = self.mailbox.load(Ordering::Acquire);
        loop {
            let error = mailbox_error(cur);
            if error == 0 {
                return 0;
            }
            let next = pack_mailbox(mailbox_generation(cur), mailbox_normal(cur), 0);
            match self
                .mailbox
                .compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return error,
                Err(observed) => cur = observed,
            }
        }
    }

    pub(crate) fn clear_normal(&self, mask: u16) {
        let mut cur = self.mailbox.load(Ordering::Acquire);
        loop {
            let next = pack_mailbox(
                mailbox_generation(cur),
                mailbox_normal(cur) & !mask,
                mailbox_error(cur),
            );
            match self
                .mailbox
                .compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return,
                Err(observed) => cur = observed,
            }
        }
    }

    pub(crate) fn clear_all(&self) {
        let mut cur = self.mailbox.load(Ordering::Acquire);
        loop {
            let next = pack_mailbox(mailbox_generation(cur), 0, 0);
            match self
                .mailbox
                .compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return,
                Err(observed) => cur = observed,
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn pending_normal(&self) -> u16 {
        mailbox_normal(self.mailbox.load(Ordering::Acquire))
    }

    #[cfg(test)]
    pub(crate) fn pending_error(&self) -> u16 {
        mailbox_error(self.mailbox.load(Ordering::Acquire))
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
}

fn pack_mailbox(generation: u32, normal: u16, error: u16) -> u64 {
    ((generation as u64) << IRQ_GENERATION_SHIFT)
        | normal as u64
        | ((error as u64) << IRQ_ERROR_SHIFT)
}

fn mailbox_generation(value: u64) -> u32 {
    (value >> IRQ_GENERATION_SHIFT) as u32
}

fn mailbox_normal(value: u64) -> u16 {
    (value & IRQ_NORMAL_MASK) as u16
}

fn mailbox_error(value: u64) -> u16 {
    ((value >> IRQ_ERROR_SHIFT) & IRQ_NORMAL_MASK) as u16
}

pub(crate) struct IrqCore {
    pub(crate) base_addr: usize,
    pub(crate) state: IrqState,
}

impl IrqCore {
    fn new(base_addr: usize) -> Self {
        Self {
            base_addr,
            state: IrqState::new(),
        }
    }
}

pub struct Sdhci {
    pub(crate) base_addr: usize,
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
    /// Optional monotonic timer used by asynchronous bus-operation state
    /// machines that have specification-defined wall-clock delays.
    pub(crate) timer: Option<&'static dyn HostTimer>,
    /// Whether the platform has wired up the IO-domain regulator needed to
    /// actually run the bus at 1.8 V. Default `false` — toggling
    /// `HOST_CONTROL2.1V8_SIGNALING_ENABLE` alone changes the controller
    /// sampling behaviour without changing the IO rail, which corrupts
    /// subsequent transfers; refusing the switch lets the protocol layer
    /// fall back to a 3.3 V-compatible mode.
    pub(crate) support_1v8: bool,
    /// Command index for the data phase currently being drained by the
    /// submit/poll data-command state machine.
    pub(crate) active_data_cmd: u8,
    pub(crate) dma: Option<DeviceDma>,
    pub(crate) dma_mask: u64,
    pub(crate) dma_poisoned: bool,
    pub(crate) irq: Arc<IrqCore>,
    pub(crate) host2_next_id: u64,
    pub(crate) host2_active_id: Option<u64>,
}

impl Sdhci {
    /// Construct a new Sdhci over an already-mapped MMIO register file.
    ///
    /// # Safety
    ///
    /// `base` must point to a memory-mapped SDHCI v3.x register file
    /// that the caller has exclusive access to.
    pub unsafe fn new(base: NonNull<u8>) -> Self {
        Self {
            base_addr: base.as_ptr() as usize,
            command_state: CommandState::Idle,
            pending_data: None,
            use_dma: false,
            ext_clock: None,
            reset_hook: None,
            timer: None,
            support_1v8: false,
            active_data_cmd: 0,
            dma: None,
            dma_mask: u32::MAX as u64,
            dma_poisoned: false,
            irq: Arc::new(IrqCore::new(base.as_ptr() as usize)),
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

    /// Install a platform post-reset hook. The hook is called after ResetAll
    /// clears, both for the legacy blocking reset helper and for the native
    /// `sdio-host2` bus-operation state machine.
    pub fn set_reset_hook<H>(&mut self, hook: H)
    where
        H: HostResetHook + 'static,
    {
        self.reset_hook = Some(Box::new(hook));
    }

    pub(crate) fn call_before_reset_all_hook(&mut self) -> Result<(), Error> {
        let Some(hook) = self.reset_hook.take() else {
            return Ok(());
        };
        let result = hook.before_reset_all(self);
        self.reset_hook = Some(hook);
        result
    }

    pub(crate) fn call_after_reset_hook(&mut self) -> Result<(), Error> {
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

    /// Reset the controller (CMD line + DAT line + state) by writing the
    /// "Reset All" bit and waiting for it to clear.
    pub fn reset_all(&mut self) -> Result<(), Error> {
        self.reset_with_mask(RESET_ALL, Phase::Init)
            .inspect(|_| self.dma_poisoned = false)
    }

    /// Reset the CMD line state machine (clears any stuck CMD inhibit).
    pub fn reset_cmd(&mut self) -> Result<(), Error> {
        self.reset_with_mask(RESET_CMD, Phase::CommandSend)
    }

    /// Reset the DAT line state machine.
    pub fn reset_dat(&mut self) -> Result<(), Error> {
        self.reset_with_mask(RESET_DAT, Phase::DataRead)
    }

    fn reset_with_mask(&mut self, mask: u8, phase: Phase) -> Result<(), Error> {
        if mask == RESET_ALL {
            self.call_before_reset_all_hook()?;
        }
        self.write_u8(REG_SOFTWARE_RESET, mask);
        for _ in 0..1000 {
            if self.read_u8(REG_SOFTWARE_RESET) & mask == 0 {
                if mask == RESET_ALL {
                    self.call_after_reset_hook()?;
                }
                return Ok(());
            }
            spin_loop();
        }
        Err(Error::Timeout(ErrorContext::new(phase)))
    }

    /// Bring the internal clock up. `base_clock_hz` is the controller's
    /// reference clock (read from Capabilities or supplied externally) and
    /// `target_hz` is the desired SD bus frequency.
    ///
    /// Uses the SDHCI v3.0 10-bit divided clock mode.
    pub fn enable_clock(&mut self, base_clock_hz: u32, target_hz: u32) -> Result<(), Error> {
        // 1. Disable SD clock so we can safely change the divider.
        self.write_u16(REG_CLOCK_CONTROL, 0);

        if target_hz == 0 {
            return Ok(());
        }

        // 2. Pick the smallest divider such that base/2N ≤ target. SDHCI
        //    v3.0 supports 10-bit divider in steps of 2 (so 2N ranges 2..1024).
        let mut div = 0u16;
        if base_clock_hz > target_hz {
            for n in 1..=0x3FF {
                if base_clock_hz / (2 * n as u32) <= target_hz {
                    div = n;
                    break;
                }
            }
        }

        // Encode divider: bits 15..8 hold low 8 bits, bits 7..6 hold the
        // upper 2 bits of the 10-bit divider for v3.0 compatible hosts.
        let clk_ctrl = ((div & 0xFF) << 8) | ((div & 0x300) >> 2) | CLOCK_INTERNAL_ENABLE;
        self.write_u16(REG_CLOCK_CONTROL, clk_ctrl);

        // 3. Wait for internal clock to stabilize.
        for _ in 0..1000 {
            if self.read_u16(REG_CLOCK_CONTROL) & CLOCK_INTERNAL_STABLE != 0 {
                let stable = self.read_u16(REG_CLOCK_CONTROL) | CLOCK_SD_ENABLE;
                self.write_u16(REG_CLOCK_CONTROL, stable);
                return Ok(());
            }
            spin_loop();
        }
        Err(Error::Timeout(ErrorContext::new(Phase::Init)))
    }

    /// Enable SD clock after the platform-supplied input clock has been set.
    ///
    /// Use this on controllers whose internal 10-bit divider is unusable
    /// (e.g. DWC MSHC variants, or cores that report `BaseClockFreq = 0`
    /// in Capabilities and require the SoC's CRU to do all the frequency
    /// scaling). In that mode the caller is expected to:
    ///
    /// 1. Reprogram the SoC clock controller to a usable input clock.
    /// 2. Call `enable_clock_external()` to gate the SD clock on, usually
    ///    with a 1:1 divider. Platforms that quantize low rates can pass the
    ///    actual input rate so the standard divider avoids broken encodings.
    ///
    /// If `target_hz` is 0 the SD clock is left disabled.
    pub fn enable_clock_external(
        &mut self,
        input_hz: u32,
        target_hz: u32,
        div_zero_broken: bool,
    ) -> Result<(), Error> {
        // Disable, then re-enable with the smallest SDHCI divider that does
        // not exceed the requested bus clock.
        self.write_u16(REG_CLOCK_CONTROL, 0);
        if target_hz == 0 {
            return Ok(());
        }
        let div = crate::sdhci_clock_divisor_with_quirk(input_hz, target_hz, div_zero_broken);
        let clk_ctrl = ((div & 0xFF) << 8) | ((div & 0x300) >> 2) | CLOCK_INTERNAL_ENABLE;
        self.write_u16(REG_CLOCK_CONTROL, clk_ctrl);
        for _ in 0..1000 {
            if self.read_u16(REG_CLOCK_CONTROL) & CLOCK_INTERNAL_STABLE != 0 {
                let stable = self.read_u16(REG_CLOCK_CONTROL) | CLOCK_SD_ENABLE;
                self.write_u16(REG_CLOCK_CONTROL, stable);
                return Ok(());
            }
            spin_loop();
        }
        Err(Error::Timeout(ErrorContext::new(Phase::Init)))
    }

    /// Enable SDHCI internal/card clock without programming a divided
    /// SDCLK value. Rockchip DWCMSHC follows Linux's `sdhci_enable_clk(host,
    /// 0)` path after SoC-side clocking and DLL registers have already been
    /// configured; applying the generic divider again can underclock
    /// identification mode and leave the command FSM stuck.
    pub fn enable_clock_passthrough(&mut self, target_hz: u32) -> Result<(), Error> {
        self.write_u16(REG_CLOCK_CONTROL, 0);
        if target_hz == 0 {
            return Ok(());
        }
        self.write_u16(REG_CLOCK_CONTROL, CLOCK_INTERNAL_ENABLE);
        for _ in 0..1000 {
            if self.read_u16(REG_CLOCK_CONTROL) & CLOCK_INTERNAL_STABLE != 0 {
                let stable = self.read_u16(REG_CLOCK_CONTROL) | CLOCK_SD_ENABLE;
                self.write_u16(REG_CLOCK_CONTROL, stable);
                return Ok(());
            }
            spin_loop();
        }
        Err(Error::Timeout(ErrorContext::new(Phase::Init)))
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

    /// Enable normal + error interrupt status flags so command/data
    /// completion is observable via the status registers (signal-level
    /// IRQ delivery is NOT enabled — the driver polls).
    pub fn enable_interrupts(&mut self) {
        self.write_u16(REG_NORMAL_INT_STATUS_ENABLE, NORMAL_INT_CLEAR_ALL);
        self.write_u16(REG_ERROR_INT_STATUS_ENABLE, ERROR_INT_CLEAR_ALL);
        // Don't route to host CPU IRQ — leave Signal Enable cleared.
        self.write_u16(REG_NORMAL_INT_SIGNAL_ENABLE, 0);
        self.write_u16(REG_ERROR_INT_SIGNAL_ENABLE, 0);
    }

    pub(crate) fn enable_polling_interrupt_status(&mut self) {
        self.enable_interrupts();
    }

    /// Route command/data-completion and error status to the host CPU IRQ line.
    pub fn enable_completion_irq(&mut self) {
        self.write_u16(
            REG_NORMAL_INT_SIGNAL_ENABLE,
            NORMAL_INT_CMD_COMPLETE
                | NORMAL_INT_XFER_COMPLETE
                | NORMAL_INT_BUFFER_WRITE_READY
                | NORMAL_INT_BUFFER_READ_READY
                | NORMAL_INT_ERROR,
        );
        self.write_u16(
            REG_ERROR_INT_SIGNAL_ENABLE,
            ERROR_INT_CMD_LINE_MASK | ERROR_INT_DATA_OR_ADMA_MASK,
        );
    }

    /// Mask host CPU IRQ delivery while keeping status bits observable.
    pub fn disable_completion_irq(&mut self) {
        self.write_u16(REG_NORMAL_INT_SIGNAL_ENABLE, 0);
        self.write_u16(REG_ERROR_INT_SIGNAL_ENABLE, 0);
    }

    pub fn completion_irq_enabled(&self) -> bool {
        self.read_u16(REG_NORMAL_INT_SIGNAL_ENABLE)
            & (NORMAL_INT_CMD_COMPLETE | NORMAL_INT_XFER_COMPLETE | NORMAL_INT_ERROR)
            != 0
    }

    /// Read the controller's base reference clock from Capabilities (Hz).
    pub fn base_clock_hz(&self) -> u32 {
        let caps_low = self.read_u32(REG_CAPABILITIES_LOW);
        // SDHCI v3: bits 15..8 contain "Base Clock Frequency" in MHz.
        // SDHCI v2: bits 13..8 contain it. Use the wider mask; QEMU
        // sdhci-pci reports a v2 layout but the result is still right.
        let mhz = (caps_low >> 8) & 0xFF;
        mhz.saturating_mul(1_000_000)
    }

    /// Whether the controller advertises ADMA2 in the capabilities register.
    pub fn supports_adma2(&self) -> bool {
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
        unsafe { core::ptr::read_volatile((self.base_addr + off) as *const u32) }
    }

    pub(crate) fn write_u32(&self, off: usize, val: u32) {
        unsafe { core::ptr::write_volatile((self.base_addr + off) as *mut u32, val) }
    }

    pub(crate) fn read_u16(&self, off: usize) -> u16 {
        unsafe { core::ptr::read_volatile((self.base_addr + off) as *const u16) }
    }

    pub(crate) fn write_u16(&self, off: usize, val: u16) {
        unsafe { core::ptr::write_volatile((self.base_addr + off) as *mut u16, val) }
    }

    pub(crate) fn read_u8(&self, off: usize) -> u8 {
        unsafe { core::ptr::read_volatile((self.base_addr + off) as *const u8) }
    }

    pub(crate) fn write_u8(&self, off: usize, val: u8) {
        unsafe { core::ptr::write_volatile((self.base_addr + off) as *mut u8, val) }
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

    /// Whether SDHCI divider encoding zero is unusable for this integration.
    fn clock_div_zero_broken(&self) -> bool {
        false
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

/// Platform hook for SDHCI integrations that need vendor register setup after
/// controller ResetAll has completed.
pub trait HostResetHook: Send + Sync {
    fn before_reset_all(&self, _host: &mut Sdhci) -> Result<(), Error> {
        Ok(())
    }

    fn after_reset(&self, host: &mut Sdhci) -> Result<(), Error>;
}

/// Platform monotonic-time capability used for specification-defined delays.
pub trait HostTimer: Sync {
    fn now_ms(&self) -> u64;
}

#[inline]
fn spin_loop() {
    core::hint::spin_loop();
}

#[cfg(test)]
mod tests {
    use core::{
        ptr::NonNull,
        sync::atomic::{AtomicU8, Ordering},
    };

    use super::*;

    #[test]
    fn constructs_from_mapped_mmio_pointer() {
        let base = NonNull::new(0x1000_0000 as *mut u8).unwrap();
        let host = unsafe { Sdhci::new(base) };

        assert_eq!(host.base_addr, 0x1000_0000);
    }

    #[test]
    fn legacy_addr_constructor_keeps_raw_mmio_boundary_explicit() {
        let host = unsafe { Sdhci::new_from_addr(0x1000_0000) };

        assert_eq!(host.base_addr, 0x1000_0000);
    }

    #[test]
    fn external_clock_can_be_scoped_and_cleared() {
        struct Clock;

        impl HostClock for Clock {
            fn set_clock(&self, _target_hz: u32) -> Result<(), Error> {
                Ok(())
            }
        }

        let mut mmio = [0u8; 256];
        let base = NonNull::new(mmio.as_mut_ptr()).unwrap();
        let mut host = unsafe { Sdhci::new(base) };

        host.set_external_clock(Clock);
        assert!(host.ext_clock.is_some());

        host.clear_external_clock();
        assert!(host.ext_clock.is_none());
    }

    #[test]
    fn reset_all_calls_owned_platform_before_hook_before_software_reset() {
        struct Hook;
        static OBSERVED_RESET: AtomicU8 = AtomicU8::new(u8::MAX);

        impl HostResetHook for Hook {
            fn before_reset_all(&self, host: &mut Sdhci) -> Result<(), Error> {
                OBSERVED_RESET.store(host.read_u8(REG_SOFTWARE_RESET), Ordering::Release);
                Ok(())
            }

            fn after_reset(&self, _host: &mut Sdhci) -> Result<(), Error> {
                Ok(())
            }
        }

        let mut mmio = [0u8; 256];
        let base = NonNull::new(mmio.as_mut_ptr()).unwrap();
        let mut host = unsafe { Sdhci::new(base) };
        host.set_reset_hook(Hook);

        assert!(host.reset_all().is_err());

        assert_eq!(OBSERVED_RESET.load(Ordering::Acquire), 0);
    }

    #[test]
    fn polling_interrupt_status_enable_keeps_signal_irq_masked() {
        let mut mmio = [0u8; 256];
        let base = NonNull::new(mmio.as_mut_ptr()).unwrap();
        let mut host = unsafe { Sdhci::new(base) };
        host.write_u16(REG_NORMAL_INT_STATUS_ENABLE, 0);
        host.write_u16(REG_ERROR_INT_STATUS_ENABLE, 0);
        host.write_u16(REG_NORMAL_INT_SIGNAL_ENABLE, NORMAL_INT_CLEAR_ALL);
        host.write_u16(REG_ERROR_INT_SIGNAL_ENABLE, ERROR_INT_CLEAR_ALL);

        host.enable_polling_interrupt_status();
        assert_eq!(
            host.read_u16(REG_NORMAL_INT_STATUS_ENABLE),
            NORMAL_INT_CLEAR_ALL
        );
        assert_eq!(
            host.read_u16(REG_ERROR_INT_STATUS_ENABLE),
            ERROR_INT_CLEAR_ALL
        );
        assert_eq!(host.read_u16(REG_NORMAL_INT_SIGNAL_ENABLE), 0);
        assert_eq!(host.read_u16(REG_ERROR_INT_SIGNAL_ENABLE), 0);
    }
}
