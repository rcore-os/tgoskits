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

//! RK3588 A76 CPU-rail regulator lever (DVFS Phase 2) — RK8602/RK8603 over the
//! `rk3x_i2c` PMIC bus.
//!
//! This is the **voltage half** of the RK3588 CPU-DVFS fix. Phase 1a raises the
//! A76 cluster clocks via SCMI/BL31, but RK3588's CPU clock is voltage-coupled,
//! so the clock overshoots the request while the rail stays at its 800 mV boot
//! value. Lowering each A76 rail to its OPP-nominal 675 mV pulls the coupled
//! clock down to the exact requested rate. This module provides the read/write
//! API; `cpufreq.rs` owns when it is called.
//!
//! # Components
//!
//! * A minimal **polling** `rk3x_i2c` master for `i2c@fd880000` (RK3588 "bus0",
//!   the PMIC bus). No IRQs: it polls the raw IPD status. u-boot has already
//!   clocked and used this controller (it programs these same PMICs at boot),
//!   so [`Rk3xI2c::init_controller`] preserves u-boot's `CLKDIV` (a proven-good
//!   SCL rate) and only clears state. Register map + transaction flow modelled
//!   on mainline u-boot `drivers/i2c/rk_i2c.c` and Linux
//!   `drivers/i2c/busses/i2c-rk3x.c`.
//! * A **RK8602/RK8603** regulator API (`get_uv` / `set_uv` / `set_uv_stepped`)
//!   for the two A76 rails (RK8602 @ `0x42` = big0/cpu4-5, RK8603 @ `0x43` =
//!   big1/cpu6-7).
//!
//! # RK8602/RK8603 VSEL register (finding)
//!
//! Linux handles RK8602/RK8603 in `drivers/regulator/fan53555.c`
//! (`RK8602_VENDOR_ROCKCHIP`). It defines two voltage registers,
//! `RK8602_VSEL0 = 0x06` and `RK8602_VSEL1 = 0x07`, and selects the **active
//! runtime** register (`di->vol_reg`) from the `sleep_vsel_id` (DT
//! `fcs,suspend-voltage-selector`): `sleep_vsel_id == 1` makes VSEL1 (`0x07`)
//! the *sleep* register and **VSEL0 (`0x06`) the active** one. The Orange Pi
//! 5 Plus RK8602/8603 nodes use `fcs,suspend-voltage-selector = <1>`, so the
//! board's live VSEL is `0x06` — matching the board-confirmed boot value
//! (`0x06 == 0x30 == 800 mV`). We therefore write/read **reg `0x06`**.
//!
//! The voltage code is a full 8-bit field (`RK8602_NVOLTAGES = 160`,
//! `vsel_mask = 0xff`) — no enable/mode bit shares reg `0x06` (the buck-enable
//! `VSEL_BUCK_EN` bit lives in the legacy FAN53555 registers `0x00/0x01`, left
//! untouched — u-boot already enabled the rail). Encoding matches the board:
//! `di->vsel_min = 500000`, `di->vsel_step = 6250` → `V = 500000 + code·6250`
//! µV, i.e. `0x1c → 675 mV`, `0x30 → 800 mV`.

use core::time::Duration;

use ax_kspin::SpinNoIrq as Mutex;
use log::{info, warn};
use mmio_api::{MmioAddr, MmioRaw};
use rdif_pinctrl::PinctrlDevice;

use crate::mmio::iomap;

// ---------------------------------------------------------------------------
// rk3x_i2c register map (offsets from the controller base)
// ---------------------------------------------------------------------------

const REG_CON: usize = 0x00; // control
const REG_CLKDIV: usize = 0x04; // clock divider
const REG_MRXADDR: usize = 0x08; // master-rx slave address
const REG_MRXRADDR: usize = 0x0c; // master-rx slave register address
const REG_MTXCNT: usize = 0x10; // master-tx byte count
const REG_MRXCNT: usize = 0x14; // master-rx byte count
const REG_IEN: usize = 0x18; // interrupt enable
const REG_IPD: usize = 0x1c; // interrupt pending (raw status we poll)
const TXDATA_BASE: usize = 0x100; // tx FIFO word 0
const RXDATA_BASE: usize = 0x200; // rx FIFO word 0

