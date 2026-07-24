// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! RK3588 A55 (`vdd_cpu_lit`) CPU-rail voltage lever — RK806 PMIC over SPI2.
//!
//! DVFS Phase 2, little cluster. Phase 1a raises the A55 clock via SCMI only;
//! the RK3588 CPU clock is voltage-coupled, so exact frequency + power needs the
//! per-OPP rail voltage that only the PMIC can set. On the OrangePi-5-Plus the
//! A55 rail `vdd_cpu_lit_s0` is provided by an **RK806** PMIC on **SPI2**
//! (`spi@feb20000`, chip-select 0). This module is a minimal polling SPI master
//! plus the RK806 buck-2 regulator access needed to read and (safely, down-only)
//! lower that rail.
//!
//! # Board-confirmed ground truth (sources cited inline)
//!
//! - **SPI2 controller** = `spi@feb20000`, size `0x1000`, compatible
//!   `rockchip,rk3066-spi` (the standard Rockchip SPI IP). Register map and the
//!   bare-metal polling flow follow mainline `drivers/spi/spi-rockchip.c` and
//!   U-Boot `drivers/spi/rk_spi.c` (`rk_spi.h`).
//! - **RK806** = `rockchip,rk806`, CS 0, `spi-max-frequency = 1_000_000`, mode 0.
//!   SPI framing from mainline `drivers/mfd/rk8xx-spi.c`
//!   (`rk806_spi_bus_read`/`rk806_spi_bus_write`) + constants in
//!   `include/linux/mfd/rk808.h`.
//! - **`vdd_cpu_lit_s0` = RK806 `dcdc-reg2` (buck #2)** — from
//!   `arch/arm64/boot/dts/rockchip/rk3588-orangepi-5.dtsi`. Its running voltage
//!   selector is `RK806_BUCK2_ON_VSEL = 0x1B`, full-byte selector
//!   (`vsel_mask = 0xff`), encoded by `rk806_buck_voltage_ranges` in
//!   `drivers/regulator/rk808-regulator.c`:
//!   `V(uV) = 500000 + sel*6250` for `sel 0..=159` (500 mV..1500 mV, our whole
//!   CPU window). 675 mV = sel 28 (`0x1C`), 750 mV = sel 40 (`0x28`),
//!   800 mV = sel 48 (`0x30`).
//!
//! # Safety model
//!
//! Direction is **down only**, toward Linux-proven OPP nominals. [`set_uv`]
//! refuses any target below the [`A55_MIN_UV`] floor (675 mV) or **above the
//! boot voltage read back from the rail** — it can never raise vdd. Every write
//! is read-back verified (raw selector byte compared), and a mismatch aborts and
//! returns `false`, leaving the last confirmed selector in place.
//! [`set_uv_stepped`] lowers in `<=`[`MAX_STEP_UV`] (25 mV) increments with a
//! settle delay so the voltage-coupled clock tracks down gradually.
//!
//! After U-Boot, SPI2 is left with its clocks gated **and** its controller held
//! in soft-reset, so [`init`] first **ungates `PCLK_SPI2` + `CLK_SPI2`**,
//! **de-asserts the SPI2 P+core soft-reset**, and **applies the full SPI2
//! `pinctrl-0`** (mux + pull + drive + schmitt, via the in-tree rockchip pinctrl
//! driver — a mux-only match is not enough; the pad config is what the RK806 link
//! needs, mirroring the i2c0 fix), then verifies the controller responds (a gated
//! APB reads all-zero, and a core still in reset transacts all-zero — either
//! would otherwise clock in a fabricated `0`); if it does not respond it stays
//! **unbound**. It also logs a one-shot read-only RK806 reachability probe
//! (chip-ID + raw DCDC2 frame). Every SPI poll loop is time-bounded
//! ([`SPI_TIMEOUT_NS`]) and treats an all-zero status register as a dead bus, so
//! a controller that dies after init makes [`get_uv`] return `None` rather than
//! hanging or fabricating a reading. `init`'s only SoC writes are to the SPI2
//! clock gate, soft-reset, and pin config; **no PMIC write** in `init`.
//!
//! # Integration (owned by the cpufreq probe, not wired here)
//!
//! This module performs **no** PMIC write on its own. Mirroring the A76 I2C
//! sibling, [`init`] maps the controller itself; the probe then reads/steps the
//! rail:
//!
//! ```ignore
//! use crate::soc::rockchip::pmic_spi;
//!
//! pmic_spi::init();                          // map + configure SPI2/RK806
//! let boot_uv = pmic_spi::get_uv();          // read A55 boot vdd first
//! // ... only after the SCMI clock is already at target:
//! pmic_spi::set_uv_stepped(675_000);         // down-only to the OPP nominal
//! ```

use core::time::Duration;

// PMIC access is slow (SPI register reads/writes that busy-wait on hardware), so
// the lock is held across the whole transaction. Use `SpinNoPreempt`, not
// `SpinNoIrq`: it keeps local IRQs ENABLED during the poll (so IRQ latency is not
// held hostage to a millisecond SPI transaction) while still disabling preemption
// so no task switch can interleave mid-transaction and corrupt the register
// sequence. This is sound because the lock is NEVER taken from an interrupt
// handler — every caller (`init`/`get_uv`/`set_uv*`/`force_write_dcdc2`) runs in
// the boot probe or the sleepable `cpufreq` governor task, so an IRQ arriving
// mid-transaction can never re-enter and self-deadlock.
use ax_kspin::SpinNoPreempt as Mutex;
use log::{info, warn};
use rdif_pinctrl::PinctrlDevice;

use crate::mmio::iomap;

// ---------------------------------------------------------------------------
// SPI2 controller (rockchip,rk3066-spi) — spi@feb20000
// ---------------------------------------------------------------------------

/// Physical base of the RK3588 SPI2 controller (`spi@feb20000`).
pub const RK3588_SPI2_BASE: usize = 0xfeb2_0000;
/// MMIO window size of the SPI2 controller.
pub const RK3588_SPI2_SIZE: usize = 0x1000;
/// Fallback SPI2 functional-clock rate for the baud divider, used only if
/// U-Boot left no usable divisor (see [`Rk806Spi::configure`]).
///
/// The OrangePi-5-Plus DT sets `assigned-clock-rates = <200000000>` for
/// `CLK_SPI2`; 200 MHz is also the RK3588 SPI source ceiling, so assuming it can
/// only *under*-shoot the target SCLK (safe: slower, still <= 1 MHz).
const RK3588_SPI2_FALLBACK_INPUT_HZ: u32 = 200_000_000;

/// Target serial clock for the RK806 (DT `spi-max-frequency = 1_000_000`).
const SPI_SCLK_HZ: u32 = 1_000_000;
/// RK806 chip-select index on SPI2.
const RK806_CS: u32 = 0;

