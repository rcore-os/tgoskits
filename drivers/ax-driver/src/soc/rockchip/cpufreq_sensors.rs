// Copyright 2026 The Axvisor Team
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

//! RK3588 OTP bin and firmware-configured TSADC CPU sensor access.
//!
//! OTP is read once during device probing. The TSADC path only reads an already
//! running converter with an armed hardware shutdown channel. In particular it
//! never resets the converter or changes its shutdown registers.

use alloc::{format, vec::Vec};
use core::{
    ptr::{NonNull, null_mut},
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering},
    time::Duration,
};

use log::info;
use rdrive::{probe::OnProbeError, register::FdtInfo};

use crate::{mmio::iomap, register::ProbeFdt};

const OTP_SIZE: usize = 0x400;
const TSADC_SIZE: usize = 0x400;
const CRU_GATE18: usize = 0x800 + 18 * 4;
// The board DTB omits CLK_OTPC_ARB and CLK_OTPC_AUTO_RD_G. Linux's RK3588
// clock data places these alongside the three clocks listed by the DTB.
const OTP_GATE_BITS: u32 = (1 << 9) | (1 << 10) | (1 << 11) | (1 << 12) | (1 << 13);

const OTP_AUTO_CTRL: usize = 0x04;
const OTP_AUTO_EN: usize = 0x08;
const OTP_DOUT0: usize = 0x20;
const OTP_INT_ST: usize = 0x84;
const OTP_RD_DONE: u32 = 1 << 1;
const OTP_NONSECURE_BASE: u32 = 0x300;
const OTP_BURST_ONE: u32 = 1 << 8;
const OTP_SPEC_OFFSET: usize = 6;
const OTP_CACHE_SIZE: usize = 0x80;
const OTP_POLL_LIMIT_US: usize = 10_000;

const TSADC_AUTO_CON: usize = 0x04;
const TSADC_AUTO_SRC_CON: usize = 0x0c;
const TSADC_HSHUT_CRU_EN: usize = 0x1c;
const TSADC_DATA0: usize = 0x2c;
const TSADC_COMP_SHUT0: usize = 0x10c;
const TSADC_INT_DEBOUNCE: usize = 0x14c;
const TSADC_SHUT_DEBOUNCE: usize = 0x150;
const TSADC_AUTO_PERIOD: usize = 0x154;
const TSADC_AUTO_PERIOD_HT: usize = 0x158;
const TSADC_CODE_MASK: u32 = 0x1ff;
const TSADC_RATE_HZ: u64 = 2_000_000;
// Linear interpolation of the BSP RK3588 table maps 120 C to ADC code 389.
const TSADC_SHUT_CODE: u32 = 389;
const TSADC_CHANNELS: usize = 7;

static OTP_WORDS: [AtomicU32; OTP_CACHE_SIZE / 4] =
    [const { AtomicU32::new(0) }; OTP_CACHE_SIZE / 4];
static OTP_READY: AtomicBool = AtomicBool::new(false);
static TSADC_MMIO: AtomicPtr<u8> = AtomicPtr::new(null_mut());

/// A sensor failure keeps the associated high-frequency OPPs unavailable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SensorError {
    NotReady,
    InvalidReading,
    Unprotected,
    Timeout,
}

crate::model_register!(
    name: "RK3588 CPU DVFS silicon sensors",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["rockchip,rk3588-otp"],
            on_probe: probe_otp
        },
        ProbeKind::Fdt {
            compatibles: &["rockchip,rk3588-tsadc"],
            on_probe: probe_tsadc
        }
    ],
);

/// Return a bounded slice of the OTP nonsecure area cached at probe time.
///
/// The cache covers all CPU OPP nvmem cells in the Orange Pi board DTB. A
/// failed, unstable, or incomplete OTP read publishes no bytes.
pub fn read_otp_bytes(offset: usize, len: usize) -> Result<Vec<u8>, SensorError> {
    let end = offset.checked_add(len).ok_or(SensorError::InvalidReading)?;
    if end > OTP_CACHE_SIZE {
        return Err(SensorError::InvalidReading);
    }
    if !OTP_READY.load(Ordering::Acquire) {
        return Err(SensorError::NotReady);
    }
    let mut bytes = Vec::with_capacity(len);
    for index in offset..end {
        let word = OTP_WORDS[index / 4].load(Ordering::Relaxed);
        bytes.push(word.to_le_bytes()[index % 4]);
    }
    Ok(bytes)
}