// REG_CON bits
const CON_EN: u32 = 1 << 0;
const CON_START: u32 = 1 << 3;
const CON_STOP: u32 = 1 << 4;
const CON_LASTACK: u32 = 1 << 5; // NACK after the last received byte

// REG_CON transfer modes (shifted into bits [2:1] by `con_mod`)
const MODE_TX: u32 = 0; // write: address + data from tx FIFO
const MODE_TRX: u32 = 1; // combined: write reg address, then read

const fn con_mod(mode: u32) -> u32 {
    mode << 1
}

// REG_MRXADDR/REG_MRXRADDR: BIT(24 + n) marks address byte n valid. We only
// ever send one address byte and one register byte, so byte 0 valid = BIT(24).
const MRXADDR_VALID0: u32 = 1 << 24;

// REG_IPD/REG_IEN interrupt bits
const INT_MBTF: u32 = 1 << 2; // master byte-transmit finished
const INT_MBRF: u32 = 1 << 3; // master byte-receive finished
const INT_START: u32 = 1 << 4; // START generated
const INT_STOP: u32 = 1 << 5; // STOP generated
const INT_NAKRCV: u32 = 1 << 6; // NACK received
const INT_ALL: u32 = 0x7f; // all pending bits (write-1-to-clear)

/// RK3588 PMIC I2C controller ("bus0", `i2c@fd880000`).
const RK3588_I2C0_BASE: usize = 0xfd88_0000;
const RK3588_I2C0_SIZE: usize = 0x1000;

// --- i2c0 clock ungate (PMU CRU) --------------------------------------------
//
// i2c0 lives in the PMU CRU (`pmucru: clock-controller@fd7f0000` = main CRU
// 0xfd7c0000 + 0x30000). u-boot programmed the PMICs over i2c0 at boot but
// StarryOS leaves **CLK_I2C0 gated**: PCLK is still on (registers readable, e.g.
// CLKDIV reads back), yet the controller cannot generate SCL, so any START
// times out. `init` ungates it before the first transaction.
//
// Register/bit ground truth (cross-checked, identical in both):
//   * mainline `drivers/clk/rockchip/clk-rk3588.c`:
//       GATE(PCLK_I2C0, ..., RK3588_PMU_CLKGATE_CON(2), 1)
//       COMPOSITE_NODIV(CLK_I2C0, ..., RK3588_PMU_CLKGATE_CON(2), 2)
//   * `rockchip-soc` CRU crate gate.rs: `PCLK_I2C0 => (2, 1)`, `CLK_I2C0 => (2, 2)`.
//   * PMU_CLKGATE_CON(x) = 0x800 + x*4 within the PMU CRU → CON(2) = 0x808.
/// PMU CRU physical base (`clock-controller@fd7f0000`).
const RK3588_PMU_CRU_BASE: usize = 0xfd7f_0000;
const RK3588_PMU_CRU_SIZE: usize = 0x1000;
/// `PMU_CLKGATE_CON(2)` offset within the PMU CRU (holds the i2c0 gates).
const PMU_CLKGATE_CON2: usize = 0x800 + 2 * 4;
/// Gate bits to clear: PCLK_I2C0 (bit 1) + CLK_I2C0 (bit 2).
const I2C0_GATE_BITS: u32 = (1 << 1) | (1 << 2);

// i2c0 soft-reset is in the same PMU CRU. Ungating the clock alone was not
// enough on-board (registers readable, but START still timed out): the i2c0
// core is held in **soft-reset**. SRST_P_I2C0 (PCLK reset) = bit 1 and
// SRST_I2C0 (functional/core reset) = bit 2 of PMU_SOFTRST_CON(2), from mainline
// `drivers/clk/rockchip/rst-rk3588.c:776-777`
// (`RK3588_PMU1CRU_RESET_OFFSET(SRST_P_I2C0, 2, 1)` / `(SRST_I2C0, 2, 2)`) —
// same bit layout as the gate. The functional reset (bit 2) matches the
// symptom: PCLK regs read (SRST_P deasserted) but the SCL state machine is held.
// We only DE-ASSERT (write 0, masked), never assert, so u-boot's preserved
// CLKDIV in the PCLK domain is not cleared.
/// `PMU_SOFTRST_CON(2)` offset within the PMU CRU (0xa00 + 2*4).
const PMU_SOFTRST_CON2: usize = 0xa00 + 2 * 4;
/// Reset bits to release: SRST_P_I2C0 (bit 1) + SRST_I2C0 (bit 2).
const I2C0_RESET_BITS: u32 = (1 << 1) | (1 << 2);

