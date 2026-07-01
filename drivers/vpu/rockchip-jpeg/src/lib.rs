//! Low-level device layer for the Rockchip RK3588 hardware JPEG decoder
//! (the VDPU720 / `RKDJPEG` block, DT node `jpegd@fdb90000`).
//!
//! The crate is OS-independent (`#![no_std]`) and split into:
//! - [`registers`]: the `RKDJPEG_SWREG*` register-file definitions.
//! - [`status`]: pure decoding of the `SWREG1` interrupt/status word.
//! - [`parser`]: a baseline JPEG header parser.
//! - [`command`]: the 42-word register-array + 1280-byte quant/Huffman table
//!   builder (reproduces the vendor MPP `hal_jpegd_rkv` programming).
//! - [`mpp`]: the Rockchip MPP `/dev/mpp_service` wire ABI (session state machine).
//!
//! [`JpuCore`] is the MMIO runtime (reset, program, poll, decode); [`RockchipJpeg`]
//! is the probed device (engine + DMA capability + boot self-test).

#![no_std]

// The host test harness links `std`; allow tests to build JPEG fixtures with it.
#[cfg(test)]
extern crate std;

pub mod command;
pub mod mpp;
pub mod parser;
pub mod registers;
pub mod selftest;
pub mod status;

use core::ptr::NonNull;

use dma_api::DeviceDma;
pub use selftest::{SELFTEST_JPEG_CAPACITY, write_selftest_jpeg};

use crate::{
    parser::JpegInfo,
    registers::offset,
    status::{DecodeError, DecodeStatus},
};

/// 32-bit MMIO access to the JPEG decoder register file. Abstracted so the core
/// can be exercised by host tests with a fake backend.
pub trait JpuMmio {
    /// Read the 32-bit register at byte `offset`.
    fn read32(&self, offset: usize) -> u32;
    /// Write `value` to the 32-bit register at byte `offset`.
    fn write32(&mut self, offset: usize, value: u32);
}

/// Volatile MMIO over a register base mapped by platform glue.
pub struct RawMmio {
    base: NonNull<u8>,
}

impl RawMmio {
    /// Wrap a register base pointer.
    pub fn new(base: NonNull<u8>) -> Self {
        Self { base }
    }
}

// The pointer is an MMIO base owned by platform glue; access is serialized
// through `&mut self` on the owning `JpuCore`.
unsafe impl Send for RawMmio {}

impl JpuMmio for RawMmio {
    fn read32(&self, offset: usize) -> u32 {
        unsafe { self.base.as_ptr().add(offset).cast::<u32>().read_volatile() }
    }

    fn write32(&mut self, offset: usize, value: u32) {
        unsafe {
            self.base
                .as_ptr()
                .add(offset)
                .cast::<u32>()
                .write_volatile(value)
        }
    }
}

/// Errors from the JPEG decoder runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JpuError {
    /// Soft-reset did not complete within the timeout.
    ResetTimeout,
    /// Decode did not signal completion within the timeout.
    DecodeTimeout,
    /// Hardware reported a decode error.
    Decode(DecodeError),
}

/// Physical (or device-visible) base addresses for one decode.
///
/// In the IOMMU-bypass bring-up path these are physical DRAM addresses of
/// contiguous buffers (`dma_addr == phys == iova`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeAddrs {
    /// Base of the 1280-byte quant/Huffman table buffer.
    pub table_phys: u32,
    /// Base of the input JPEG bitstream buffer.
    pub stream_phys: u32,
    /// Base of the output (NV12) frame buffer.
    pub output_phys: u32,
}

/// The JPEG decoder hardware engine over an [`JpuMmio`] backend.
pub struct JpuCore<M> {
    mmio: M,
}

impl<M: JpuMmio> JpuCore<M> {
    /// Create a core over the given MMIO backend.
    pub fn new(mmio: M) -> Self {
        Self { mmio }
    }

    /// Read the `SWREG0` ID register (`prod_num` in the upper half).
    pub fn read_id(&self) -> u32 {
        self.mmio.read32(offset(registers::REG_ID))
    }

    /// Pulse soft-reset and wait for `soft_reset_rdy`, bounded by `timeout`.
    ///
    /// `clock` and `timeout` must share a unit (the runtime passes a nanosecond
    /// monotonic clock with a nanosecond budget); the comparison is unit-agnostic.
    pub fn soft_reset<C: FnMut() -> u64>(
        &mut self,
        clock: &mut C,
        timeout: u64,
    ) -> Result<(), JpuError> {
        self.mmio
            .write32(offset(registers::REG_INT), registers::INT_SOFTRESET);
        let start = clock();
        loop {
            if self.mmio.read32(offset(registers::REG_INT)) & registers::INT_SOFTRESET_RDY != 0 {
                return Ok(());
            }
            if clock().wrapping_sub(start) >= timeout {
                return Err(JpuError::ResetTimeout);
            }
        }
    }

