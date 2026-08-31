//! `Sdhci` core: MMIO accessors, reset, clock and bus-width setup.

use alloc::{boxed::Box, sync::Arc};
use core::{
    ptr::NonNull,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
};

use dma_api::DeviceDma;
use mmio_api::MmioRaw;
use sdmmc_protocol::error::{Error, ErrorContext, Phase};

use crate::{command::CommandState, dma::Adma2DescriptorTable, regs::*};

/// Shape of the single data phase carried by an in-flight command state.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingData {
    pub direction: sdmmc_protocol::DataDirection,
    pub block_size: u32,
    pub block_count: u32,
}

mod irq_state;
pub(crate) use irq_state::IrqCore;

/// Generic SD Host Controller (SDHCI) backend.
///
/// Owns the MMIO base address of one host controller instance and
/// implements [`sdmmc_protocol::sdio::SdMmcHost`] so that the protocol
/// driver in `sdmmc-protocol` can drive it. Data transfers use the ADMA2
/// state machine exclusively.
///
/// # Safety
///
/// `new` is `unsafe` because the caller must provide a valid, exclusive
/// MMIO base address for an SDHCI v3.x compatible controller. Concurrent
/// use of the same controller from multiple `Sdhci` instances is undefined.
pub struct Sdhci {
    pub(crate) base_addr: usize,
    pub(crate) command_state: CommandState,
    /// Optional CRU-side clock callback. When set, the `SdMmcHost::set_clock`
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
    /// IRQ-driven data-command state machine.
    pub(crate) active_data_cmd: u8,
    pub(crate) dma: Option<DeviceDma>,
    /// Controller-lifetime ADMA2 table. Queue depth one guarantees that the
    /// hardware and the maintenance thread never reuse it concurrently.
    pub(crate) adma2_table: Option<Adma2DescriptorTable>,
    pub(crate) dma_mask: u64,
    pub(crate) v4_mode: bool,
    pub(crate) dma_poisoned: bool,
    pub(crate) irq: Arc<IrqCore>,
    pub(crate) host2_next_id: u64,
    pub(crate) host2_active_id: Option<u64>,
    #[cfg(test)]
    pub(crate) reset_auto_complete: bool,
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
            ext_clock: None,
            reset_hook: None,
            timer: None,
            support_1v8: false,
            active_data_cmd: 0,
            dma: None,
            adma2_table: None,
            dma_mask: u32::MAX as u64,
            v4_mode: false,
            dma_poisoned: false,
            irq: Arc::new(IrqCore::new(base.as_ptr() as usize)),
            host2_next_id: 0,
            host2_active_id: None,
            #[cfg(test)]
            reset_auto_complete: false,
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
    /// `sdmmc-host` bus-operation state machine.
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
    /// Until this is called, [`SdMmcHost::switch_voltage`] refuses
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
    /// Once installed, data transactions use ADMA2 for compatible block I/O.
    /// Requests are rejected when DMA is unavailable or violates the host
    /// limits; the driver never falls back to PIO.
    pub fn configure_dma(&mut self, dma: DeviceDma) -> Result<(), Error> {
        if !matches!(self.command_state, CommandState::Idle) {
            return Err(Error::UnsupportedCommand);
        }
        if !self.supports_adma2() {
            return Err(Error::UnsupportedCommand);
        }
        let hardware_mask = if self.supports_64bit_system_addressing() {
            dma.info().constraints().addr_mask
        } else {
            dma.info().constraints().addr_mask.min(u32::MAX as u64)
        };
        let mut constraints = dma.info().constraints();
        constraints.addr_mask = hardware_mask;
        constraints.align = constraints.align.max(4);
        let dma = dma.with_constraints(constraints);
        let use_64bit = hardware_mask > u32::MAX as u64 && self.supports_64bit_system_addressing();
        let table = Adma2DescriptorTable::allocate(&dma, use_64bit)?;
        self.dma_mask = hardware_mask;
        self.dma = Some(dma);
        self.adma2_table = Some(table);
        Ok(())
    }