/// Return the raw OTP specification byte (bit[4:0] selects the CPU SKU).
pub fn otp_specification_byte() -> Result<u8, SensorError> {
    Ok(read_otp_bytes(OTP_SPEC_OFFSET, 1)?[0])
}

/// Return the CPU SKU serial from the five low bits of the OTP cell.
pub fn sku_serial() -> Result<u8, SensorError> {
    Ok(otp_specification_byte()? & 0x1f)
}

/// Read the CPU TSADC channel for A55, big0, or big1, in millidegrees Celsius.
///
/// `domain` follows the cpufreq domain order 0=A55, 1=big0, 2=big1. A result
/// is returned only while the converter, that channel, and a verified <=120 C
/// CRU hardware shutdown route remain enabled. A missing or implausible
/// reading requires the caller to cap the OPP.
pub fn cpu_temperature_millidegrees(domain: usize) -> Result<i32, SensorError> {
    let channel = match domain {
        0 => 3,
        1 => 1,
        2 => 2,
        _ => return Err(SensorError::InvalidReading),
    };
    // probe_tsadc publishes this permanent mapping after all clock and
    // shutdown checks. Acquire also makes the completed probe visible here.
    let mmio = NonNull::new(TSADC_MMIO.load(Ordering::Acquire)).ok_or(SensorError::NotReady)?;
    let auto_con = read32(mmio, TSADC_AUTO_CON);
    if auto_con & 1 == 0 || read32(mmio, TSADC_AUTO_SRC_CON) & (1 << channel) == 0 {
        return Err(SensorError::NotReady);
    }
    if auto_con & ((1 << 1) | (1 << 8)) != 0 {
        return Err(SensorError::Unprotected);
    }
    let shut_mask = 1 << channel;
    let shut_code = read32(mmio, TSADC_COMP_SHUT0 + 4 * channel);
    if read32(mmio, TSADC_HSHUT_CRU_EN) & shut_mask == 0
        || !(215..=TSADC_SHUT_CODE).contains(&shut_code)
    {
        return Err(SensorError::Unprotected);
    }
    code_to_millidegrees(read32(mmio, TSADC_DATA0 + 4 * channel)).ok_or(SensorError::InvalidReading)
}