// Register offsets (spi-rockchip.c ROCKCHIP_SPI_* / rk_spi.h struct layout).
const SPI_CTRLR0: usize = 0x0000;
const SPI_CTRLR1: usize = 0x0004;
const SPI_ENR: usize = 0x0008;
const SPI_SER: usize = 0x000c;
const SPI_BAUDR: usize = 0x0010;
const SPI_RXFTLR: usize = 0x0018;
const SPI_SR: usize = 0x0024;
const SPI_IMR: usize = 0x002c;
const SPI_ICR: usize = 0x0038;
const SPI_TXDR: usize = 0x0400;
const SPI_RXDR: usize = 0x0800;

// CTRLR0 fields (rk_spi.h). We now use the EXACT value Linux `spi-rockchip.c`
// programs on this board (read live: 0x2C01), since Linux reads the RK806 with
// it — master, 8-bit frame, mode 0, transmit+receive, EM_BIG.
const CR0_DFS_8BIT: u32 = 0x1; // [1:0] data frame size = 8 bit
const CR0_SSN_DELAY_ONE: u32 = 0x1 << 10; // one sclk CS-to-clk delay
const CR0_EM_BIG: u32 = 0x1 << 11; // endian: big (matches Linux spi-rockchip.c)
const CR0_HALF_WORD_OFF: u32 = 0x1 << 13; // APB 8-bit / SPI 8-bit access
// FRF_SPI (0<<16), TMOD_TR (0<<18), OMOD_MASTER (0<<20), FBM_MSB (0<<12),
// SCPH/SCPOL (mode 0 => 0<<6/0<<7), RXD sample delay 0 all contribute zero.
const SPI_CTRLR0_MODE0_8BIT: u32 =
    CR0_DFS_8BIT | CR0_SSN_DELAY_ONE | CR0_EM_BIG | CR0_HALF_WORD_OFF;

// Status register bits (rk_spi.h).
const SR_BUSY: u32 = 1 << 0;
const SR_TF_FULL: u32 = 1 << 1;
const SR_RF_EMPT: u32 = 1 << 3;

const BAUDR_MIN: u32 = 2;
const BAUDR_MAX: u32 = 0xfffe;

// Main RK3588 CRU (write-masked clock/reset registers). U-Boot gates the SPI2
// clocks after using the RK806 at boot, so `init` must ungate them first — a
// gated APB reads all-zero, which the controller-alive check below rejects.
/// Main RK3588 CRU base.
const RK3588_CRU_BASE: usize = 0xfd7c_0000;
/// CRU window mapped for the ungate (only the first page is needed).
const RK3588_CRU_SIZE: usize = 0x1000;
/// `clkgate_con(14)` (`14*4 + 0x800`): holds the SPI2 gate bits.
const CRU_CLKGATE_CON14: usize = 0x838;
/// Write-masked ungate of `PCLK_SPI2` (bit 8) + `CLK_SPI2` (bit 13): high half
/// is the write-enable mask, low half is the value (0 == clock enabled on a
/// Rockchip CRU gate). == `0x2100_0000`.
const SPI2_CLK_UNGATE: u32 = ((1 << 8) | (1 << 13)) << 16;

/// Upper bound on any SPI poll loop. Generous vs. the ~40 us a 4-byte 1 MHz
/// frame takes; a dead SPI clock trips this instead of hanging.
const SPI_TIMEOUT_NS: u64 = 20_000_000; // 20 ms

/// Largest RK806 transaction this module issues: cmd + 2 addr + 1 data.
const RK806_FRAME_LEN: usize = 4;

// ---------------------------------------------------------------------------
// RK806 SPI framing (rk8xx-spi.c / rk808.h)
// ---------------------------------------------------------------------------

/// Command byte: write. `RK806_CMD_WRITE = BIT(7)`; CRC disabled (`= 0`); the
/// low nibble carries `value_bytes - 1`, which is 0 for our single-byte access.
const RK806_CMD_WRITE: u8 = 0x80;
/// Command byte: read. `RK806_CMD_READ = 0`; CRC disabled; low nibble 0.
const RK806_CMD_READ: u8 = 0x00;

/// RK806 buck-2 (`vdd_cpu_lit_s0`) running-voltage selector register.
const RK806_BUCK2_ON_VSEL: u8 = 0x1B;

/// RK806 chip-name / chip-version registers (rk808.h `RK806_CHIP_NAME`/`_VER`).
/// Both carry nonzero power-on defaults, so a nonzero read proves the SPI path
/// actually reaches the RK806 (vs. reading fabricated zeros off a floating MISO).
const RK806_CHIP_NAME: u8 = 0x5A;
const RK806_CHIP_VER: u8 = 0x5B;

// --- SPI2 controller soft-reset (RK3588 main CRU) -------------------------
// Board-observed (boot #2): after ungate the controller is clocked and its APB
// registers stick (`is_alive` passes), yet the serial transaction reads all-zero
// — the classic signature of the controller **core** held in soft-reset: the
// PCLK-domain reset is released (registers work) but the shift-engine reset is
// held, so nothing actually transacts. Same split the i2c0 sibling hit. SPI2's
// resets live in the MAIN CRU: SRST_P_SPI2 (PCLK) = bit 8 and SRST_SPI2 (core) =
// bit 13 of `softrst_con(14)`, from mainline `drivers/clk/rockchip/rst-rk3588.c`
// (`RK3588_CRU_RESET_OFFSET(SRST_P_SPI2,14,8)` / `(SRST_SPI2,14,13)`) — same bit
// layout as the clock gate. We de-assert both (0 == released); the core reset
// (bit 13) is the one that matches the symptom.
/// `softrst_con(14)` (`14*4 + 0xa00`): holds the SPI2 reset bits.
const CRU_SOFTRST_CON14: usize = 0xa38;
/// Write-masked de-assert of SRST_P_SPI2 (bit 8) + SRST_SPI2 (bit 13): high half
/// = write mask, low half = 0 (0 == reset released). Numerically equals the gate
/// ungate value (same bit positions) but targets a different register.
const SPI2_RST_DEASSERT: u32 = ((1 << 8) | (1 << 13)) << 16;
/// Settle after releasing the reset before touching the controller.
const RST_SETTLE_US: u64 = 20;

