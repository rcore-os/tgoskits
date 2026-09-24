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

//! RK3588 CPU PVTPLL measurement and voltage-grade selection.
//!
//! The caller must first set the CPU rail and SCMI clock to the measurement OPP
//! from the corresponding device-tree table, wait its `rockchip,pvtm-sample-time`,
//! and restore both resources after sampling. In the Orange Pi 5 Plus DTB these
//! measurement points are 750 mV / 1.416 GHz for the A55 and 750 mV / 1.608 GHz
//! for either A76 pair, with a 1100 us settling time. This module deliberately
//! does not change the rail or clock: their transition and rollback belong to
//! the serialized CPU OPP transaction.

use core::mem::size_of;

use crate::{mmio::iomap, probe::OnProbeError};

/// Read a CPU PVTPLL sample from the GRF selected by the OPP table's
/// `rockchip,grf` phandle and `rockchip,pvtm-offset` property.
///
/// `grf_phys` and `grf_size` must be taken from that GRF node's `reg` property.
/// The function rejects an offset outside the declared register range and a
/// zero sample, which the BSP treats as unavailable. A successful result is a
/// raw sample only; temperature correction and voltage-grade selection still
/// have to succeed before it can authorize an OPP.
pub(crate) fn read_raw_sample(
    grf_phys: u64,
    grf_size: u64,
    offset: u32,
) -> Result<u32, OnProbeError> {
    let address = usize::try_from(grf_phys)
        .map_err(|_| OnProbeError::other("PVTM GRF address does not fit usize"))?;
    let size = usize::try_from(grf_size)
        .map_err(|_| OnProbeError::other("PVTM GRF size does not fit usize"))?;
    let offset = offset as usize;
    let end = offset
        .checked_add(size_of::<u32>())
        .ok_or_else(|| OnProbeError::other("PVTM GRF register offset overflow"))?;
    if address == 0 || address & 0xfff != 0 || offset & 3 != 0 || end > size {
        return Err(OnProbeError::other("invalid PVTM GRF register range"));
    }

    let map_size = size
        .checked_add(0xfff)
        .map(|length| length & !0xfff)
        .ok_or_else(|| OnProbeError::other("PVTM GRF mapping size overflow"))?;
    address
        .checked_add(map_size)
        .ok_or_else(|| OnProbeError::other("PVTM GRF mapping address overflow"))?;
    let base = iomap(address, map_size)?;

    // SAFETY: iomap returned a device mapping of at least map_size bytes. The
    // checked end is inside the DT-declared GRF range and offset is u32-aligned;
    // the mapped page base is aligned. Only a volatile read is performed, so no
    // Rust reference aliases a register that hardware may update concurrently.
    let raw = unsafe { base.as_ptr().add(offset).cast::<u32>().read_volatile() };
    if raw == 0 {
        return Err(OnProbeError::other("PVTM GRF returned an empty sample"));
    }
    Ok(raw)
}

/// Apply the BSP's temperature correction to a raw PVTPLL sample.
///
/// Temperatures are in millicelsius; the reference temperature is in Celsius.
/// `temp_prop[0]` applies below the reference and `temp_prop[1]` at or above it.
/// The BSP truncates the Celsius conversion before applying the coefficient.
/// An invalid or non-positive corrected value cannot select a voltage grade.
pub(crate) fn temperature_correct(
    raw: u32,
    temperature_mc: i32,
    reference_c: i32,
    temp_prop: [i32; 2],
) -> Option<u32> {
    let raw = i32::try_from(raw).ok()?;
    if raw <= 0 {
        return None;
    }
    let delta_c = i64::from(temperature_mc / 1000) - i64::from(reference_c);
    let coefficient = if delta_c < 0 {
        temp_prop[0]
    } else {
        temp_prop[1]
    };
    let corrected = i64::from(raw) + delta_c * i64::from(coefficient) / 1000;
    i32::try_from(corrected)
        .ok()
        .filter(|value| *value > 0)
        .map(|value| value as u32)
}

/// Select the BSP voltage grade from a DT `rockchip,pvtm-voltage-sel*` table.
///
/// Each row is `[min, max, grade]`. Like `rockchip_get_sel()`, selection uses
/// the **last** row whose `min <= sample`; `max` is only checked for malformed
/// table data. This intentionally permits a sample above the last row's `max`,
/// matching the BSP. The rows must have increasing, non-overlapping ranges.
pub(crate) fn select_voltage_grade(sample: u32, rows: &[[u32; 3]]) -> Option<u32> {
    if sample == 0 {
        return None;
    }
    let mut selected = None;
    let mut previous_max = None;
    for &[min, max, grade] in rows {
        if min > max || previous_max.is_some_and(|prev| min <= prev) {
            return None;
        }
        if sample >= min {
            selected = Some(grade);
        }
        previous_max = Some(max);
    }
    selected
}

/// The BSP selects the hardware-bin PVTM table for J/M SKUs when its mask
/// contains the OTP bin; other bins use the normal table.
pub(crate) fn uses_hardware_bin_table(bin: u32, pvtm_hw_mask: u32) -> bool {
    bin > 0
        && 1_u32
            .checked_shl(bin)
            .is_some_and(|bit| pvtm_hw_mask & bit != 0)
}

#[cfg(test)]
mod tests {
    use super::{select_voltage_grade, temperature_correct, uses_hardware_bin_table};

    #[test]
    fn bsp_temperature_and_bin_select_a_safe_voltage_grade() {
        // The BSP truncates 20.9 C to 20 C before applying the coefficient.
        let sample = temperature_correct(1600, 20_900, 25, [270, 270]).unwrap();
        assert_eq!(sample, 1599);

        let standard = [[0, 1595, 0], [1596, 1615, 1], [1616, 9999, 2]];
        let hardware_bin = [[0, 1539, 0], [1540, 1564, 1], [1565, 9999, 2]];
        let table = if uses_hardware_bin_table(2, 0x06) {
            &hardware_bin
        } else {
            &standard
        };
        assert_eq!(select_voltage_grade(sample, table), Some(2));
        assert_eq!(select_voltage_grade(sample, &standard), Some(1));
        assert!(!uses_hardware_bin_table(0, 0x06));
        assert!(!uses_hardware_bin_table(3, 0x06));
    }

    #[test]
    fn malformed_measurement_or_table_cannot_authorize_an_opp() {
        assert_eq!(temperature_correct(0, 25_000, 25, [270, 270]), None);
        assert_eq!(temperature_correct(1, -40_000, 25, [270, 270]), None);
        assert_eq!(select_voltage_grade(100, &[[200, 300, 1]]), None);
        assert_eq!(
            select_voltage_grade(100, &[[0, 100, 0], [100, 200, 1]]),
            None
        );
        assert_eq!(
            select_voltage_grade(100, &[[0, 99, 0], [200, 100, 1]]),
            None
        );
        assert_eq!(select_voltage_grade(10_000, &[[0, 9999, 7]]), Some(7));
    }
}