fn probe_otp(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, _) = probe.into_parts();
    let (otp_address, _) = checked_reg(&info, OTP_SIZE)?;
    let cru = rdrive::fdt_ref()
        .ok_or_else(|| OnProbeError::other("RK3588 FDT is unavailable"))?
        .find_compatible(&["rockchip,rk3588-cru"])
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other("RK3588 OTP has no CRU node"))?;
    let cru_phandle = cru
        .as_node()
        .phandle()
        .ok_or_else(|| OnProbeError::other("RK3588 CRU has no phandle"))?;
    let clocks = info.clocks()?;
    let selectors = clocks
        .iter()
        .map(|clock| clock.select())
        .collect::<Vec<_>>();
    if clocks.iter().any(|clock| clock.phandle != cru_phandle)
        || (selectors.as_slice() != [Some(150), Some(149), Some(153)]
            && selectors.as_slice() != [Some(150), Some(149), Some(151), Some(153)])
    {
        return Err(OnProbeError::other(
            "unexpected RK3588 OTP clock references",
        ));
    }
    let cru_reg = cru
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other("RK3588 CRU has no register range"))?;
    let cru_address = usize::try_from(cru_reg.address)
        .map_err(|_| OnProbeError::other("RK3588 CRU address overflow"))?;
    let cru_size = usize::try_from(cru_reg.size.unwrap_or(0))
        .map_err(|_| OnProbeError::other("RK3588 CRU size overflow"))?;
    if cru_size < CRU_GATE18 + 4 {
        return Err(OnProbeError::other(
            "RK3588 CRU gate register is outside range",
        ));
    }
    let cru_mmio = iomap(cru_address, CRU_GATE18 + 4)?;
    enable_otp_clocks(cru_mmio)?;

    let otp_mmio = iomap(otp_address, OTP_SIZE)?;
    for (word_offset, word) in OTP_WORDS.iter().enumerate() {
        let first = read_otp_word(otp_mmio, word_offset).map_err(|error| {
            OnProbeError::other(format!(
                "RK3588 OTP word {word_offset} read failed: {error:?}"
            ))
        })?;
        let second = read_otp_word(otp_mmio, word_offset).map_err(|error| {
            OnProbeError::other(format!(
                "RK3588 OTP word {word_offset} reread failed: {error:?}"
            ))
        })?;
        if first != second {
            return Err(OnProbeError::other(format!(
                "RK3588 OTP word {word_offset} was unstable"
            )));
        }
        if word_offset == OTP_SPEC_OFFSET / 4 && (first == 0 || first == u32::MAX) {
            return Err(OnProbeError::other(
                "RK3588 OTP specification word is invalid",
            ));
        }
        word.store(first, Ordering::Relaxed);
    }
    OTP_READY.store(true, Ordering::Release);
    let specification = otp_specification_byte().map_err(|_| OnProbeError::other("OTP cache"))?;
    info!("RK3588 OTP specification byte: {specification:#04x}");
    Ok(())
}

fn probe_tsadc(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, _) = probe.into_parts();
    let (address, _) = checked_reg(&info, TSADC_SIZE)?;
    let node = info.node.as_node();
    let tshut_temp = node
        .get_property("rockchip,hw-tshut-temp")
        .and_then(|property| property.get_u32());
    let tshut_mode = node
        .get_property("rockchip,hw-tshut-mode")
        .and_then(|property| property.get_u32());
    let tshut_polarity = node
        .get_property("rockchip,hw-tshut-polarity")
        .and_then(|property| property.get_u32());
    if (tshut_temp, tshut_mode, tshut_polarity) != (Some(120_000), Some(0), Some(0)) {
        return Err(OnProbeError::other(
            "RK3588 TSADC hardware shutdown does not match board policy",
        ));
    }
    let clock = info
        .find_clock_line_by_name("tsadc")?
        .ok_or_else(|| OnProbeError::other("RK3588 TSADC clock is absent"))?;
    let pclk = info
        .find_clock_line_by_name("apb_pclk")?
        .ok_or_else(|| OnProbeError::other("RK3588 TSADC APB clock is absent"))?;
    let assigned = info
        .node
        .as_node()
        .get_property("assigned-clock-rates")
        .and_then(|property| property.get_u32());
    if assigned != Some(TSADC_RATE_HZ as u32) || clock.rate()? != TSADC_RATE_HZ {
        return Err(OnProbeError::other(
            "RK3588 TSADC is not clocked at its 2 MHz DT rate",
        ));
    }
    pclk.enable()?;
    clock.enable()?;
    let resets = info.reset_lines()?;
    if resets.len() != 2 {
        return Err(OnProbeError::other(
            "RK3588 TSADC reset references are incomplete",
        ));
    }
    for reset in resets {
        // Releasing an asserted reset is idempotent; never pulse or assert it
        // against a running thermal protection controller.
        reset.deassert()?;
    }
    let mmio = iomap(address, TSADC_SIZE)?;
    prepare_tsadc(mmio)?;
    TSADC_MMIO.store(mmio.as_ptr(), Ordering::Release);
    info!("RK3588 TSADC sampling with verified <=120 C hardware shutdown on all channels");
    Ok(())
}