// RK3588 PMU1_IOC (@ 0xfd5f0000) GPIO0 A/B pad config, decoded from u-boot
// pinctrl-rk3588.c tables (verified against Linux's live values):
//   mux:   0x04 = GPIO0A_H (A4-7), 0x08 = GPIO0B_L (B0-3)  [4 bits/pin]
//   drive: 0x14 = GPIO0A_H,        0x18 = GPIO0B_L         [4 bits/pin]
//   pull:  0x20 = GPIO0A,          0x24 = GPIO0B           [2 bits/pin]
//   smt:   0x30 = GPIO0A,          0x34 = GPIO0B           [1 bit/pin]
// SPI2 pins: A5=CLK, A6=MOSI (in 0x04/0x14/0x20/0x30), B1=CS0, B3=MISO (in
// 0x08/0x18/0x24/0x34). The read-only dump below logs this whole region so it
// can be diffed against Linux's known-good reference to find the B3/MISO delta.
const RK3588_IOC_BASE: usize = 0xfd5f_0000;
const RK3588_IOC_SIZE: usize = 0x1000;

// ---------------------------------------------------------------------------
// RK806 buck voltage encoding (rk806_buck_voltage_ranges, range 1)
// ---------------------------------------------------------------------------

/// Range-1 base: selector 0 == 500 mV.
const RK806_VSEL_BASE_UV: u32 = 500_000;
/// Range-1 step: 6.25 mV per selector unit.
const RK806_VSEL_STEP_UV: u32 = 6_250;
/// Last selector of range 1 (== 1_493_750 uV; selector 160 is the range-2 base
/// at 1_500_000 uV). Linux's "500mV ~ 1500mV" range comment is approximate.
const RK806_VSEL_R1_MAX_SEL: u8 = 159;
/// Range-2 base (selector 160 == 1_500_000 uV), 25 mV step, up to selector 235.
const RK806_VSEL_R2_BASE_UV: u32 = 1_500_000;
const RK806_VSEL_R2_BASE_SEL: u8 = 160;
const RK806_VSEL_R2_STEP_UV: u32 = 25_000;
const RK806_VSEL_R2_MAX_SEL: u8 = 235;
/// Range-3: selector 236..=255 is a fixed 3_400_000 uV.
const RK806_VSEL_R3_UV: u32 = 3_400_000;

// ---------------------------------------------------------------------------
// Safety envelope for the A55 rail
// ---------------------------------------------------------------------------

/// Hard voltage floor for the A55 rail: the lowest Phase-1a OPP nominal
/// (1008 MHz -> 675 mV, standard SKU). Never set below this. Industrial J/M SKU
/// low OPPs sit at 750 mV; a boot read of ~750 is expected there and is fine.
pub const A55_MIN_UV: u32 = 675_000;
/// Maximum single-step magnitude for [`set_uv_stepped`] (25 mV == 4 selectors).
pub const MAX_STEP_UV: u32 = 25_000;
/// Settle delay between stepped-lowering writes (>> the 25 mV ramp time at the
/// DT `regulator-ramp-delay = 12500` uV/us => ~2 us).
const STEP_SETTLE_US: u64 = 150;

// ---------------------------------------------------------------------------
// Controller handle + global state
// ---------------------------------------------------------------------------

/// A mapped, initialized SPI2 controller bound to the RK806.
struct Rk806Spi {
    base: *mut u8,
}

// The controller is reached only through `PMIC` (a `SpinNoPreempt` mutex), which
// serializes all access; the MMIO mapping is stable for the kernel lifetime.
unsafe impl Send for Rk806Spi {}

/// Global RK806/SPI2 handle, populated once by [`init`].
static PMIC: Mutex<Option<Rk806Spi>> = Mutex::new(None);

impl Rk806Spi {
    #[inline]
    fn read(&self, off: usize) -> u32 {
        unsafe { self.base.add(off).cast::<u32>().read_volatile() }
    }

    #[inline]
    fn write(&self, off: usize, val: u32) {
        unsafe { self.base.add(off).cast::<u32>().write_volatile(val) }
    }

    /// One-time controller setup: master, mode 0, 8-bit frames, ~1 MHz SCLK.
    /// Idempotent; leaves the controller disabled (transfers enable per frame).
    ///
    /// U-Boot already drives the RK806 over SPI2 at boot, so its BAUDR is a
    /// proven-good ~1 MHz divisor — preserve it (like the I2C sibling preserves
    /// `CLKDIV`) and only compute one if the controller was left unconfigured.
    /// This removes any dependence on knowing the exact `CLK_SPI2` rate.
    fn configure(&self) {
        // Disable before touching CTRLR0/BAUDR (config is latched while idle).
        self.write(SPI_ENR, 0);
        self.write(SPI_SER, 0);

        let baud = self.read(SPI_BAUDR) & 0xffff;
        if (BAUDR_MIN..=BAUDR_MAX).contains(&baud) && (baud & 1) == 0 {
            info!("pmic_spi: SPI2 baud divisor preserved from U-Boot ({baud})");
        } else {
            let computed = baud_divider(RK3588_SPI2_FALLBACK_INPUT_HZ, SPI_SCLK_HZ);
            self.write(SPI_BAUDR, computed);
            info!("pmic_spi: SPI2 baud divisor computed {computed} (U-Boot left {baud:#x})");
        }

        self.write(SPI_CTRLR0, SPI_CTRLR0_MODE0_8BIT);
        // Mask + clear any interrupts; this is a poll-only master.
        self.write(SPI_IMR, 0);
        self.write(SPI_ICR, 0xffff_ffff);
    }

    /// Confirm the controller actually responds on its APB. `configure` always
    /// writes the fixed `SPI_CTRLR0_MODE0_8BIT`, so those bits reading back
    /// proves the bus is clocked and out of reset. A still-gated (or reset-held)
    /// controller reads 0 here, which fails the mask check — this is what closes
    /// the "gated APB looks like an empty-but-working bus" false-success hole.
    fn is_alive(&self) -> bool {
        self.read(SPI_CTRLR0) & SPI_CTRLR0_MODE0_8BIT == SPI_CTRLR0_MODE0_8BIT
    }

    /// Full-duplex byte exchange of `tx.len()` bytes, CS asserted for the whole
    /// transaction. Replicates Linux `spi-rockchip.c`'s exact PIO sequence (the
    /// board's known-good driver uses an identical CTRLR0 to ours yet reads the
    /// RK806, so the transfer *sequence* is the remaining variable): assert CS,
    /// program CTRLR0/CTRLR1/**RXFTLR** while disabled, clear interrupts, enable,
    /// **pre-fill the entire TX FIFO**, then drain RX. `rx[3]` holds the value.
    /// Returns `false` on timeout, leaving CS deasserted and the controller off.
    #[must_use]
    fn xfer(&self, tx: &[u8], rx: &mut [u8]) -> bool {
        self.xfer_cs(tx, rx, true)
    }