/// Poll budget for a single IPD event, at 1 µs granularity — ~100 ms, matching
/// u-boot's `I2C_TIMEOUT_MS`. A PMIC register access completes in tens of µs;
/// this only bounds a wedged bus.
const I2C_POLL_MAX: u32 = 100_000;

// ---------------------------------------------------------------------------
// RK8602/RK8603 regulator constants
// ---------------------------------------------------------------------------

/// RK8602 @ I2C `0x42` — A76 big0 rail (`vdd_cpu_big0_s0`, cpu4-5).
pub const RK8602_BIG0_ADDR: u8 = 0x42;
/// RK8603 @ I2C `0x43` — A76 big1 rail (`vdd_cpu_big1_s0`, cpu6-7).
pub const RK8603_BIG1_ADDR: u8 = 0x43;

/// Active runtime VSEL register on this board (RK8602_VSEL0). See module docs.
const VSEL_REG: u8 = 0x06;
/// 8-bit voltage-code mask (`RK8602_NVOLTAGES = 160`, `vsel_mask = 0xff`).
const VSEL_MASK: u8 = 0xff;
/// Voltage encoding: `V = VSEL_BASE_UV + code · VSEL_STEP_UV` (µV).
const VSEL_BASE_UV: u32 = 500_000;
const VSEL_STEP_UV: u32 = 6_250;

/// Safety envelope for the A76 rails. Floor = the lowest OPP nominal (675 mV);
/// ceiling = the top OPP voltage (1.0 V @ 2256 MHz). `set_uv*` refuse anything
/// outside. The governor only ever passes OPP-matched voltages (each a
/// Linux-proven freq/voltage pair), so any accepted value is proven-safe; the
/// clamp is defense-in-depth against a bad caller. (Phase-1a only sets 675 mV.)
const VDD_FLOOR_UV: u32 = 675_000;
const VDD_CEIL_UV: u32 = 1_000_000;

/// Maximum voltage change per stepped-lowering step (≤ 4 LSB = 25 mV), so the
/// voltage-coupled clock tracks the rail down gradually.
const STEP_MAX_UV: u32 = 25_000;
/// Settle delay after each step, giving the rail time to slew before the next.
const SETTLE_US: u64 = 500;

// ---------------------------------------------------------------------------
// Voltage <-> VSEL code (pure, host-testable)
// ---------------------------------------------------------------------------

/// Decode an 8-bit VSEL code to microvolts.
fn vsel_to_uv(vsel: u8) -> u32 {
    VSEL_BASE_UV + (vsel as u32) * VSEL_STEP_UV
}

/// Encode microvolts to a VSEL code, requiring an **exact** round-trip. Returns
/// `None` for values below the base, above the 8-bit range, or that do not land
/// exactly on a 6.25 mV step (so we never program an approximated voltage).
fn uv_to_vsel(uv: u32) -> Option<u8> {
    if uv < VSEL_BASE_UV {
        return None;
    }
    let code = (uv - VSEL_BASE_UV) / VSEL_STEP_UV;
    if code > VSEL_MASK as u32 {
        return None;
    }
    let vsel = code as u8;
    (vsel_to_uv(vsel) == uv).then_some(vsel)
}

/// Whether `uv` is inside the Phase-2 down-only safety envelope.
fn in_envelope(uv: u32) -> bool {
    (VDD_FLOOR_UV..=VDD_CEIL_UV).contains(&uv)
}

// ---------------------------------------------------------------------------
// rk3x_i2c polling master
// ---------------------------------------------------------------------------

/// Outcome of waiting on an I2C completion event.
enum I2cErr {
    Nak,
    Timeout,
}

struct Rk3xI2c {
    mmio: MmioRaw,
}

impl Rk3xI2c {
    #[inline]
    fn r(&self, off: usize) -> u32 {
        self.mmio.read::<u32>(off)
    }

    #[inline]
    fn w(&self, off: usize, val: u32) {
        self.mmio.write::<u32>(off, val);
    }

