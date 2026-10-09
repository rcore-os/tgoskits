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

//! RK3588 CPU and DSU GRF read-margin programming.
//!
//! The OPP table's `volt-mem-read-margin` property selects an SRAM read
//! margin for the confirmed CPU rail voltage. The CPU OPP transaction owns the
//! ordering: when raising frequency, set and confirm the supply before calling
//! [`ReadMargin::set_for_voltage`]; when lowering frequency, lower and confirm
//! the clock before updating the margin and supply. All calls must be serialized
//! with other users of these GRFs and the affected CPU domain.

use alloc::vec::Vec;
use core::time::Duration;

use crate::mmio::iomap;

const PAGE_SIZE: usize = 4096;
const MIN_GRF_SIZE: usize = 0x3c;
const CPU_RM_REGISTERS: [(usize, u32); 3] = [(0x20, 0x1c), (0x28, 0x3c), (0x2c, 0x3c)];
const DSU_RM_REGISTERS: [(usize, u32); 5] = [
    (0x20, 0x1c),
    (0x28, 0x3c),
    (0x2c, 0x3c),
    (0x30, 0x1c),
    (0x38, 0x1c),
];

/// One DT `reg` region for a CPU or DSU GRF.
#[derive(Clone, Copy)]
pub(crate) struct GrfResource {
    pub address: u64,
    pub size: u64,
}

/// A malformed DT resource/table or an unconfirmed GRF update.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MarginError {
    InvalidConfiguration,
    HardwareFailure,
}

#[derive(Clone, Copy)]
struct VoltageMargin {
    voltage_uv: u32,
    margin: u8,
}

fn parse_table(table_cells: &[u32]) -> Result<Vec<VoltageMargin>, MarginError> {
    let (pairs, remainder) = table_cells.as_chunks::<2>();
    if pairs.is_empty() || !remainder.is_empty() {
        return Err(MarginError::InvalidConfiguration);
    }
    let mut table = Vec::with_capacity(pairs.len());
    for pair in pairs {
        let voltage_uv = pair[0];
        let margin = u8::try_from(pair[1]).map_err(|_| MarginError::InvalidConfiguration)?;
        if voltage_uv == 0
            || margin > 7
            || table
                .last()
                .is_some_and(|previous: &VoltageMargin| previous.voltage_uv <= voltage_uv)
        {
            return Err(MarginError::InvalidConfiguration);
        }
        table.push(VoltageMargin { voltage_uv, margin });
    }
    Ok(table)
}

fn select_margin(table: &[VoltageMargin], voltage_uv: u32) -> Result<u8, MarginError> {
    table
        .iter()
        .find(|entry| voltage_uv >= entry.voltage_uv)
        .map(|entry| entry.margin)
        .ok_or(MarginError::InvalidConfiguration)
}

/// A mapped GRF. The mapping is permanent for the boot lifetime.
struct Grf {
    base: usize,
}

impl Grf {
    fn map(resource: GrfResource) -> Result<Self, MarginError> {
        let address =
            usize::try_from(resource.address).map_err(|_| MarginError::InvalidConfiguration)?;
        let size = usize::try_from(resource.size).map_err(|_| MarginError::InvalidConfiguration)?;
        if address == 0 || address & (PAGE_SIZE - 1) != 0 || size < MIN_GRF_SIZE {
            return Err(MarginError::InvalidConfiguration);
        }
        let map_size = size
            .checked_add(PAGE_SIZE - 1)
            .map(|length| length & !(PAGE_SIZE - 1))
            .ok_or(MarginError::InvalidConfiguration)?;
        address
            .checked_add(map_size)
            .ok_or(MarginError::InvalidConfiguration)?;
        let base = iomap(address, map_size)
            .map_err(|_| MarginError::HardwareFailure)?
            .as_ptr() as usize;
        Ok(Self { base })
    }

    fn read(&self, offset: usize) -> u32 {
        // SAFETY: `map` checked the DT-declared range and mapped at least one
        // page. Every offset passed below is word-aligned and below 0x3c, so
        // this volatile read is inside that live MMIO mapping. A raw pointer
        // avoids creating a Rust reference to device-owned register memory.
        unsafe { ((self.base + offset) as *const u32).read_volatile() }
    }

    fn write(&self, offset: usize, value: u32) {
        // SAFETY: The same range, alignment and boot-lifetime mapping proof as
        // `read` applies. The serialized OPP caller is the only writer to these
        // CPU/DSU read-margin fields while this driver is active.
        unsafe { ((self.base + offset) as *mut u32).write_volatile(value) }
    }
}