    /// As [`Rk806Spi::xfer`], but `native_cs` selects whether the controller's
    /// own CS (SER) is asserted. The GPIO-CS diagnostic drives the CS pin
    /// manually and calls this with `native_cs = false`.
    #[must_use]
    fn xfer_cs(&self, tx: &[u8], rx: &mut [u8], native_cs: bool) -> bool {
        debug_assert_eq!(tx.len(), rx.len());
        let len = tx.len();
        let n = len as u32;

        // set_cs first (Linux calls set_cs before the transfer).
        if native_cs {
            self.write(SPI_SER, 1 << RK806_CS);
        }

        // Per-transfer config while disabled: CTRLR0, frame count, and the RX
        // FIFO threshold (Linux sets RXFTLR = len-1 for small transfers; we
        // previously left it unset — a prime suspect).
        self.write(SPI_ENR, 0);
        self.write(SPI_CTRLR0, SPI_CTRLR0_MODE0_8BIT);
        self.write(SPI_CTRLR1, n.saturating_sub(1));
        self.write(SPI_RXFTLR, n.saturating_sub(1));
        self.write(SPI_ICR, 0xffff_ffff);
        self.write(SPI_ENR, 1);

        let deadline = now_ns() + SPI_TIMEOUT_NS;
        let mut ok = true;

        // Pre-fill the ENTIRE TX FIFO first (Linux fills then waits), rather than
        // interleaving TX/RX. The 4-byte frame fits the FIFO with room to spare.
        let mut ti = 0usize;
        while ti < len {
            let sr = self.read(SPI_SR);
            if sr == 0 {
                warn!("pmic_spi: SPI2 SR reads 0 (controller not clocking); aborting");
                ok = false;
                break;
            }
            if (sr & SR_TF_FULL) == 0 {
                self.write(SPI_TXDR, u32::from(tx[ti]));
                ti += 1;
            } else if now_ns() >= deadline {
                warn!("pmic_spi: SPI2 TX fill timed out ({ti}/{len})");
                ok = false;
                break;
            }
        }

        // Then drain the RX FIFO (one byte per clocked frame).
        let mut ri = 0usize;
        while ok && ri < len {
            if (self.read(SPI_SR) & SR_RF_EMPT) == 0 {
                rx[ri] = self.read(SPI_RXDR) as u8;
                ri += 1;
            } else if now_ns() >= deadline {
                warn!("pmic_spi: SPI2 RX drain timed out ({ri}/{len})");
                ok = false;
                break;
            }
        }

        // Wait for the shift engine to go idle before dropping CS.
        while ok && (self.read(SPI_SR) & SR_BUSY) != 0 {
            if now_ns() >= deadline {
                warn!("pmic_spi: SPI2 stayed BUSY after transfer");
                ok = false;
                break;
            }
        }

        // Deassert CS0 and disable.
        if native_cs {
            self.write(SPI_SER, 0);
        }
        self.write(SPI_ENR, 0);
        ok
    }

    /// Read one RK806 register (8-bit value, 16-bit little-endian address).
    /// Frame: `[READ, addr_lo, addr_hi=0, dummy]`; value is the last RX byte.
    fn rk806_read(&self, reg: u8) -> Option<u8> {
        let tx = [RK806_CMD_READ, reg, 0x00, 0x00];
        let mut rx = [0u8; RK806_FRAME_LEN];
        if self.xfer(&tx, &mut rx) {
            Some(rx[RK806_FRAME_LEN - 1])
        } else {
            None
        }
    }

    /// Write one RK806 register. Frame: `[WRITE, addr_lo, addr_hi=0, value]`.
    /// RX is ignored. Returns `false` on transfer timeout.
    #[must_use]
    fn rk806_write(&self, reg: u8, val: u8) -> bool {
        let tx = [RK806_CMD_WRITE, reg, 0x00, val];
        let mut rx = [0u8; RK806_FRAME_LEN];
        self.xfer(&tx, &mut rx)
    }

    /// Read-only one-shot reachability probe, logged at init. The RK806
    /// chip-name/version registers have nonzero power-on defaults, so a nonzero
    /// read proves the SPI path actually reaches the RK806; an all-zero read (and
    /// an all-zero DCDC2 frame) means it is not reached (pin-mux / CS). Also dumps
    /// the raw DCDC2 read frame for inspection. Performs no PMIC write.
    fn log_diagnostics(&self) {
        let name = self.rk806_read(RK806_CHIP_NAME);
        let ver = self.rk806_read(RK806_CHIP_VER);
        let tx = [RK806_CMD_READ, RK806_BUCK2_ON_VSEL, 0x00, 0x00];
        let mut rx = [0u8; RK806_FRAME_LEN];
        let ok = self.xfer(&tx, &mut rx);
        info!(
            "pmic_spi: RK806 probe: chip_name(0x5A)={name:?} chip_ver(0x5B)={ver:?} \
             dcdc2_raw_rx={rx:02x?} (xfer_ok={ok})"
        );
    }

    /// Read-only read-shape diagnostic (boot #8). The RK806 read now returns data
    /// but the RX mirrors the TX (e.g. chip-name read `tx=[00,5a,00,00]` came back
    /// `rx=[00,5a,00,00]`). Distinguish a MISO-reads-MOSI loopback from a
    /// value-position offset:
    /// - **distinct**: send a distinctive TX pattern; if the RX echoes it byte for
    ///   byte, MISO is reading MOSI (pad/routing), not the RK806.
    /// - **long6**: a 6-byte frame; if instead the register value lands at a later
    ///   wire byte (rx\[4]/rx\[5]), it is just a frame-length/index offset.
    ///
    /// No PMIC write.
    fn probe_read_shape(&self) {
        let tx1 = [RK806_CMD_READ, RK806_CHIP_NAME, 0xA5, 0x3C];
        let mut rx1 = [0u8; 4];
        let ok1 = self.xfer(&tx1, &mut rx1);
        info!("pmic_spi: read-shape distinct: tx={tx1:02x?} rx={rx1:02x?} ok={ok1}");

        let tx2 = [RK806_CMD_READ, RK806_CHIP_NAME, 0x00, 0x00, 0x00, 0x00];
        let mut rx2 = [0u8; 6];
        let ok2 = self.xfer(&tx2, &mut rx2);
        info!("pmic_spi: read-shape long6: tx={tx2:02x?} rx={rx2:02x?} ok={ok2}");
    }
}

// ---------------------------------------------------------------------------
// Baud + voltage math (pure, unit-tested on the host)
// ---------------------------------------------------------------------------

/// SPI baud divider: even, in `[2, 0xfffe]`, rounded so the resulting SCLK is
/// `<= target_hz` (mirrors U-Boot `rkspi_set_clk`: `DIV_ROUND_UP` then round up
/// to the next even value).
fn baud_divider(input_hz: u32, target_hz: u32) -> u32 {
    let target = target_hz.max(1);
    let mut div = input_hz.div_ceil(target);
    div = div.clamp(BAUDR_MIN, BAUDR_MAX);
    // Round up to the next even number (hardware requires an even divisor);
    // rounding up only lowers the clock, which is the safe direction.
    div = (div + 1) & 0xfffe;
    div.clamp(BAUDR_MIN, BAUDR_MAX)
}