    /// Prepare the controller for polled use. u-boot has already clocked and
    /// used this bus, so its `CLKDIV` is a proven-good SCL rate; preserve it and
    /// only fall back to a conservative ~100 kHz divider (assuming RK3588's
    /// 200 MHz i2c function clock) if it is somehow unset. Then park the
    /// controller disabled with interrupts off and pending status cleared.
    fn init_controller(&self) {
        if self.r(REG_CLKDIV) == 0 {
            // div = ceil(200 MHz / (100 kHz · 8)) - 2 = 248; split high/low.
            let (divl, divh) = (124u32, 124u32);
            self.w(REG_CLKDIV, (divh << 16) | divl);
            warn!("pmic_i2c: CLKDIV was 0; set conservative ~100 kHz (divl=divh=124 @200 MHz)");
        }
        self.w(REG_CON, 0);
        self.w(REG_IEN, 0);
        self.w(REG_IPD, INT_ALL);
        info!(
            "pmic_i2c: rk3x_i2c@{:#x} ready (clkdiv={:#x})",
            RK3588_I2C0_BASE,
            self.r(REG_CLKDIV)
        );
    }

    /// Poll IPD until any bit in `mask` is set. A received NACK aborts early.
    fn wait_ipd(&self, mask: u32) -> Result<(), I2cErr> {
        for _ in 0..I2C_POLL_MAX {
            let ipd = self.r(REG_IPD);
            if ipd & INT_NAKRCV != 0 {
                self.w(REG_IPD, INT_NAKRCV);
                return Err(I2cErr::Nak);
            }
            if ipd & mask != 0 {
                self.w(REG_IPD, mask);
                return Ok(());
            }
            axklib::time::busy_wait(Duration::from_micros(1));
        }
        Err(I2cErr::Timeout)
    }

    fn send_start(&self) -> Result<(), I2cErr> {
        self.w(REG_IPD, INT_ALL);
        self.w(REG_CON, CON_EN | CON_START);
        self.w(REG_IEN, INT_START);
        let res = self.wait_ipd(INT_START);
        if res.is_err() {
            // Board diagnostic: if START never completes, dump the controller
            // state. A live controller sets INT_START(bit4) in IPD; CON reads
            // back EN|START; CLKDIV should hold the (u-boot) divisor. All-zero
            // or stuck values point at a still-gated clock or held reset.
            warn!(
                "pmic_i2c: START did not complete: CON={:#010x} IPD={:#010x} CLKDIV={:#010x} \
                 IEN={:#010x}",
                self.r(REG_CON),
                self.r(REG_IPD),
                self.r(REG_CLKDIV),
                self.r(REG_IEN)
            );
        }
        res
    }

    fn send_stop(&self) -> Result<(), I2cErr> {
        self.w(REG_IPD, INT_ALL);
        self.w(REG_CON, CON_EN | CON_STOP);
        self.w(REG_IEN, INT_STOP);
        self.wait_ipd(INT_STOP)
    }

    #[inline]
    fn disable(&self) {
        self.w(REG_CON, 0);
    }

    /// SMBus-style single-byte register write: `[START][chip.W][reg][val][STOP]`.
    fn write_reg(&self, chip: u8, reg: u8, val: u8) -> bool {
        if self.send_start().is_err() {
            warn!("pmic_i2c: START failed writing chip {chip:#x} reg {reg:#x}");
            self.disable();
            return false;
        }
        // tx FIFO word 0: byte0 = write address, byte1 = reg, byte2 = data.
        let word0 = ((chip as u32) << 1) | ((reg as u32) << 8) | ((val as u32) << 16);
        self.w(TXDATA_BASE, word0);
        self.w(REG_CON, CON_EN | con_mod(MODE_TX));
        self.w(REG_MTXCNT, 3);
        self.w(REG_IEN, INT_MBTF | INT_NAKRCV);
        let ok = self.wait_ipd(INT_MBTF);
        // STOP before disable (disabling first would emit an illegal START/STOP).
        let _ = self.send_stop();
        self.disable();
        match ok {
            Ok(()) => true,
            Err(I2cErr::Nak) => {
                warn!("pmic_i2c: NACK writing chip {chip:#x} reg {reg:#x}");
                false
            }
            Err(I2cErr::Timeout) => {
                warn!("pmic_i2c: timeout writing chip {chip:#x} reg {reg:#x}");
                false
            }
        }
    }