fn prepare_tsadc(mmio: NonNull<u8>) -> Result<(), OnProbeError> {
    let running = read32(mmio, TSADC_AUTO_CON) & 1 != 0;
    if read32(mmio, TSADC_AUTO_CON) & (1 << 1) != 0 {
        if running {
            return Err(OnProbeError::other(
                "active RK3588 TSADC has unexpected ADC code inversion",
            ));
        }
        write32(mmio, TSADC_AUTO_CON, 1 << 17);
        if read32(mmio, TSADC_AUTO_CON) & (1 << 1) != 0 {
            return Err(OnProbeError::other(
                "RK3588 TSADC ADC code direction did not verify",
            ));
        }
    }
    if read32(mmio, TSADC_AUTO_CON) & (1 << 8) != 0 {
        // The board DTB requests low-active shutdown. Changing an active
        // controller's polarity could temporarily disarm protection.
        if running {
            return Err(OnProbeError::other(
                "active RK3588 TSADC has unexpected TSHUT polarity",
            ));
        }
        write32(mmio, TSADC_AUTO_CON, 1 << 24);
        if read32(mmio, TSADC_AUTO_CON) & (1 << 8) != 0 {
            return Err(OnProbeError::other(
                "RK3588 TSADC TSHUT polarity did not verify",
            ));
        }
    }

    if !running {
        // BSP rk_tsadcv8_initialize: 5000 cycles at 2 MHz is 2.5 ms.
        for (offset, value) in [
            (TSADC_AUTO_PERIOD, 5000),
            (TSADC_AUTO_PERIOD_HT, 5000),
            (TSADC_INT_DEBOUNCE, 4),
            (TSADC_SHUT_DEBOUNCE, 4),
        ] {
            write32(mmio, offset, value);
            if read32(mmio, offset) != value {
                return Err(OnProbeError::other(format!(
                    "RK3588 TSADC timing register {offset:#x} did not verify"
                )));
            }
        }
    }

    for channel in 0..TSADC_CHANNELS {
        let mask = 1u32 << channel;
        // Set the shutdown comparator before enabling the auto source and
        // CRU route. All three readbacks precede AUTO_EN on a stopped block.
        let threshold_offset = TSADC_COMP_SHUT0 + channel * 4;
        let previous = read32(mmio, threshold_offset);
        if running && previous < 215 {
            return Err(OnProbeError::other(format!(
                "active RK3588 TSADC channel {channel} has an unknown low shutdown code"
            )));
        }
        let shutdown_code = if running && (215..=TSADC_SHUT_CODE).contains(&previous) {
            previous
        } else {
            TSADC_SHUT_CODE
        };
        if previous != shutdown_code {
            write32(mmio, threshold_offset, shutdown_code);
        }
        if read32(mmio, threshold_offset) != shutdown_code {
            return Err(OnProbeError::other(format!(
                "RK3588 TSADC channel {channel} shutdown threshold did not verify"
            )));
        }
        write32(mmio, TSADC_HSHUT_CRU_EN, mask | (mask << 16));
        if read32(mmio, TSADC_HSHUT_CRU_EN) & mask == 0 {
            return Err(OnProbeError::other(format!(
                "RK3588 TSADC channel {channel} shutdown route did not verify"
            )));
        }
        write32(mmio, TSADC_AUTO_SRC_CON, mask | (mask << 16));
        if read32(mmio, TSADC_AUTO_SRC_CON) & mask == 0 {
            return Err(OnProbeError::other(format!(
                "RK3588 TSADC channel {channel} auto source did not verify"
            )));
        }
    }

    if !running {
        write32(mmio, TSADC_AUTO_CON, (1 << 16) | 1);
        if read32(mmio, TSADC_AUTO_CON) & 1 == 0 {
            return Err(OnProbeError::other("RK3588 TSADC AUTO_EN did not verify"));
        }
    }
    for _ in 0..50 {
        if [1, 2, 3]
            .into_iter()
            .all(|channel| code_to_millidegrees(read32(mmio, TSADC_DATA0 + channel * 4)).is_some())
        {
            return Ok(());
        }
        axklib::time::busy_wait(Duration::from_millis(1));
    }
    Err(OnProbeError::other(
        "RK3588 TSADC CPU channels did not produce valid samples",
    ))
}