/// Decode an RK806 buck selector byte to microvolts (all three ranges).
fn vsel_to_uv(sel: u8) -> u32 {
    if sel <= RK806_VSEL_R1_MAX_SEL {
        RK806_VSEL_BASE_UV + u32::from(sel) * RK806_VSEL_STEP_UV
    } else if sel <= RK806_VSEL_R2_MAX_SEL {
        RK806_VSEL_R2_BASE_UV + u32::from(sel - RK806_VSEL_R2_BASE_SEL) * RK806_VSEL_R2_STEP_UV
    } else {
        RK806_VSEL_R3_UV
    }
}

/// Encode microvolts to an RK806 buck selector byte, range 1 only (500 mV..
/// 1493.75 mV — spanning the entire CPU-rail window). Requires an **exact**
/// 6.25 mV-aligned value (like `pmic_i2c`'s encoder): returns `None` outside
/// range 1 or for any input that does not land exactly on a selector step, so
/// we never program an approximated voltage. Our OPP nominals (675/750/800 mV)
/// are all aligned.
fn uv_to_vsel(uv: u32) -> Option<u8> {
    let r1_max_uv = RK806_VSEL_BASE_UV + u32::from(RK806_VSEL_R1_MAX_SEL) * RK806_VSEL_STEP_UV;
    if uv < RK806_VSEL_BASE_UV || uv > r1_max_uv {
        return None;
    }
    let offset = uv - RK806_VSEL_BASE_UV;
    if !offset.is_multiple_of(RK806_VSEL_STEP_UV) {
        return None;
    }
    Some((offset / RK806_VSEL_STEP_UV) as u8)
}

// ---------------------------------------------------------------------------
// Time helpers
// ---------------------------------------------------------------------------