/// Controls a CPU GRF and, for the A55 domain, the shared DSU GRF.
pub(crate) struct ReadMargin {
    cpu: Grf,
    dsu: Option<Grf>,
    table: Vec<VoltageMargin>,
}

impl ReadMargin {
    /// Parse DT `[voltage_uv, margin]` cells in the BSP's descending order.
    /// Margin values occupy GRF bits `[4:2]`; a missing or malformed table
    /// cannot authorize a voltage transition.
    pub(crate) fn new(
        cpu_grf: GrfResource,
        dsu_grf: Option<GrfResource>,
        table_cells: &[u32],
    ) -> Result<Self, MarginError> {
        let table = parse_table(table_cells)?;
        let cpu = Grf::map(cpu_grf)?;
        let dsu = dsu_grf.map(Grf::map).transpose()?;
        Ok(Self { cpu, dsu, table })
    }

    /// Select the first threshold that the confirmed rail voltage meets.
    pub(crate) fn margin_for_voltage(&self, voltage_uv: u32) -> Result<u8, MarginError> {
        select_margin(&self.table, voltage_uv)
    }

    /// Read every affected field. Inconsistent values mean that the current
    /// hardware margin is not known, so the caller must keep high OPPs closed.
    pub(crate) fn current_margin(&self) -> Result<u8, MarginError> {
        let first = self.cpu.read(CPU_RM_REGISTERS[0].0);
        let margin = ((first & CPU_RM_REGISTERS[0].1) >> 2) as u8;
        if !self.matches_margin(margin) {
            return Err(MarginError::HardwareFailure);
        }
        Ok(margin)
    }

    /// Establish the margin for a rail whose voltage and bootstrap ring have
    /// already been read back. Firmware may leave different CPU/DSU fields;
    /// no prior software margin state is assumed here.
    pub(crate) fn establish_for_voltage(&self, voltage_uv: u32) -> Result<u8, MarginError> {
        let target = self.margin_for_voltage(voltage_uv)?;
        self.write_margin(target);
        self.matches_margin(target)
            .then_some(target)
            .ok_or(MarginError::HardwareFailure)
    }

    /// Program the margin selected by a *confirmed* CPU rail voltage.
    ///
    /// The old margin is read from every field before writing. If any field
    /// does not read back as requested, an attempt is made to restore the old
    /// margin; either way the caller receives an error and must withhold the
    /// target OPP until the whole transition has been checked.
    pub(crate) fn set_for_voltage(&self, voltage_uv: u32) -> Result<u8, MarginError> {
        let target = self.margin_for_voltage(voltage_uv)?;
        let old = self.current_margin()?;
        if target == old {
            return Ok(target);
        }
        self.write_margin(target);
        if self.matches_margin(target) {
            return Ok(target);
        }
        self.write_margin(old);
        let _ = self.matches_margin(old);
        Err(MarginError::HardwareFailure)
    }

    fn matches_margin(&self, margin: u8) -> bool {
        let expected = u32::from(margin) << 2;
        CPU_RM_REGISTERS
            .iter()
            .all(|&(offset, mask)| self.cpu.read(offset) & mask == expected & mask)
            && self.dsu.as_ref().is_none_or(|dsu| {
                DSU_RM_REGISTERS
                    .iter()
                    .all(|&(offset, mask)| dsu.read(offset) & mask == expected & mask)
            })
    }

    fn write_margin(&self, margin: u8) {
        let value = u32::from(margin) << 2;
        for &(offset, mask) in &CPU_RM_REGISTERS {
            self.cpu.write(offset, mask << 16 | value);
        }
        self.cpu.write(0x30, 0x0020_0020);
        axklib::time::busy_wait(Duration::from_micros(1));
        self.cpu.write(0x30, 0x0020_0000);

        if let Some(dsu) = &self.dsu {
            for &(offset, mask) in &DSU_RM_REGISTERS {
                dsu.write(offset, mask << 16 | value);
            }
            dsu.write(0x18, 0x4000_4000);
            axklib::time::busy_wait(Duration::from_micros(1));
            dsu.write(0x18, 0x4000_0000);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MarginError, parse_table, select_margin};

    #[test]
    fn bsp_thresholds_select_the_first_matching_voltage() {
        let table = parse_table(&[855_000, 1, 765_000, 2, 675_000, 3, 495_000, 4]).unwrap();
        assert_eq!(select_margin(&table, 925_000), Ok(1));
        assert_eq!(select_margin(&table, 800_000), Ok(2));
        assert_eq!(select_margin(&table, 750_000), Ok(3));
        assert_eq!(
            select_margin(&table, 494_999),
            Err(MarginError::InvalidConfiguration)
        );
        assert!(parse_table(&[675_000, 3, 765_000, 2]).is_err());
    }
}