    /// Write all configuration/address registers, then start the decoder by
    /// writing `SWREG1` (the enable bit) last, so the engine never starts before
    /// its inputs are programmed. `SWREG0` (read-only id) is skipped.
    pub fn program_and_start(&mut self, regs: &[u32; registers::REG_COUNT]) {
        for (i, &val) in regs.iter().enumerate().skip(2) {
            self.mmio.write32(offset(i), val);
        }
        self.mmio
            .write32(offset(registers::REG_INT), regs[registers::REG_INT]);
    }

    /// Poll `SWREG1` until done or error, bounded by `timeout` (same unit as `clock`).
    pub fn poll_complete<C: FnMut() -> u64>(
        &self,
        clock: &mut C,
        timeout: u64,
    ) -> Result<DecodeStatus, JpuError> {
        let start = clock();
        loop {
            let status = DecodeStatus::from_int(self.mmio.read32(offset(registers::REG_INT)));
            if let Some(err) = status.error() {
                return Err(JpuError::Decode(err));
            }
            if status.is_done() {
                return Ok(status);
            }
            if clock().wrapping_sub(start) >= timeout {
                return Err(JpuError::DecodeTimeout);
            }
        }
    }

    /// Clear the `SWREG1` completion state after handling a run.
    ///
    /// Read-modify-write: it writes back the currently-set write-1-to-clear status
    /// bits (clearing them) while writing 0 to every control bit (`dec_e`,
    /// `irq_dis`, `timeout_e`, `buf_empty_e`), which also disables the engine.
    pub fn clear_status(&mut self) {
        let v = self.mmio.read32(offset(registers::REG_INT));
        self.mmio.write32(
            offset(registers::REG_INT),
            v & registers::INT_STATUS_CLEAR_MASK,
        );
    }

    /// Build the register array for `info`, patch in the buffer addresses, start
    /// the decode and wait for completion.
    pub fn decode<C: FnMut() -> u64>(
        &mut self,
        info: &JpegInfo,
        addrs: DecodeAddrs,
        clock: &mut C,
        timeout: u64,
    ) -> Result<DecodeStatus, JpuError> {
        let mut regs = command::build_reg_array(info);
        let hw_strm_offset = info.strm_offset - info.strm_offset % 16;
        regs[registers::REG_QTBL_BASE] = addrs.table_phys + command::QUANT_TBL_OFFSET as u32;
        regs[registers::REG_HUFFMIN_BASE] = addrs.table_phys + command::MINCODE_TBL_OFFSET as u32;
        regs[registers::REG_HUFFVAL_BASE] = addrs.table_phys + command::VALUE_TBL_OFFSET as u32;
        regs[registers::REG_STRM_BASE] = addrs.stream_phys + hw_strm_offset;
        regs[registers::REG_DEC_OUT_BASE] = addrs.output_phys;
        self.program_and_start(&regs);
        // Always clear the status bits (W1C) — even on error/timeout — so stale
        // done/error bits can't be observed by the next decode.
        let result = self.poll_complete(clock, timeout);
        self.clear_status();
        // On error or timeout the engine may be wedged mid-transaction; soft-reset
        // it (best-effort) so the next decode starts from a clean state.
        if result.is_err() {
            let _ = self.soft_reset(clock, timeout);
        }
        result
    }

    /// Run a pre-built register array (as supplied over the MPP ABI, with the
    /// enable bit already in `SWREG1`): program, start, wait for done-or-error,
    /// then copy the whole register file into `readback`. On timeout, `readback`
    /// is still populated for diagnostics before the error is returned.
    pub fn run_raw<C: FnMut() -> u64>(
        &mut self,
        regs: &[u32; registers::REG_COUNT],
        readback: &mut [u32; registers::REG_COUNT],
        clock: &mut C,
        timeout: u64,
    ) -> Result<DecodeStatus, JpuError> {
        self.program_and_start(regs);
        let result = self.poll_terminal(clock, timeout);
        self.read_regs(readback);
        // Always clear status (even on timeout) so stale bits don't leak.
        self.clear_status();
        // Recover a possibly-wedged engine after a timeout or a hardware error
        // (`poll_terminal` returns a hardware error in `status`, not as `Err`).
        let needs_reset = match &result {
            Err(_) => true,
            Ok(status) => status.error().is_some(),
        };
        if needs_reset {
            let _ = self.soft_reset(clock, timeout);
        }
        result
    }