    /// Enable SDHCI v4 register semantics before configuring DMA.
    ///
    /// Platforms opt in explicitly, matching Linux's `sdhci_enable_v4_mode`;
    /// capability bits alone do not change descriptor format.
    pub fn enable_v4_mode(&mut self) -> Result<(), Error> {
        if self.dma.is_some() || !matches!(self.command_state, CommandState::Idle) {
            return Err(Error::UnsupportedCommand);
        }
        let mut control = self.read_u16(REG_HOST_CONTROL2);
        control |= HOST_CTRL2_V4_MODE;
        self.write_u16(REG_HOST_CONTROL2, control);
        self.v4_mode = true;
        Ok(())
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
        #[cfg(test)]
        if self.reset_auto_complete {
            self.write_u8(REG_SOFTWARE_RESET, 0);
        }
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

    /// Enable normal and error status capture without unmasking CPU IRQ delivery.
    ///
    /// Recovery uses this before restoring the runtime-owned signal mask so a
    /// subsequent IRQ cannot arrive without a corresponding latched status.
    pub(crate) fn enable_interrupt_status_capture(&mut self) {
        write_irq_register(
            self.base_addr,
            REG_NORMAL_INT_STATUS_ENABLE,
            NORMAL_INT_CLEAR_ALL,
            ERROR_INT_CLEAR_ALL,
        );
        write_irq_register(self.base_addr, REG_NORMAL_INT_SIGNAL_ENABLE, 0, 0);
    }

    /// Route command/data-completion and error status to the host CPU IRQ line.
    pub fn enable_completion_irq(&mut self) {
        let (status_enable, _) = read_irq_register(self.base_addr, REG_NORMAL_INT_STATUS_ENABLE);
        let card_status = status_enable & NORMAL_INT_CARD_INTERRUPT;
        let (signals, _) = read_irq_register(self.base_addr, REG_NORMAL_INT_SIGNAL_ENABLE);
        let card_interrupt = signals & NORMAL_INT_CARD_INTERRUPT;
        // STATUS_ENABLE owns the controller's latched sources; SIGNAL_ENABLE
        // only gates assertion of the parent IRQ.  Linux programs both masks
        // during host bring-up.  Keeping this here makes every enable path
        // self-contained, including SDIO initialization after a controller
        // reset where the status mask is not guaranteed to retain its value.
        write_irq_register(
            self.base_addr,
            REG_NORMAL_INT_STATUS_ENABLE,
            (NORMAL_INT_CLEAR_ALL & !NORMAL_INT_CARD_INTERRUPT) | card_status,
            ERROR_INT_CLEAR_ALL,
        );
        write_irq_register(
            self.base_addr,
            REG_NORMAL_INT_SIGNAL_ENABLE,
            NORMAL_INT_CMD_COMPLETE
                | NORMAL_INT_XFER_COMPLETE
                | NORMAL_INT_BUFFER_WRITE_READY
                | NORMAL_INT_BUFFER_READ_READY
                | NORMAL_INT_ERROR
                | card_interrupt,
            ERROR_INT_CMD_LINE_MASK | ERROR_INT_DATA_OR_ADMA_MASK,
        );
    }

    /// Mask host CPU IRQ delivery while keeping status bits observable.
    pub fn disable_completion_irq(&mut self) {
        let (signals, _) = read_irq_register(self.base_addr, REG_NORMAL_INT_SIGNAL_ENABLE);
        let card_interrupt = signals & NORMAL_INT_CARD_INTERRUPT;
        write_irq_register(
            self.base_addr,
            REG_NORMAL_INT_SIGNAL_ENABLE,
            card_interrupt,
            0,
        );
    }

    pub fn completion_irq_enabled(&self) -> bool {
        let (signals, _) = read_irq_register(self.base_addr, REG_NORMAL_INT_SIGNAL_ENABLE);
        signals & (NORMAL_INT_CMD_COMPLETE | NORMAL_INT_XFER_COMPLETE | NORMAL_INT_ERROR) != 0
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

    pub fn supports_64bit_system_addressing(&self) -> bool {
        let capabilities = self.read_u32(REG_CAPABILITIES_LOW);
        if self.v4_mode {
            capabilities & CAPS_LOW_64BIT_SYSBUS_V4 != 0
        } else {
            capabilities & CAPS_LOW_64BIT_SYSBUS_V3 != 0
        }
    }

    /// Program the ADMA system address registers with the bus address of
    /// the descriptor table.
    pub(crate) fn write_adma_addr(&self, addr: u64, use_64bit: bool) {
        self.write_u32(REG_ADMA_SYS_ADDR_LOW, addr as u32);
        self.write_u32(
            REG_ADMA_SYS_ADDR_HIGH,
            if use_64bit { (addr >> 32) as u32 } else { 0 },
        );
    }

    pub(crate) fn select_adma2(&mut self, use_64bit: bool) {
        let mut ctrl = self.read_u8(REG_HOST_CONTROL1);
        let selection = if use_64bit && !self.v4_mode {
            HOST_CTRL1_DMA_SEL_ADMA2_64
        } else {
            HOST_CTRL1_DMA_SEL_ADMA2_32
        };
        ctrl = (ctrl & !HOST_CTRL1_DMA_SEL_MASK) | selection;
        self.write_u8(REG_HOST_CONTROL1, ctrl);

        if self.v4_mode {
            let mut ctrl2 = self.read_u16(REG_HOST_CONTROL2);
            if use_64bit {
                ctrl2 |= HOST_CTRL2_64BIT_ADDR;
            } else {
                ctrl2 &= !HOST_CTRL2_64BIT_ADDR;
            }
            self.write_u16(REG_HOST_CONTROL2, ctrl2);
        }
    }

    /// Read raw 32-bit response slot.
    pub(crate) fn response32(&self, slot: usize) -> u32 {
        let off = REG_RESPONSE0 + slot * 4;
        self.read_u32(off)
    }

    pub(crate) fn read_interrupt_status(&self) -> (u16, u16) {
        read_irq_register(self.base_addr, REG_NORMAL_INT_STATUS)
    }

    pub(crate) fn write_interrupt_status(&self, normal: u16, error: u16) {
        write_irq_register(self.base_addr, REG_NORMAL_INT_STATUS, normal, error);
    }

    pub(crate) fn read_interrupt_status_enable(&self) -> (u16, u16) {
        read_irq_register(self.base_addr, REG_NORMAL_INT_STATUS_ENABLE)
    }

    pub(crate) fn read_interrupt_signal_enable(&self) -> (u16, u16) {
        read_irq_register(self.base_addr, REG_NORMAL_INT_SIGNAL_ENABLE)
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

/// Read one of the SDHCI interrupt register pairs with the 32-bit access used
/// by Linux `sdhci_readl()`. The low half is the normal interrupt field and
/// the high half is its corresponding error field.
pub(crate) fn read_irq_register(base_addr: usize, off: usize) -> (u16, u16) {
    let value = unsafe { core::ptr::read_volatile((base_addr + off) as *const u32) };
    (value as u16, (value >> 16) as u16)
}

/// Write an adjacent normal/error interrupt pair with the Linux SDHCI 32-bit
/// transaction. Splitting this into 16-bit writes can lose completion state on
/// DWC MSHC integrations.
pub(crate) fn write_irq_register(base_addr: usize, off: usize, normal: u16, error: u16) {
    let value = u32::from(normal) | (u32::from(error) << 16);
    unsafe { core::ptr::write_volatile((base_addr + off) as *mut u32, value) }
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
#[path = "../host_tests.rs"]
mod tests;