    /// SMBus-style single-byte register read: combined write-address + read.
    fn read_reg(&self, chip: u8, reg: u8) -> Option<u8> {
        if self.send_start().is_err() {
            warn!("pmic_i2c: START failed reading chip {chip:#x} reg {reg:#x}");
            self.disable();
            return None;
        }
        // Read address (byte0 valid) and the register byte to send (byte0 valid).
        self.w(REG_MRXADDR, (((chip as u32) << 1) | 1) | MRXADDR_VALID0);
        self.w(REG_MRXRADDR, (reg as u32) | MRXADDR_VALID0);
        self.w(REG_CON, CON_EN | CON_LASTACK | con_mod(MODE_TRX));
        self.w(REG_MRXCNT, 1);
        self.w(REG_IEN, INT_MBRF | INT_NAKRCV);
        let res = self.wait_ipd(INT_MBRF);
        let val = match res {
            Ok(()) => Some((self.r(RXDATA_BASE) & 0xff) as u8),
            Err(I2cErr::Nak) => {
                warn!("pmic_i2c: NACK reading chip {chip:#x} reg {reg:#x}");
                None
            }
            Err(I2cErr::Timeout) => {
                warn!("pmic_i2c: timeout reading chip {chip:#x} reg {reg:#x}");
                None
            }
        };
        let _ = self.send_stop();
        self.disable();
        val
    }