    /// Copy the full register file into `out`.
    pub fn read_regs(&self, out: &mut [u32; registers::REG_COUNT]) {
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = self.mmio.read32(offset(i));
        }
    }

    /// Poll `SWREG1` until done OR error (returned as status); error only on timeout.
    fn poll_terminal<C: FnMut() -> u64>(
        &self,
        clock: &mut C,
        timeout: u64,
    ) -> Result<DecodeStatus, JpuError> {
        let start = clock();
        loop {
            let status = DecodeStatus::from_int(self.mmio.read32(offset(registers::REG_INT)));
            if status.is_done() || status.error().is_some() {
                return Ok(status);
            }
            if clock().wrapping_sub(start) >= timeout {
                return Err(JpuError::DecodeTimeout);
            }
        }
    }
}

/// Errors from the JPEG decode self-test path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelftestError {
    /// The JPEG header could not be parsed.
    Parse(parser::ParseError),
    /// A DMA buffer allocation failed.
    Alloc,
    /// The hardware decode failed.
    Decode(JpuError),
}

/// A probed RK3588 JPEG decoder device: the hardware engine plus a DMA
/// capability for allocating contiguous decode buffers.
pub struct RockchipJpeg {
    core: JpuCore<RawMmio>,
    dma: DeviceDma,
}

impl RockchipJpeg {
    /// Build a device over the mapped register `base` and a DMA capability.
    pub fn new(base: NonNull<u8>, dma: DeviceDma) -> Self {
        Self {
            core: JpuCore::new(RawMmio::new(base)),
            dma,
        }
    }

    /// Access the underlying hardware engine.
    pub fn core(&mut self) -> &mut JpuCore<RawMmio> {
        &mut self.core
    }

    /// Read the hardware ID register (upper half is `prod_num`).
    pub fn read_id(&self) -> u32 {
        self.core.read_id()
    }

    /// Decode a baseline JPEG into a freshly-allocated NV12 buffer using
    /// contiguous (IOMMU-bypass) DMA buffers; returns the decode status.
    pub fn decode_jpeg<C: FnMut() -> u64>(
        &mut self,
        jpeg: &[u8],
        clock: &mut C,
        timeout: u64,
    ) -> Result<DecodeStatus, SelftestError> {
        let info = parser::parse(jpeg).map_err(SelftestError::Parse)?;

        let mut stream = self
            .dma
            .coherent_array_zero::<u8>(jpeg.len())
            .map_err(|_| SelftestError::Alloc)?;
        stream.copy_from_slice_cpu(jpeg);

        let mut table_buf = [0u8; command::TABLE_SIZE];
        command::build_table_buffer(&info, &mut table_buf);
        let mut table = self
            .dma
            .coherent_array_zero::<u8>(command::TABLE_SIZE)
            .map_err(|_| SelftestError::Alloc)?;
        table.copy_from_slice_cpu(&table_buf);

        let out_w = (info.width as usize + 15) & !15;
        let out_h = (info.height as usize + 15) & !15;
        let out_size = out_w * out_h * 3 / 2;
        let output = self
            .dma
            .coherent_array_zero::<u8>(out_size)
            .map_err(|_| SelftestError::Alloc)?;

        let addrs = DecodeAddrs {
            table_phys: table.dma_addr().as_u64() as u32,
            stream_phys: stream.dma_addr().as_u64() as u32,
            output_phys: output.dma_addr().as_u64() as u32,
        };

        // Buffers stay alive across the synchronous (poll-to-completion) decode.
        let status = self
            .core
            .decode(&info, addrs, clock, timeout)
            .map_err(SelftestError::Decode)?;
        drop((stream, table, output));
        Ok(status)
    }
}

impl rdif_base::DriverGeneric for RockchipJpeg {
    fn name(&self) -> &str {
        "rockchip-jpeg"
    }
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;
    use std::vec::Vec;

    use super::*;
    use crate::registers::*;

    /// Fake MMIO: stored registers plus a scripted sequence of `SWREG1` reads
    /// (to simulate the hardware progressing to done/error), and a recorded
    /// write order (to assert the start register is written last).
    struct FakeMmio {
        regs: Cell<[u32; 64]>,
        int_reads: Vec<u32>,
        int_idx: Cell<usize>,
        writes: core::cell::RefCell<Vec<usize>>,
    }

