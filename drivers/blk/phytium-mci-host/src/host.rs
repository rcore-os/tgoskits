use core::{
    ptr::NonNull,
    sync::atomic::{self, AtomicBool, AtomicU32, Ordering},
};

use mmio_api::MmioRaw;
use sdmmc_protocol::{
    error::{Error, ErrorContext, Phase},
    sdio::{BusWidth, SignalVoltage},
};
use volatile::VolatilePtr;

use crate::{
    Event, PhytiumMciIrqHandle,
    command::CommandState,
    regs::{
        CARD_THRCTL_OFFSET, CLK_SRC_OFFSET, CType, ClkEna, ClockSource, Cmd, RIntSts,
        RegisterBlock, RegisterBlockVolatileFieldAccess, Uhs,
    },
    timing::TimingTable,
};

pub const DEFAULT_FIFO_OFFSET: usize = 0x200;
const DEFAULT_FIFO_WORD_DEPTH: u32 = 128;
const FIFO_THRESHOLD: u32 = (2 << 28) | (7 << 16) | 0x100;
const CARD_READ_THRESHOLD_ENABLE: u32 = 1;
const CARD_READ_THRESHOLD_DEPTH8: u32 = 1 << 23;
const BMOD_SOFTWARE_RESET: u32 = 1;
const RESET_POLL_LIMIT: usize = 1_000_000;
const CLOCK_POLL_LIMIT: usize = 1_000_000;

#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingData {
    pub direction: sdmmc_protocol::DataDirection,
    pub block_size: u32,
    pub block_count: u32,
    pub use_idmac: bool,
}

pub(crate) struct IrqState {
    pending_status: AtomicU32,
    pending_idmac_status: AtomicU32,
}

impl IrqState {
    const fn new() -> Self {
        Self {
            pending_status: AtomicU32::new(0),
            pending_idmac_status: AtomicU32::new(0),
        }
    }

    pub(crate) fn cache_status(&self, status: u32) {
        if status != 0 {
            self.pending_status.fetch_or(status, Ordering::AcqRel);
        }
    }

    pub(crate) fn cache_idmac_status(&self, status: u32) {
        if status != 0 {
            self.pending_idmac_status.fetch_or(status, Ordering::AcqRel);
        }
    }

    pub(crate) fn take_status(&self, mask: u32) -> u32 {
        take_cached_bits(&self.pending_status, mask)
    }

    pub(crate) fn take_idmac_status(&self, mask: u32) -> u32 {
        take_cached_bits(&self.pending_idmac_status, mask)
    }

    pub(crate) fn clear_status(&self, mask: u32) {
        self.pending_status.fetch_and(!mask, Ordering::AcqRel);
    }