#[inline]
fn now_ns() -> u64 {
    axklib::time::monotonic_nanos()
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Ungate the SPI2 controller clocks (`PCLK_SPI2` + `CLK_SPI2`) in the main CRU.
///
/// Board-observed: U-Boot gates these after programming the RK806 at boot, so at
/// probe time the SPI2 APB is dead and reads all-zero — which without this looks
/// like a working-but-empty bus and yields a bogus `0` voltage. The Rockchip CRU
/// gate register is write-masked, so writing [`SPI2_CLK_UNGATE`] clears both gate
/// bits (0 == enabled). Returns `false` only if the CRU cannot be mapped.
fn ungate_spi2_clocks() -> bool {
    let cru = match iomap(RK3588_CRU_BASE, RK3588_CRU_SIZE) {
        Ok(v) => v,
        Err(err) => {
            warn!("pmic_spi: iomap CRU {RK3588_CRU_BASE:#x} failed: {err:?}");
            return false;
        }
    };
    // SAFETY: the CRU mapping covers `RK3588_CRU_SIZE` bytes and
    // `CRU_CLKGATE_CON14` (0x838) is within the first page. One write-masked
    // MMIO store; the gate register tolerates concurrent write-masked updates to
    // other bits, so no read-modify-write is needed.
    unsafe {
        cru.as_ptr()
            .add(CRU_CLKGATE_CON14)
            .cast::<u32>()
            .write_volatile(SPI2_CLK_UNGATE);
    }
    info!(
        "pmic_spi: ungated SPI2 PCLK+CLK (CRU {:#x} <= {SPI2_CLK_UNGATE:#010x})",
        RK3588_CRU_BASE + CRU_CLKGATE_CON14
    );
    true
}

/// Release the SPI2 controller from soft-reset in the main CRU so its shift
/// engine can transact. De-assert only (write masked zeros to SRST_P_SPI2 bit 8 +
/// SRST_SPI2 bit 13); we never assert, so nothing else is disturbed, and the
/// BAUDR/CTRLR0 that `configure` (re)writes afterwards take effect on a released
/// core. Idempotent (re-clearing an already-released reset is a no-op). Returns
/// `false` only if the CRU cannot be mapped.
fn deassert_spi2_reset() -> bool {
    let cru = match iomap(RK3588_CRU_BASE, RK3588_CRU_SIZE) {
        Ok(v) => v,
        Err(err) => {
            warn!("pmic_spi: iomap CRU {RK3588_CRU_BASE:#x} failed: {err:?}");
            return false;
        }
    };
    // SAFETY: the CRU mapping covers `RK3588_CRU_SIZE` bytes and
    // `CRU_SOFTRST_CON14` (0xa38) is within the first page. One write-masked MMIO
    // store clearing only the two SPI2 reset bits.
    unsafe {
        cru.as_ptr()
            .add(CRU_SOFTRST_CON14)
            .cast::<u32>()
            .write_volatile(SPI2_RST_DEASSERT);
    }
    axklib::time::busy_wait(Duration::from_micros(RST_SETTLE_US));
    info!(
        "pmic_spi: de-asserted SPI2 P+core reset (CRU {:#x} <= {SPI2_RST_DEASSERT:#010x})",
        RK3588_CRU_BASE + CRU_SOFTRST_CON14
    );
    true
}

/// Apply the FULL `spi@feb20000` pinctrl-0 (mux **+ pull + drive-strength +
/// schmitt**) via the registered Rockchip pinctrl driver — the same path that
/// fixed i2c0.
///
/// Board finding (boot #7): our previous mux-only check saw "already func1" yet
/// read AND write to the RK806 were both dead. i2c0 had the identical symptom and
/// was fixed by applying the *full* pin config, not just the mux — the DTS
/// pinconf flags (pull/drive/slew on `spi2m2_pins`/`spi2m2_cs0`) were u-boot's or
/// default, not what the RK806 link needs, so CLK/MOSI/CS never cleanly reached
/// the chip. Rather than hand-decode the pinconf, we apply the node's `pinctrl-0`
/// state through the in-tree pinctrl driver, which already encodes it. Pad-config
/// change only (no PMIC access); best-effort — any failure is logged and the
/// later transaction times out into a safe no-op.
fn set_spi2_pinmux() {
    let Some(pinctrl) = rdrive::get_one::<PinctrlDevice>() else {
        warn!("pmic_spi: PinctrlDevice not registered; cannot apply spi2 pinctrl");
        return;
    };
    let mut pinctrl = match pinctrl.lock() {
        Ok(guard) => guard,
        Err(err) => {
            warn!("pmic_spi: failed to lock PinctrlDevice: {err}; cannot apply spi2 pinctrl");
            return;
        }
    };
    let Some(fdt) = rdrive::with_fdt(Clone::clone) else {
        warn!("pmic_spi: live FDT not found; cannot apply spi2 pinctrl");
        return;
    };
    // Match the spi2 controller node by reg base (several SPI controllers share
    // the rk3066-spi compatible).
    let node = fdt
        .find_compatible(&["rockchip,rk3066-spi"])
        .into_iter()
        .find(|n| {
            n.regs()
                .into_iter()
                .next()
                .is_some_and(|r| r.address as usize == RK3588_SPI2_BASE)
        });
    let Some(node) = node else {
        warn!("pmic_spi: spi2 node @ {RK3588_SPI2_BASE:#x} not found in FDT; cannot apply pinctrl");
        return;
    };
    match pinctrl.apply_fdt_default_state(&fdt, node.as_node()) {
        Ok(()) => info!(
            "pmic_spi: applied spi2 pinctrl-0 (spi2m2 clk/mosi/miso/cs0 mux+pull+drive) via \
             rockchip pinctrl"
        ),
        Err(err) => warn!("pmic_spi: failed to apply spi2 pinctrl-0: {err:?}"),
    }

    // MISO loopback probe (last untested register): force GPIO0_B3 (= SPI2_MISO) to
    // INPUT direction. Linux leaves it input (GPIO0 DDR_L bit11 = 0); if StarryOS's
    // boot left B3 as a GPIO *output*, the pad would drive instead of sample, so the
    // controller reads its own TX shift register — exactly the observed rx==tx
    // loopback. The func-1 mux normally overrides GPIO direction, so this is a
    // long-shot, but it is the one register we have not yet forced. GPIO0 @ 0xfd8a0000,
    // SWPORT_DR_L=0x00, SWPORT_DDR_L=0x08 (write-masked: hi16 = per-bit mask).
    if let Ok(gpio0) = iomap(0xfd8a_0000, 0x100) {
        let p = gpio0.as_ptr();
        // SAFETY: GPIO0 mapping covers 0x100; DDR_L is at 0x08, within the page.
        let ddr = unsafe { p.add(0x08).cast::<u32>().read_volatile() };
        info!(
            "pmic_spi: GPIO0 DDR_L={:#010x} B3(bit11)={} (0=input) — forcing B3 input",
            ddr,
            (ddr >> 11) & 1
        );
        // Force B3 -> input: mask bit11 (hi16), value 0 (lo16).
        unsafe { p.add(0x08).cast::<u32>().write_volatile((1u32 << 11) << 16) };
    }
}

/// Read-only dump of the applied GPIO0 A/B pad config (boot #9 loopback debug).
/// Logs the whole PMU1_IOC first page for a direct line-by-line diff against
/// Linux's known-good reference, plus the decoded B3/MISO fields (mux 0x08[15:12],
/// drive 0x18[15:12], pull 0x24[7:6], smt 0x34[3]). Linux (works): B3 mux=1,
/// pull=3 (up), smt=0. Whichever field differs on our side is the loopback cause.
/// No writes.
fn dump_spi2_iomux() {
    let ioc = match iomap(RK3588_IOC_BASE, RK3588_IOC_SIZE) {
        Ok(v) => v.as_ptr(),
        Err(err) => {
            warn!("pmic_spi: iomap IOC {RK3588_IOC_BASE:#x} failed: {err:?}; skipping IOC dump");
            return;
        }
    };
    // SAFETY: the IOC mapping covers `RK3588_IOC_SIZE` bytes; every offset read
    // below is < 0x44, well within the first page. Read-only.
    let rd = |off: usize| unsafe { ioc.add(off).cast::<u32>().read_volatile() };
    let (mux_b, drv_b, pull_b, smt_b) = (rd(0x08), rd(0x18), rd(0x24), rd(0x34));
    info!(
        "pmic_spi: PMU1_IOC 00={:08x} 04={:08x} 08={:08x} 0c={:08x} 10={:08x} 14={:08x} 18={:08x} \
         1c={:08x} 20={:08x} 24={:08x} 28={:08x} 2c={:08x} 30={:08x} 34={:08x} 40={:08x}",
        rd(0x00),
        rd(0x04),
        rd(0x08),
        rd(0x0c),
        rd(0x10),
        rd(0x14),
        rd(0x18),
        rd(0x1c),
        rd(0x20),
        rd(0x24),
        rd(0x28),
        rd(0x2c),
        rd(0x30),
        rd(0x34),
        rd(0x40),
    );
    info!(
        "pmic_spi: B3/MISO decoded: mux={} (want 1) drive={} pull={} (want 3) smt={} (want 0)",
        (mux_b >> 12) & 0xf,
        (drv_b >> 12) & 0xf,
        (pull_b >> 6) & 0x3,
        (smt_b >> 3) & 0x1,
    );
}

/// Map and initialize the RK806-on-SPI2 controller. Idempotent; safe to call
/// from a `PostKernel` probe context (MMU up).
///
/// Ungates the SPI2 clocks (U-Boot gates them after talking to the RK806 at
/// boot), maps + configures the controller, then verifies it actually responds
/// on its APB. Returns `true` when the controller is bound and ready — including
/// an idempotent re-`init()` of an already-bound controller. Returns `false` on
/// a CRU/SPI2 MMIO mapping failure **or** if the controller does not respond
/// after ungate (still gated, or held in soft-reset — which would need a
/// separate reset de-assert). In every `false` case the controller is left
/// **unbound**, so `get_uv`/`set_uv*` are safe no-ops that leave the boot
/// voltage rather than returning a fabricated `0`.
pub fn init() -> bool {
    let mut guard = PMIC.lock();
    if guard.is_some() {
        // Already bound: the controller is ready, so report success (not a
        // failure) and keep the existing binding.
        return true;
    }
    if !ungate_spi2_clocks() {
        return false;
    }
    let virt = match iomap(RK3588_SPI2_BASE, RK3588_SPI2_SIZE) {
        Ok(v) => v,
        Err(err) => {
            warn!("pmic_spi: iomap {RK3588_SPI2_BASE:#x} failed: {err:?}");
            return false;
        }
    };
    // SAFETY: `iomap` returned a mapping of `RK3588_SPI2_SIZE` bytes at the SPI2
    // controller base; `base` is only ever used for volatile MMIO within that
    // window, serialized through the `PMIC` mutex.
    let dev = Rk806Spi {
        base: virt.as_ptr(),
    };
    // Release the controller core from soft-reset (boot #2: is_alive passed but
    // the shift engine read all-zero — core reset held). Must precede configure()
    // so BAUDR/CTRLR0 land on a released core.
    if !deassert_spi2_reset() {
        return false;
    }
    // Apply the FULL spi2 pinctrl-0 (mux + pull + drive + schmitt) via the
    // rockchip pinctrl driver — boot #7 showed a mux-only match isn't enough; the
    // pad config is what fixed i2c0. Must precede any transfer.
    set_spi2_pinmux();
    // Boot #8: read confirmed MISO loopback. Dump the applied SPI2 mux so we can
    // check B3(MISO) is really func1 (vs A6/MOSI which works).
    dump_spi2_iomux();
    dev.configure();
    if !dev.is_alive() {
        warn!(
            "pmic_spi: SPI2 controller not responding after ungate (CTRLR0 read-back failed; APB \
             may still be gated or held in reset); leaving A55 rail untouched"
        );
        return false;
    }
    // Read-only reachability probe (chip-ID + raw DCDC2 frame).
    dev.log_diagnostics();
    // Boot #8: the read now returns data but RX mirrors TX — distinguish MISO
    // loopback from a value-position offset (read-only).
    dev.probe_read_shape();
    info!("pmic_spi: RK806/SPI2 bound at {RK3588_SPI2_BASE:#x}");
    *guard = Some(dev);
    true
}

/// Read the current `vdd_cpu_lit` (A55 rail) voltage in microvolts, or `None`
/// if the controller is not initialized or the SPI read timed out.
pub fn get_uv() -> Option<u32> {
    let guard = PMIC.lock();
    let dev = guard.as_ref()?;
    let sel = dev.rk806_read(RK806_BUCK2_ON_VSEL)?;
    let uv = vsel_to_uv(sel);
    info!("pmic_spi: A55 vdd_cpu_lit = {uv} uV (buck2 vsel {sel:#04x})");
    Some(uv)
}

/// Set the A55 rail to `target_uv` in a single write, **down only**.
///
/// Refuses (returns `false`, no write) when: not initialized; the SPI read of
/// the boot voltage fails; `target_uv` is below [`A55_MIN_UV`]; or `target_uv`
/// is above the boot voltage (never raises). On an accepted target it encodes
/// the selector, writes buck-2 `ON_VSEL`, reads it back, and returns `true` only
/// when the read-back selector matches.
// Down-only single-write A55 setter; retained as part of the PMIC-SPI API surface
// for the calibration/voltage-lever path even when the shipped governor build does
// not call it.
#[allow(dead_code)]
pub fn set_uv(target_uv: u32) -> bool {
    let guard = PMIC.lock();
    let Some(dev) = guard.as_ref() else {
        warn!("pmic_spi: set_uv before init; ignored");
        return false;
    };
    let Some(boot_sel) = dev.rk806_read(RK806_BUCK2_ON_VSEL) else {
        warn!("pmic_spi: set_uv could not read boot voltage; leaving rail untouched");
        return false;
    };
    let boot_uv = vsel_to_uv(boot_sel);
    let Some(target_sel) = check_down_only(target_uv, boot_uv) else {
        return false;
    };
    write_and_verify(dev, target_sel)
}

/// Lower the A55 rail to `target_uv` in `<=`[`MAX_STEP_UV`] steps, verifying
/// each write and settling between steps, **down only**.
///
/// Same refusal rules as [`set_uv`]. A `target_uv` equal to the current voltage
/// is an accepted no-op (`true`). Any read-back mismatch aborts mid-descent and
/// returns `false`, leaving the last verified selector in place.
pub fn set_uv_stepped(target_uv: u32) -> bool {
    let guard = PMIC.lock();
    let Some(dev) = guard.as_ref() else {
        warn!("pmic_spi: set_uv_stepped before init; ignored");
        return false;
    };
    let Some(cur_sel) = dev.rk806_read(RK806_BUCK2_ON_VSEL) else {
        warn!("pmic_spi: set_uv_stepped could not read current voltage; leaving rail untouched");
        return false;
    };
    let cur_uv = vsel_to_uv(cur_sel);
    let Some(target_sel) = check_down_only(target_uv, cur_uv) else {
        return false;
    };

    // Higher selector == higher voltage. Down-only means target_sel <= cur_sel.
    let step_sel = (MAX_STEP_UV / RK806_VSEL_STEP_UV).max(1) as u8;
    let mut sel = cur_sel;
    while sel > target_sel {
        let next = target_sel.max(sel.saturating_sub(step_sel));
        if !write_and_verify(dev, next) {
            return false;
        }
        axklib::time::busy_wait(Duration::from_micros(STEP_SETTLE_US));
        sel = next;
    }
    info!(
        "pmic_spi: A55 vdd stepped to {} uV (vsel {sel:#04x})",
        vsel_to_uv(sel)
    );
    true
}

/// **Diagnostic** force-write of the A55 rail (DCDC2 `ON_VSEL`) — proves whether
/// SPI *writes* physically reach the RK806 while reads are dead. Not part of the
/// normal DVFS API; intended to be called once from a cpufreq test flag.
///
/// Safety: hard-clamped to `[675_000, 950_000]` uV — the A55 (little cluster) OPP
/// voltage range (675 mV @ 1008 MHz up to 950 mV @ 1800 MHz, all Linux-proven
/// freq/voltage pairs). The governor only ever passes OPP-matched voltages, so any
/// accepted value is a proven-safe rail voltage regardless of the (currently
/// unreadable) present value, and RK3588's voltage-coupled clock tracks the rail in
/// lockstep. Unlike [`set_uv`] it deliberately SKIPS the read-current/down-only
/// guard — the read path is a scope-wall (rx==tx loopback) and the write is what we
/// have. If the SPI path can't reach the RK806, the write is simply a no-op.
///
/// This is the A55 voltage-set primitive for the ondemand governor (the read-back
/// path is deferred pending the MISO scope fix). Attempts a read-back for the log
/// (`0x00` while reads fail) and returns the write transfer's success.
pub fn force_write_dcdc2(target_uv: u32) -> bool {
    if !(675_000..=950_000).contains(&target_uv) {
        warn!(
            "pmic_spi: force_write_dcdc2 refused target {target_uv} uV (outside A55 OPP range \
             [675000, 950000])"
        );
        return false;
    }
    let guard = PMIC.lock();
    let Some(dev) = guard.as_ref() else {
        warn!("pmic_spi: force_write_dcdc2 before init; ignored");
        return false;
    };
    let Some(sel) = uv_to_vsel(target_uv) else {
        warn!("pmic_spi: force_write_dcdc2 target {target_uv} uV not encodable");
        return false;
    };
    let wrote = dev.rk806_write(RK806_BUCK2_ON_VSEL, sel);
    let readback = dev.rk806_read(RK806_BUCK2_ON_VSEL);
    info!(
        "pmic_spi: force_write_dcdc2: wrote vsel={sel:#04x} ({target_uv} uV) xfer_ok={wrote} \
         readback={readback:#04x?}"
    );
    wrote
}

/// Shared down-only clamp: returns the encoded target selector, or `None`
/// (after logging why) when the target must be refused.
fn check_down_only(target_uv: u32, reference_uv: u32) -> Option<u8> {
    if target_uv < A55_MIN_UV {
        warn!("pmic_spi: refusing target {target_uv} uV below floor {A55_MIN_UV} uV");
        return None;
    }
    if target_uv > reference_uv {
        warn!("pmic_spi: refusing to RAISE vdd ({reference_uv} uV -> {target_uv} uV); down-only");
        return None;
    }
    match uv_to_vsel(target_uv) {
        Some(sel) => Some(sel),
        None => {
            warn!("pmic_spi: target {target_uv} uV not encodable in buck range 1");
            None
        }
    }
}

/// Write buck-2 `ON_VSEL` and confirm the read-back selector matches.
fn write_and_verify(dev: &Rk806Spi, sel: u8) -> bool {
    if !dev.rk806_write(RK806_BUCK2_ON_VSEL, sel) {
        warn!("pmic_spi: buck2 vsel write timed out (sel {sel:#04x})");
        return false;
    }
    match dev.rk806_read(RK806_BUCK2_ON_VSEL) {
        Some(rb) if rb == sel => true,
        Some(rb) => {
            warn!("pmic_spi: buck2 vsel read-back {rb:#04x} != written {sel:#04x}; aborting");
            false
        }
        None => {
            warn!("pmic_spi: buck2 vsel read-back failed after write; aborting");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vsel_decodes_known_opp_nominals() {
        assert_eq!(vsel_to_uv(0x1c), 675_000); // sel 28
        assert_eq!(vsel_to_uv(0x28), 750_000); // sel 40
        assert_eq!(vsel_to_uv(0x30), 800_000); // sel 48
        assert_eq!(vsel_to_uv(0), 500_000);
        // Selector 159 (range-1 max) is 1_493_750 uV; 1_500_000 is selector 160.
        assert_eq!(vsel_to_uv(RK806_VSEL_R1_MAX_SEL), 1_493_750);
    }

    #[test]
    fn vsel_decodes_upper_ranges() {
        assert_eq!(vsel_to_uv(160), 1_500_000); // range 2 base
        assert_eq!(vsel_to_uv(161), 1_525_000);
        assert_eq!(vsel_to_uv(235), 3_375_000);
        assert_eq!(vsel_to_uv(236), 3_400_000); // range 3 fixed
        assert_eq!(vsel_to_uv(255), 3_400_000);
    }

    #[test]
    fn uv_encodes_opp_nominals_exactly() {
        assert_eq!(uv_to_vsel(675_000), Some(0x1c));
        assert_eq!(uv_to_vsel(750_000), Some(0x28));
        assert_eq!(uv_to_vsel(800_000), Some(0x30));
    }

    #[test]
    fn uv_encode_roundtrips_within_range1() {
        for sel in 0..=RK806_VSEL_R1_MAX_SEL {
            let uv = vsel_to_uv(sel);
            assert_eq!(uv_to_vsel(uv), Some(sel), "sel {sel}");
        }
    }

    #[test]
    fn uv_encode_rejects_out_of_range1() {
        assert_eq!(uv_to_vsel(499_000), None);
        assert_eq!(uv_to_vsel(1_600_000), None);
    }

    #[test]
    fn uv_encode_rejects_unaligned_steps() {
        // In-range but not on a 6.25 mV boundary: reject rather than approximate.
        assert_eq!(uv_to_vsel(675_000 + 1), None);
        assert_eq!(uv_to_vsel(676_000), None); // 176_000 % 6250 != 0
        assert_eq!(uv_to_vsel(500_001), None);
        // Boundaries stay accepted.
        assert_eq!(uv_to_vsel(500_000), Some(0));
        assert_eq!(uv_to_vsel(506_250), Some(1));
    }

    #[test]
    fn down_only_clamp_refuses_below_floor_and_raises() {
        // Below the 675 mV floor.
        assert_eq!(check_down_only(650_000, 800_000), None);
        // Raise attempt (target above reference).
        assert_eq!(check_down_only(825_000, 800_000), None);
        // Legal down move.
        assert_eq!(check_down_only(675_000, 800_000), Some(0x1c));
        // No-op (equal) is allowed.
        assert_eq!(check_down_only(800_000, 800_000), Some(0x30));
    }

    #[test]
    fn baud_divider_even_and_under_target() {
        // 200 MHz functional clock, 1 MHz target -> even divisor, sclk <= 1 MHz.
        let div = baud_divider(200_000_000, 1_000_000);
        assert_eq!(div % 2, 0);
        assert!(200_000_000 / div <= 1_000_000, "div {div}");
        assert_eq!(div, 200);
    }

    #[test]
    fn baud_divider_clamps_and_stays_even() {
        assert_eq!(baud_divider(1_000_000, 1_000_000) % 2, 0);
        assert!(baud_divider(u32::MAX, 1) <= BAUDR_MAX);
        assert!(baud_divider(1, 1_000_000) >= BAUDR_MIN);
    }

    #[test]
    fn step_size_is_four_selectors() {
        assert_eq!(MAX_STEP_UV / RK806_VSEL_STEP_UV, 4); // 25 mV / 6.25 mV
    }

    #[test]
    fn spi2_ungate_value_matches_gate_bits() {
        // clkgate_con(14): PCLK_SPI2 = bit 8, CLK_SPI2 = bit 13. Write-masked:
        // high half selects the bits, low half writes 0 to enable them.
        assert_eq!(SPI2_CLK_UNGATE, 0x2100_0000);
        assert_eq!(SPI2_CLK_UNGATE & 0xffff, 0); // low half all-0 => enable
        assert_eq!(SPI2_CLK_UNGATE >> 16, (1 << 8) | (1 << 13));
        assert_eq!(CRU_CLKGATE_CON14, 14 * 4 + 0x800); // 0x838
    }

    #[test]
    fn ctrlr0_mode0_8bit_value() {
        // Linux spi-rockchip.c live value on this board: master, mode 0, 8-bit
        // DFS, one-cycle CS delay, EM_BIG, APB 8-bit access.
        assert_eq!(SPI_CTRLR0_MODE0_8BIT, 0x2c01);
    }

    #[test]
    fn spi2_reset_deassert_matches_reset_bits() {
        // softrst_con(14): SRST_P_SPI2 = bit 8, SRST_SPI2 = bit 13. Write-masked
        // de-assert: high half selects the bits, low half writes 0 to release.
        assert_eq!(SPI2_RST_DEASSERT, 0x2100_0000);
        assert_eq!(SPI2_RST_DEASSERT & 0xffff, 0); // low half all-0 => released
        assert_eq!(SPI2_RST_DEASSERT >> 16, (1 << 8) | (1 << 13));
        assert_eq!(CRU_SOFTRST_CON14, 14 * 4 + 0xa00); // 0xa38
    }
}