    impl FakeMmio {
        fn new(int_reads: Vec<u32>) -> Self {
            Self {
                regs: Cell::new([0; 64]),
                int_reads,
                int_idx: Cell::new(0),
                writes: core::cell::RefCell::new(Vec::new()),
            }
        }

        fn reg(&self, index: usize) -> u32 {
            self.regs.get()[index]
        }
    }

    impl JpuMmio for FakeMmio {
        fn read32(&self, off: usize) -> u32 {
            if off == offset(REG_INT) && !self.int_reads.is_empty() {
                let i = self.int_idx.get();
                let v = self.int_reads[i.min(self.int_reads.len() - 1)];
                self.int_idx.set(i + 1);
                return v;
            }
            self.regs.get()[off / 4]
        }

        fn write32(&mut self, off: usize, value: u32) {
            let mut r = self.regs.get();
            r[off / 4] = value;
            self.regs.set(r);
            self.writes.borrow_mut().push(off / 4);
        }
    }

    /// A clock that advances by 1us per call.
    fn ticking_clock() -> impl FnMut() -> u64 {
        let mut t = 0u64;
        move || {
            let now = t;
            t += 1;
            now
        }
    }

    fn fixture_420() -> JpegInfo {
        use crate::parser::{Component, YuvMode};
        let mut info = JpegInfo::zeroed();
        info.width = 64;
        info.height = 48;
        info.nb_components = 3;
        info.yuv_mode = YuvMode::Yuv420;
        info.qtbl_entry = 2;
        info.htbl_entry = 0x0f;
        info.strm_offset = 44;
        info.pkt_len = 200;
        info.components[0] = Component {
            id: 1,
            h: 2,
            v: 2,
            quant_index: 0,
            dc_index: 0,
            ac_index: 0,
        };
        info.components[1] = Component {
            id: 2,
            h: 1,
            v: 1,
            quant_index: 1,
            dc_index: 1,
            ac_index: 1,
        };
        info.components[2] = Component {
            id: 3,
            h: 1,
            v: 1,
            quant_index: 1,
            dc_index: 1,
            ac_index: 1,
        };
        info
    }

    #[test]
    fn read_id_returns_register_zero() {
        let fake = FakeMmio::new(Vec::new());
        let mut r = fake.regs.get();
        r[REG_ID] = 0x1234_0000;
        fake.regs.set(r);
        let core = JpuCore::new(fake);
        assert_eq!(core.read_id(), 0x1234_0000);
    }

    #[test]
    fn soft_reset_succeeds_when_ready_bit_set() {
        let fake = FakeMmio::new(std::vec![INT_SOFTRESET_RDY]);
        let mut core = JpuCore::new(fake);
        let mut clock = ticking_clock();
        assert_eq!(core.soft_reset(&mut clock, 1000), Ok(()));
    }

    #[test]
    fn soft_reset_times_out_when_never_ready() {
        let fake = FakeMmio::new(std::vec![0]);
        let mut core = JpuCore::new(fake);
        let mut clock = ticking_clock();
        assert_eq!(core.soft_reset(&mut clock, 10), Err(JpuError::ResetTimeout));
    }

    #[test]
    fn poll_complete_returns_done() {
        let fake = FakeMmio::new(std::vec![INT_IRQ | INT_RDY_STA]);
        let core = JpuCore::new(fake);
        let mut clock = ticking_clock();
        let st = core.poll_complete(&mut clock, 1000).unwrap();
        assert!(st.is_success());
    }

    #[test]
    fn poll_complete_returns_error() {
        let fake = FakeMmio::new(std::vec![INT_BUS_STA]);
        let core = JpuCore::new(fake);
        let mut clock = ticking_clock();
        assert_eq!(
            core.poll_complete(&mut clock, 1000),
            Err(JpuError::Decode(DecodeError::BusError))
        );
    }

    #[test]
    fn poll_complete_times_out() {
        let fake = FakeMmio::new(std::vec![0]);
        let core = JpuCore::new(fake);
        let mut clock = ticking_clock();
        assert_eq!(
            core.poll_complete(&mut clock, 5),
            Err(JpuError::DecodeTimeout)
        );
    }