    pub(crate) fn clear_all(&self) {
        self.pending_status.store(0, Ordering::Release);
        self.pending_idmac_status.store(0, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn pending_status(&self) -> u32 {
        self.pending_status.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn pending_idmac_status(&self) -> u32 {
        self.pending_idmac_status.load(Ordering::Acquire)
    }
}

fn take_cached_bits(cache: &AtomicU32, mask: u32) -> u32 {
    let mut cur = cache.load(Ordering::Acquire);
    loop {
        let taken = cur & mask;
        if taken == 0 {
            return 0;
        }
        match cache.compare_exchange_weak(cur, cur & !mask, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return taken,
            Err(next) => cur = next,
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
    pub(crate) use_hold_reg: bool,
    pub(crate) irq_state: IrqState,
    completion_irq_enabled: AtomicBool,
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
            use_hold_reg: true,
            irq_state: IrqState::new(),
            completion_irq_enabled: AtomicBool::new(false),
        }
    }

    pub unsafe fn new_from_mmio_raw(mmio: &MmioRaw) -> Self {
        unsafe { Self::new(mmio.as_nonnull_ptr()) }
    }

    pub unsafe fn new_from_addr(base_addr: usize) -> Self {
        let base = NonNull::new(base_addr as *mut u8).expect("MMIO base address must be non-null");
        unsafe { Self::new(base) }
    }

    pub fn reset_and_init(&mut self) -> Result<(), Error> {
        self.regs.clkena().write(ClkEna::new());
        self.regs.ctrl().update(|r| {
            r.with_use_internal_dmac(false)
                .with_dma_enable(false)
                .with_int_enable(false)
        });
        self.regs.ctrl().update(|r| {
            r.with_controller_reset(true)
                .with_fifo_reset(true)
                .with_dma_reset(true)
        });
        self.wait_reset_clear(Phase::Init)?;

        self.regs.intmask().write(0);
        self.regs.idinten().write(0);
        self.clear_all_int_status();
        self.regs.idsts().write(u32::MAX);
        self.irq_state.clear_all();
        self.completion_irq_enabled.store(false, Ordering::Release);

        self.regs.ctype().write(CType::new());
        self.regs.uhs().write(Uhs::new());
        self.regs.tmout().write(0xffff_ffff);
        self.regs.pwren().write(1);
        self.regs.fifoth().write(FIFO_THRESHOLD);
        self.write_ext_reg(
            CARD_THRCTL_OFFSET,
            CARD_READ_THRESHOLD_ENABLE | CARD_READ_THRESHOLD_DEPTH8,
        );

        self.program_timing(TimingTable::sd_for_speed(
            sdmmc_protocol::sdio::ClockSpeed::Identification,
        )?)?;
        Ok(())
    }

    pub fn program_timing(&mut self, timing: TimingTable) -> Result<(), Error> {
        self.use_hold_reg = timing.use_hold;
        self.update_external_clock(timing.clk_src)?;
        self.set_card_clock(false)?;
        self.send_update_clock(false)?;
        self.regs.clkdiv().write(timing.clk_div);
        self.set_card_clock(true)?;
        self.send_update_clock(false)?;
        Ok(())
    }

    fn update_external_clock(&self, raw: u32) -> Result<(), Error> {
        self.write_ext_reg(CLK_SRC_OFFSET, 0);
        self.write_ext_reg(CLK_SRC_OFFSET, raw);
        for _ in 0..CLOCK_POLL_LIMIT {
            if self.regs.cksts().read().ready() {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(Error::Timeout(ErrorContext::new(Phase::Init)))
    }

    fn set_card_clock(&self, enable: bool) -> Result<(), Error> {
        let value = if enable {
            ClkEna::new().with_cclk_enable(1)
        } else {
            ClkEna::new()
        };
        self.regs.clkena().write(value);
        Ok(())
    }

    pub(crate) fn send_update_clock(&self, voltage_switch: bool) -> Result<(), Error> {
        self.regs.cmd().write(
            Cmd::new()
                .with_start_cmd(true)
                .with_wait_prvdata_complete(true)
                .with_update_clock_registers_only(true)
                .with_volt_switch(voltage_switch),
        );
        for _ in 0..CLOCK_POLL_LIMIT {
            if !self.regs.cmd().read().start_cmd() {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(Error::Timeout(ErrorContext::new(Phase::Init)))
    }

    fn wait_reset_clear(&self, phase: Phase) -> Result<(), Error> {
        for _ in 0..RESET_POLL_LIMIT {
            let c = self.regs.ctrl().read();
            if !c.controller_reset() && !c.fifo_reset() && !c.dma_reset() {
                self.send_update_clock(false)?;
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(Error::Timeout(ErrorContext::new(phase)))
    }

    pub(crate) fn clear_all_int_status(&self) {
        let cur = self.regs.rintsts().read();
        self.regs.rintsts().write(cur);
    }

    pub fn enable_completion_irq(&mut self) {
        self.completion_irq_enabled.store(true, Ordering::Release);
        self.regs.intmask().write(
            crate::MCI_INT_COMMAND_DONE
                | crate::MCI_INT_DATA_TRANSFER_OVER
                | crate::MCI_INT_RXDR
                | crate::MCI_INT_TXDR
                | crate::MCI_INT_ERROR_MASK,
        );
        self.regs.ctrl().update(|r| r.with_int_enable(true));
    }

    pub fn disable_completion_irq(&mut self) {
        self.completion_irq_enabled.store(false, Ordering::Release);
        self.regs.intmask().write(0);
        self.regs.ctrl().update(|r| r.with_int_enable(false));
    }

    pub fn completion_irq_enabled(&self) -> bool {
        self.completion_irq_enabled.load(Ordering::Acquire)
    }

    pub fn irq_handle(&self) -> PhytiumMciIrqHandle {
        PhytiumMciIrqHandle {
            regs: self.regs,
            irq_state: &self.irq_state,
        }
    }

    pub fn handle_irq(&self) -> Event {
        use sdmmc_protocol::sdio::SdioIrqHandle;

        self.irq_handle().handle_irq()
    }

    pub(crate) fn event_from_raw_irq(raw: u32, idsts: u32) -> Event {
        if raw & crate::MCI_INT_ERROR_MASK != 0 {
            Event::Error { raw_status: raw }
        } else if idsts & crate::MCI_IDSTS_ERROR_MASK != 0 {
            Event::Error { raw_status: idsts }
        } else if raw & crate::MCI_INT_DATA_TRANSFER_OVER != 0
            || idsts & (crate::MCI_IDSTS_RECEIVE | crate::MCI_IDSTS_TRANSMIT) != 0
        {
            Event::TransferComplete
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

    pub(crate) fn set_signal_voltage(&mut self, voltage: SignalVoltage) -> Result<(), Error> {
        let cur = self.regs.uhs().read();
        let next = uhs_bits_after_voltage(cur, voltage)?;
        self.regs.uhs().write(next);
        self.send_update_clock(matches!(voltage, SignalVoltage::V180))?;
        Ok(())
    }

    pub(crate) fn program_data_phase(&self, block_size: u32, block_count: u32) {
        self.regs.blksiz().write(block_size);
        self.regs.bytcnt().write(block_size * block_count);
    }

    pub(crate) fn reset_fifo(&self, phase: Phase) -> Result<(), Error> {
        self.regs.ctrl().update(|r| r.with_fifo_reset(true));
        for _ in 0..RESET_POLL_LIMIT {
            if !self.regs.ctrl().read().fifo_reset() {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(Error::Timeout(ErrorContext::new(phase)))
    }

    pub(crate) fn reset_dma(&self, phase: Phase) -> Result<(), Error> {
        self.regs.ctrl().update(|r| r.with_dma_reset(true));
        for _ in 0..RESET_POLL_LIMIT {
            if !self.regs.ctrl().read().dma_reset() {
                self.regs.bmod().write(BMOD_SOFTWARE_RESET);
                for _ in 0..RESET_POLL_LIMIT {
                    if self.regs.bmod().read() & BMOD_SOFTWARE_RESET == 0 {
                        return Ok(());
                    }
                    core::hint::spin_loop();
                }
                break;
            }
            core::hint::spin_loop();
        }
        Err(Error::Timeout(ErrorContext::new(phase)))
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

    fn write_ext_reg(&self, offset: usize, value: u32) {
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

impl sdmmc_protocol::sdio::SdioIrqHandle for PhytiumMciIrqHandle {
    type Event = Event;

    fn handle_irq(&self) -> Self::Event {
        let raw = self.regs.rintsts().read().into_bits();
        let idsts = self.regs.idsts().read();
        if raw != 0 {
            self.regs.rintsts().write(RIntSts::from_bits(raw));
            unsafe { &*self.irq_state }.cache_status(raw);
        }
        if idsts != 0 {
            self.regs.idsts().write(idsts);
            unsafe { &*self.irq_state }.cache_idmac_status(idsts);
        }

        PhytiumMci::event_from_raw_irq(raw, idsts)
    }
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
unsafe impl Sync for PhytiumMci {}

#[cfg(test)]
mod tests {
    use core::ptr::NonNull;

    use super::*;

    #[test]
    fn constructs_from_mapped_mmio_pointer() {
        let base = NonNull::new(0x2800_0000 as *mut u8).unwrap();
        let host = unsafe { PhytiumMci::new(base) };

        assert_eq!(host.base_addr, 0x2800_0000);
        assert_eq!(host.fifo_offset, DEFAULT_FIFO_OFFSET);
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
        let host = unsafe { PhytiumMci::new(base) };
        const IDSTS_WORD: usize = 36;
        const IDSTS_RECEIVE: u32 = 1 << 1;

        unsafe {
            mmio.as_mut_ptr()
                .add(IDSTS_WORD)
                .write_volatile(IDSTS_RECEIVE)
        };

        assert_eq!(host.handle_irq(), crate::Event::TransferComplete);
        assert_eq!(host.irq_state.pending_idmac_status(), IDSTS_RECEIVE);
        assert_eq!(host.irq_state.pending_status(), 0);
    }
}