    /// Write one VSEL code and confirm the read-back matches exactly.
    fn set_vsel_verify(&self, chip: u8, vsel: u8) -> bool {
        if !self.write_reg(chip, VSEL_REG, vsel) {
            return false;
        }
        match self.read_reg(chip, VSEL_REG) {
            Some(rb) if (rb & VSEL_MASK) == vsel => true,
            Some(rb) => {
                warn!(
                    "pmic_i2c: chip {chip:#x} VSEL read-back {:#x} != written {vsel:#x}",
                    rb & VSEL_MASK
                );
                false
            }
            None => {
                warn!("pmic_i2c: chip {chip:#x} VSEL read-back failed after write");
                false
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Public API — single global PMIC bus (mapped once)
// ---------------------------------------------------------------------------

static CONTROLLER: Mutex<Option<Rk3xI2c>> = Mutex::new(None);

/// Ungate the i2c0 PCLK + functional clock in the PMU CRU so the controller can
/// generate SCL. Idempotent — the Rockchip CRU gate registers are write-masked
/// (high 16 bits select which bits to write, low 16 are the values), and a `0`
/// in a gate bit means *enabled*, so re-clearing an already-enabled gate is a
/// no-op. Best-effort: on a mapping failure it logs and returns, leaving the
/// clock as-is (the subsequent transaction then times out into a safe no-op).
/// The controller is also released from soft-reset separately, see
/// [`deassert_i2c0_reset`].
fn ungate_i2c0_clocks() {
    let virt = match iomap(RK3588_PMU_CRU_BASE, RK3588_PMU_CRU_SIZE) {
        Ok(ptr) => ptr,
        Err(err) => {
            warn!(
                "pmic_i2c: iomap PMU CRU {RK3588_PMU_CRU_BASE:#x} failed: {err:?}; i2c0 clock \
                 left as-is"
            );
            return;
        }
    };
    // Clear both gate bits (write mask in the upper half, zeros in the lower).
    let val = I2C0_GATE_BITS << 16;
    // SAFETY: `virt` maps `RK3588_PMU_CRU_SIZE` bytes at the PMU CRU base;
    // `PMU_CLKGATE_CON2` (0x808) is within that window.
    let gate = unsafe { virt.as_ptr().add(PMU_CLKGATE_CON2).cast::<u32>() };
    unsafe { gate.write_volatile(val) };
    // Let the newly-ungated functional clock start before the first transfer.
    axklib::time::busy_wait(Duration::from_micros(5));
    // Read back the (unmasked) gate bits for board diagnostics: 0 == enabled.
    let after = unsafe { gate.read_volatile() } & I2C0_GATE_BITS;
    info!(
        "pmic_i2c: ungated i2c0 clocks (PMU_CLKGATE_CON(2)@{:#x} <- {val:#010x}); gate bits now \
         {after:#x} (0 == enabled)",
        RK3588_PMU_CRU_BASE + PMU_CLKGATE_CON2
    );
}

/// Release the i2c0 controller from soft-reset in the PMU CRU so its SCL state
/// machine can run. De-assert only (write masked zeros to SRST_P_I2C0 bit 1 +
/// SRST_I2C0 bit 2); we never assert, so the PCLK-domain CLKDIV that u-boot left
/// is preserved. Idempotent (re-clearing an already-released reset is a no-op).
/// Best-effort: a mapping failure is logged and the later transaction just times
/// out into a safe no-op. Reads the bits back for board diagnostics.
fn deassert_i2c0_reset() {
    let virt = match iomap(RK3588_PMU_CRU_BASE, RK3588_PMU_CRU_SIZE) {
        Ok(ptr) => ptr,
        Err(err) => {
            warn!(
                "pmic_i2c: iomap PMU CRU {RK3588_PMU_CRU_BASE:#x} failed: {err:?}; i2c0 reset \
                 left as-is"
            );
            return;
        }
    };
    // Clear both reset bits (write mask in the upper half, zeros in the lower).
    let val = I2C0_RESET_BITS << 16;
    // SAFETY: `virt` maps `RK3588_PMU_CRU_SIZE` bytes at the PMU CRU base;
    // `PMU_SOFTRST_CON2` (0xa08) is within that window.
    let softrst = unsafe { virt.as_ptr().add(PMU_SOFTRST_CON2).cast::<u32>() };
    unsafe { softrst.write_volatile(val) };
    // Give the core a moment to come out of reset before the first transfer.
    axklib::time::busy_wait(Duration::from_micros(10));
    // Read back the (unmasked) reset bits for board diagnostics: 0 == released.
    let after = unsafe { softrst.read_volatile() } & I2C0_RESET_BITS;
    info!(
        "pmic_i2c: de-asserted i2c0 reset (PMU_SOFTRST_CON(2)@{:#x} <- {val:#010x}); reset bits \
         now {after:#x} (0 == released)",
        RK3588_PMU_CRU_BASE + PMU_SOFTRST_CON2
    );
}

/// Mux the i2c0 SCL/SDA pads to the i2c0 function via the registered Rockchip
/// pinctrl driver. Without this the rk3x master waits forever for a free bus and
/// never generates START (observed on-board: `IPD=0` after ungate + reset):
/// u-boot muxed these pads to i2c0 to program the PMICs, but StarryOS reverts
/// them at boot. The pins are i2c0m2 — `GPIO0_D1` (SCL) / `GPIO0_D2` (SDA) at
/// function 3 — which sit in the GPIO0_D "high group" split PMU2_IOC/BUS_IOC
/// mux path; rather than hand-decode that, we apply the i2c0 node's `pinctrl-0`
/// state through the in-tree pinctrl driver, which already encodes it. This is a
/// mux change only (no PMIC access). Best-effort: any failure is logged and the
/// later transaction times out into a safe no-op.
fn set_i2c0_pinmux() {
    let Some(pinctrl) = rdrive::get_one::<PinctrlDevice>() else {
        warn!("pmic_i2c: PinctrlDevice not registered; cannot mux i2c0 pins (START will fail)");
        return;
    };
    let mut pinctrl = match pinctrl.lock() {
        Ok(guard) => guard,
        Err(err) => {
            warn!("pmic_i2c: failed to lock PinctrlDevice: {err}; cannot mux i2c0 pins");
            return;
        }
    };
    let Some(fdt) = rdrive::with_fdt(Clone::clone) else {
        warn!("pmic_i2c: live FDT not found; cannot mux i2c0 pins");
        return;
    };
    // The i2c0 controller node (reg base == RK3588_I2C0_BASE) carries
    // `pinctrl-0 = <&i2c0m2_xfer>`; several i2c controllers share the
    // rk3399-i2c compatible, so match on the reg address.
    let node = fdt
        .find_compatible(&["rockchip,rk3588-i2c", "rockchip,rk3399-i2c"])
        .into_iter()
        .find(|n| {
            n.regs()
                .into_iter()
                .next()
                .is_some_and(|r| r.address as usize == RK3588_I2C0_BASE)
        });
    let Some(node) = node else {
        warn!("pmic_i2c: i2c0 node @ {RK3588_I2C0_BASE:#x} not found in FDT; cannot mux pins");
        return;
    };
    match pinctrl.apply_fdt_default_state(&fdt, node.as_node()) {
        Ok(()) => info!(
            "pmic_i2c: applied i2c0 pinctrl-0 (scl GPIO0_D1 / sda GPIO0_D2 -> func3) via rockchip \
             pinctrl"
        ),
        Err(err) => warn!("pmic_i2c: failed to apply i2c0 pinctrl-0: {err:?}"),
    }
}

/// Map and initialise the RK3588 PMIC I2C controller. Idempotent; safe to call
/// from a `PostKernel` probe context (MMU up). Returns `false` if the MMIO
/// mapping fails, in which case every `get_uv`/`set_uv*` below is a no-op that
/// returns `None`/`false` and leaves the rails at their (safe) boot voltage.
pub fn init() -> bool {
    let mut guard = CONTROLLER.lock();
    if guard.is_some() {
        return true;
    }
    // Bring the i2c0 bus up before any transaction: ungate its clocks, release
    // it from soft-reset, and mux its SCL/SDA pads to the i2c0 function (all
    // three left closed/reverted at boot; any one makes every START time out).
    // All best-effort — a failure just leaves the later transaction to time out
    // into a safe no-op.
    ungate_i2c0_clocks();
    deassert_i2c0_reset();
    set_i2c0_pinmux();
    let virt = match iomap(RK3588_I2C0_BASE, RK3588_I2C0_SIZE) {
        Ok(ptr) => ptr,
        Err(err) => {
            warn!("pmic_i2c: iomap {RK3588_I2C0_BASE:#x} failed: {err:?}");
            return false;
        }
    };
    // SAFETY: `virt` is a fresh device mapping of `RK3588_I2C0_BASE` of
    // `RK3588_I2C0_SIZE` bytes returned by `iomap`.
    let mmio = unsafe {
        MmioRaw::new(
            MmioAddr::from(RK3588_I2C0_BASE as u64),
            virt,
            RK3588_I2C0_SIZE,
        )
    };
    let i2c = Rk3xI2c { mmio };
    i2c.init_controller();
    *guard = Some(i2c);
    true
}

/// Read a rail's current voltage in microvolts. `chip` is [`RK8602_BIG0_ADDR`]
/// or [`RK8603_BIG1_ADDR`]. `None` if the bus is uninitialised or the read
/// fails.
pub fn get_uv(chip: u8) -> Option<u32> {
    let guard = CONTROLLER.lock();
    let i2c = guard.as_ref()?;
    let vsel = i2c.read_reg(chip, VSEL_REG)?;
    Some(vsel_to_uv(vsel & VSEL_MASK))
}

/// Set a rail directly to `target_uv` (single write), refusing anything outside
/// the Phase-2 envelope `[675_000, 800_000]` µV or that is not an exact 6.25 mV
/// step, and verifying the read-back. Returns `false` on any rejection or
/// mismatch (leaving the rail unchanged/at its last confirmed value).
///
/// This is the direct setter. Live-core lowering should use
/// [`set_uv_stepped`], which ramps down in small increments.
pub fn set_uv(chip: u8, target_uv: u32) -> bool {
    if !in_envelope(target_uv) {
        warn!(
            "pmic_i2c: refusing chip {chip:#x} set to {target_uv} uV (outside [{VDD_FLOOR_UV}, \
             {VDD_CEIL_UV}])"
        );
        return false;
    }
    let Some(vsel) = uv_to_vsel(target_uv) else {
        warn!("pmic_i2c: {target_uv} uV is not an exact VSEL step; refusing");
        return false;
    };
    let guard = CONTROLLER.lock();
    let Some(i2c) = guard.as_ref() else {
        warn!("pmic_i2c: not initialised; refusing set");
        return false;
    };
    i2c.set_vsel_verify(chip, vsel)
}

/// Safely lower a rail to `target_uv` by stepping **down** in ≤25 mV (4-LSB)
/// increments, verifying the read-back and settling after each step, so the
/// voltage-coupled CPU clock tracks the rail down without a large undervolt
/// transient. This is the path `cpufreq` calls on live cores.
///
/// Refuses (returns `false`) if `target_uv` is outside the envelope, is not an
/// exact step, or is **above** the rail's current voltage (this path never
/// raises voltage). A no-op success if already at target. Aborts and returns
/// `false` on the first read-back mismatch, leaving the rail at the last
/// verified step.
pub fn set_uv_stepped(chip: u8, target_uv: u32) -> bool {
    if !in_envelope(target_uv) {
        warn!(
            "pmic_i2c: refusing chip {chip:#x} step to {target_uv} uV (outside [{VDD_FLOOR_UV}, \
             {VDD_CEIL_UV}])"
        );
        return false;
    }
    let Some(target_vsel) = uv_to_vsel(target_uv) else {
        warn!("pmic_i2c: {target_uv} uV is not an exact VSEL step; refusing");
        return false;
    };

    let guard = CONTROLLER.lock();
    let Some(i2c) = guard.as_ref() else {
        warn!("pmic_i2c: not initialised; refusing step");
        return false;
    };

    let Some(cur_vsel) = i2c.read_reg(chip, VSEL_REG).map(|v| v & VSEL_MASK) else {
        warn!("pmic_i2c: chip {chip:#x} current VSEL read failed; refusing step");
        return false;
    };

    if target_vsel > cur_vsel {
        warn!(
            "pmic_i2c: refusing to step UP chip {chip:#x} {} -> {target_uv} uV (down-only path)",
            vsel_to_uv(cur_vsel)
        );
        return false;
    }
    if target_vsel == cur_vsel {
        return true;
    }

    const STEP_LSB: u8 = (STEP_MAX_UV / VSEL_STEP_UV) as u8; // 25000 / 6250 = 4
    let mut v = cur_vsel;
    while v > target_vsel {
        let next = v.saturating_sub(STEP_LSB).max(target_vsel);
        if !i2c.set_vsel_verify(chip, next) {
            warn!(
                "pmic_i2c: chip {chip:#x} step to VSEL {next:#x} ({} uV) failed; aborting at {} uV",
                vsel_to_uv(next),
                vsel_to_uv(v)
            );
            return false;
        }
        axklib::time::busy_wait(Duration::from_micros(SETTLE_US));
        v = next;
    }
    info!(
        "pmic_i2c: chip {chip:#x} stepped {} -> {} uV",
        vsel_to_uv(cur_vsel),
        vsel_to_uv(target_vsel)
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    // Board-confirmed VSEL points.
    #[test]
    fn vsel_decode_matches_board_points() {
        assert_eq!(vsel_to_uv(0x1c), 675_000);
        assert_eq!(vsel_to_uv(0x20), 700_000);
        assert_eq!(vsel_to_uv(0x24), 725_000);
        assert_eq!(vsel_to_uv(0x30), 800_000);
        assert_eq!(vsel_to_uv(0x50), 1_000_000);
    }

    #[test]
    fn uv_encode_roundtrips_board_points() {
        assert_eq!(uv_to_vsel(675_000), Some(0x1c));
        assert_eq!(uv_to_vsel(700_000), Some(0x20));
        assert_eq!(uv_to_vsel(725_000), Some(0x24));
        assert_eq!(uv_to_vsel(800_000), Some(0x30));
        assert_eq!(uv_to_vsel(1_000_000), Some(0x50));
    }

    #[test]
    fn uv_encode_rejects_non_step_and_out_of_range() {
        // Not a multiple of 6.25 mV.
        assert_eq!(uv_to_vsel(676_000), None);
        assert_eq!(uv_to_vsel(675_001), None);
        // Below the encoding base.
        assert_eq!(uv_to_vsel(499_999), None);
        // Above the 8-bit code range (code 255 -> 2.09375 V).
        assert_eq!(uv_to_vsel(2_100_000), None);
    }

    #[test]
    fn envelope_is_down_only_675_to_800() {
        assert!(in_envelope(675_000));
        assert!(in_envelope(800_000));
        assert!(in_envelope(725_000));
        assert!(!in_envelope(674_999)); // below OPP floor
        assert!(!in_envelope(800_001)); // above boot voltage
        assert!(!in_envelope(1_000_000));
    }

    #[test]
    fn step_size_is_four_lsb_25mv() {
        const STEP_LSB: u8 = (STEP_MAX_UV / VSEL_STEP_UV) as u8;
        assert_eq!(STEP_LSB, 4);
        assert_eq!((STEP_LSB as u32) * VSEL_STEP_UV, 25_000);
        // 800 -> 675 mV is 20 LSB = exactly 5 whole 4-LSB steps.
        assert_eq!(
            uv_to_vsel(800_000).unwrap() - uv_to_vsel(675_000).unwrap(),
            20
        );
    }
}