    #[test]
    fn decode_patches_addresses_and_starts_last() {
        let fake = FakeMmio::new(std::vec![INT_IRQ | INT_RDY_STA]);
        let mut core = JpuCore::new(fake);
        let mut clock = ticking_clock();
        let addrs = DecodeAddrs {
            table_phys: 0x1000_0000,
            stream_phys: 0x2000_0000,
            output_phys: 0x3000_0000,
        };
        let st = core
            .decode(&fixture_420(), addrs, &mut clock, 1000)
            .unwrap();
        assert!(st.is_success());
        // Address registers patched (stream gets the 16-byte-floored offset of 44 -> 32).
        assert_eq!(core.mmio.reg(REG_QTBL_BASE), 0x1000_0000);
        assert_eq!(core.mmio.reg(REG_HUFFMIN_BASE), 0x1000_0000 + 384);
        assert_eq!(core.mmio.reg(REG_HUFFVAL_BASE), 0x1000_0000 + 704);
        assert_eq!(core.mmio.reg(REG_STRM_BASE), 0x2000_0000 + 32);
        assert_eq!(core.mmio.reg(REG_DEC_OUT_BASE), 0x3000_0000);
    }

    #[test]
    fn run_raw_programs_polls_and_reads_back() {
        let fake = FakeMmio::new(std::vec![INT_IRQ | INT_RDY_STA]);
        let mut core = JpuCore::new(fake);
        let mut clock = ticking_clock();
        let mut regs = [0u32; REG_COUNT];
        regs[REG_PIC_SIZE] = 0x002f_003f;
        regs[REG_QTBL_BASE] = 0x1000_0000;
        regs[REG_INT] = INT_DEC_E;
        let mut readback = [0u32; REG_COUNT];
        let st = core
            .run_raw(&regs, &mut readback, &mut clock, 1000)
            .unwrap();
        assert!(st.is_success());
        assert_eq!(readback[REG_PIC_SIZE], 0x002f_003f);
        assert_eq!(readback[REG_QTBL_BASE], 0x1000_0000);
        assert_eq!(readback[REG_INT], INT_IRQ | INT_RDY_STA);
    }

    #[test]
    fn run_raw_reports_hw_error_via_status() {
        let fake = FakeMmio::new(std::vec![INT_ERROR_STA]);
        let mut core = JpuCore::new(fake);
        let mut clock = ticking_clock();
        let mut readback = [0u32; REG_COUNT];
        let st = core
            .run_raw(&[0u32; REG_COUNT], &mut readback, &mut clock, 1000)
            .unwrap();
        assert_eq!(st.error(), Some(DecodeError::StreamError));
        assert_eq!(readback[REG_INT], INT_ERROR_STA);
    }

    #[test]
    fn run_raw_times_out() {
        let fake = FakeMmio::new(std::vec![0]);
        let mut core = JpuCore::new(fake);
        let mut clock = ticking_clock();
        let mut readback = [0u32; REG_COUNT];
        assert_eq!(
            core.run_raw(&[0u32; REG_COUNT], &mut readback, &mut clock, 5),
            Err(JpuError::DecodeTimeout)
        );
    }

    #[test]
    fn run_raw_soft_resets_engine_after_timeout() {
        // After a timeout the recovery path pulses a soft-reset, so the final
        // SWREG1 write is the soft-reset bit.
        let fake = FakeMmio::new(std::vec![0]);
        let mut core = JpuCore::new(fake);
        let mut clock = ticking_clock();
        let mut readback = [0u32; REG_COUNT];
        assert_eq!(
            core.run_raw(&[0u32; REG_COUNT], &mut readback, &mut clock, 5),
            Err(JpuError::DecodeTimeout)
        );
        assert_eq!(core.mmio.reg(REG_INT), INT_SOFTRESET);
    }

    #[test]
    fn program_and_start_writes_enable_after_all_inputs() {
        let mut core = JpuCore::new(FakeMmio::new(Vec::new()));
        let mut regs = [0u32; REG_COUNT];
        regs[REG_INT] = INT_DEC_E;
        core.program_and_start(&regs);
        let writes = core.mmio.writes.borrow();
        let n = writes.len();
        // SWREG1 (enable) is written exactly once, and it is the final write so
        // the engine never starts before its config/address registers are set.
        assert_eq!(writes.iter().filter(|&&i| i == REG_INT).count(), 1);
        assert_eq!(writes[n - 1], REG_INT);
        // Address slots are written before the enable.
        for slot in [
            REG_QTBL_BASE,
            REG_HUFFMIN_BASE,
            REG_HUFFVAL_BASE,
            REG_STRM_BASE,
            REG_DEC_OUT_BASE,
        ] {
            let pos = writes.iter().position(|&i| i == slot).unwrap();
            assert!(pos < n - 1);
        }
    }
}