fn checked_reg(info: &FdtInfo<'_>, minimum: usize) -> Result<(usize, usize), OnProbeError> {
    let reg = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other(format!("{} has no registers", info.node.name())))?;
    let address = usize::try_from(reg.address)
        .map_err(|_| OnProbeError::other(format!("{} address overflow", info.node.name())))?;
    let size = usize::try_from(reg.size.unwrap_or(0))
        .map_err(|_| OnProbeError::other(format!("{} size overflow", info.node.name())))?;
    if address % 4 != 0 || size < minimum || address.checked_add(size).is_none() {
        return Err(OnProbeError::other(format!(
            "{} invalid register range",
            info.node.name()
        )));
    }
    Ok((address, size))
}

fn enable_otp_clocks(cru: NonNull<u8>) -> Result<(), OnProbeError> {
    // RK3588 CRU gates are write-masked: the upper 16 bits select which gate
    // bits to clear. This preserves all other CRU clients sharing gate 18.
    write32(cru, CRU_GATE18, OTP_GATE_BITS << 16);
    axklib::time::busy_wait(Duration::from_micros(5));
    if read32(cru, CRU_GATE18) & OTP_GATE_BITS != 0 {
        return Err(OnProbeError::other("RK3588 OTP clocks did not ungate"));
    }
    Ok(())
}

fn read_otp_word(otp: NonNull<u8>, word_offset: usize) -> Result<u32, SensorError> {
    let address = OTP_NONSECURE_BASE + word_offset as u32;
    write32(otp, OTP_INT_ST, OTP_RD_DONE);
    if read32(otp, OTP_INT_ST) & OTP_RD_DONE != 0 {
        return Err(SensorError::InvalidReading);
    }
    write32(otp, OTP_AUTO_CTRL, (address << 16) | OTP_BURST_ONE);
    write32(otp, OTP_AUTO_EN, 1);
    for _ in 0..OTP_POLL_LIMIT_US {
        if read32(otp, OTP_INT_ST) & OTP_RD_DONE != 0 {
            write32(otp, OTP_INT_ST, OTP_RD_DONE);
            return Ok(read32(otp, OTP_DOUT0));
        }
        axklib::time::busy_wait(Duration::from_micros(1));
    }
    Err(SensorError::Timeout)
}

fn code_to_millidegrees(code: u32) -> Option<i32> {
    // RK3588 table from the local Orange Pi BSP rockchip_thermal.c. Reject
    // sentinel values, including values outside the characterized -40..125 C.
    const POINTS: [(u32, i32); 4] = [(215, -40_000), (285, 25_000), (350, 85_000), (395, 125_000)];
    let code = code & TSADC_CODE_MASK;
    for pair in POINTS.windows(2) {
        let (low_code, low_temp) = pair[0];
        let (high_code, high_temp) = pair[1];
        if (low_code..=high_code).contains(&code) {
            return Some(
                low_temp
                    + ((code - low_code) as i32 * (high_temp - low_temp))
                        / (high_code - low_code) as i32,
            );
        }
    }
    None
}

fn read32(base: NonNull<u8>, offset: usize) -> u32 {
    // SAFETY: callers validate a permanent mapping containing each aligned
    // 32-bit register offset; volatile access is required for MMIO semantics.
    unsafe { base.as_ptr().add(offset).cast::<u32>().read_volatile() }
}

fn write32(base: NonNull<u8>, offset: usize, value: u32) {
    // SAFETY: callers validate a permanent mapping containing each aligned
    // 32-bit register offset and own these writable OTP/CRU/TSADC registers.
    unsafe {
        base.as_ptr()
            .add(offset)
            .cast::<u32>()
            .write_volatile(value)
    }
}

#[cfg(test)]
mod tests {
    use super::code_to_millidegrees;

    #[test]
    fn rk3588_tsadc_thresholds_and_invalid_samples() {
        assert_eq!(code_to_millidegrees(285), Some(25_000));
        assert_eq!(code_to_millidegrees(350), Some(85_000));
        assert_eq!(code_to_millidegrees(299), Some(37_923));
        assert_eq!(code_to_millidegrees(0), None);
        assert_eq!(code_to_millidegrees(455), None);
    }
}
