//! RK3588 CPU OPP selection from the live device tree.
//!
//! This module only selects and validates hardware operating points. The caller
//! must establish the OTP and PVTM evidence, then decide when it is safe to
//! apply an OPP to the clock, regulators, GRF, and firmware as one transition.

use alloc::{format, vec::Vec};

use fdt_edit::{Fdt, Node, Phandle, Status};

/// Voltage requested by an OPP, with its regulator bounds in microvolts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OppVoltage {
    pub target_uv: u32,
    pub min_uv: u32,
    pub max_uv: u32,
}

/// One selected CPU OPP. The second supply is the optional `mem-supply` entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CpuOpp {
    pub frequency_hz: u64,
    pub cpu: OppVoltage,
    pub memory: Option<OppVoltage>,
}

/// The six-byte RK3588 `opp-info` OTP cell used by the BSP.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OtpOppInfo {
    pub min_freq_mhz: u16,
    pub max_freq_mhz: u16,
    pub added_mv: u8,
    pub pvtpll_length: u8,
}

impl OtpOppInfo {
    /// Parses the packed little-endian BSP `struct otp_opp_info`.
    pub fn from_cell(raw: &[u8]) -> Result<Self, OppTableError> {
        let [min0, min1, max0, max1, added_mv, pvtpll_length] = raw else {
            return Err(OppTableError::InvalidOtp);
        };
        let info = Self {
            min_freq_mhz: u16::from_le_bytes([*min0, *min1]),
            max_freq_mhz: u16::from_le_bytes([*max0, *max1]),
            added_mv: *added_mv,
            pvtpll_length: *pvtpll_length,
        };
        // The BSP ignores a zero voltage correction. A nonzero correction
        // requires a meaningful inclusive frequency interval.
        if info.added_mv != 0 && (info.min_freq_mhz == 0 || info.max_freq_mhz < info.min_freq_mhz) {
            return Err(OppTableError::InvalidOtp);
        }
        Ok(info)
    }

    fn applies_to(self, frequency_hz: u64) -> bool {
        self.added_mv != 0
            && frequency_hz >= u64::from(self.min_freq_mhz) * 1_000_000
            && frequency_hz <= u64::from(self.max_freq_mhz) * 1_000_000
    }
}

/// Confirmed RK3588 bin and voltage grade. Construction requires OTP cell data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HardwareSelection {
    pub bin: u8,
    pub voltage_grade: u8,
    pub otp_opp_info: OtpOppInfo,
}

impl HardwareSelection {
    /// Matches the Orange Pi BSP's RK3588M/J serial-number bin mapping.
    pub fn from_otp(
        specification_serial_number: u8,
        voltage_grade: u8,
        opp_info_cell: &[u8],
    ) -> Result<Self, OppTableError> {
        if voltage_grade >= 32 {
            return Err(OppTableError::InvalidHardwareSelection);
        }
        // The DT nvmem cell selects bits [4:0] from OTP offset 0x06. Mask
        // here as well so a caller passing the raw byte gets the same bin.
        let bin = match specification_serial_number & 0x1f {
            0x0d => 1, // RK3588M
            0x0a => 2, // RK3588J
            _ => 0,
        };
        Ok(Self {
            bin,
            voltage_grade,
            otp_opp_info: OtpOppInfo::from_cell(opp_info_cell)?,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OppTableError {
    MissingTable,
    InvalidTable,
    InvalidOpp,
    InvalidOtp,
    InvalidHardwareSelection,
    NoSupportedOpp,
    DuplicateFrequency,
}

/// Selects OPPs for one CPU domain from its `operating-points-v2` phandle.
///
/// The caller supplies a confirmed OTP serial, OTP OPP correction and PVTM
/// grade via [`HardwareSelection`]. A missing or malformed critical property
/// rejects the table rather than admitting a possibly unsafe high OPP.
pub fn parse_domain_opps(
    fdt: &Fdt,
    cpu_node: &Node,
    selection: HardwareSelection,
) -> Result<Vec<CpuOpp>, OppTableError> {
    if selection.bin >= 32 || selection.voltage_grade >= 32 {
        return Err(OppTableError::InvalidHardwareSelection);
    }
    let table_phandle = cpu_node
        .get_property("operating-points-v2")
        .and_then(|p| p.get_u32())
        .filter(|p| *p != 0)
        .ok_or(OppTableError::MissingTable)?;
    let table = fdt
        .get_by_phandle(Phandle::from(table_phandle))
        .ok_or(OppTableError::MissingTable)?;
    let table = table.as_node();
    if !table
        .compatibles()
        .any(|compatible| compatible == "operating-points-v2")
        || table.get_property("rockchip,supported-hw").is_none()
        || matches!(table.status(), Some(Status::Disabled))
    {
        return Err(OppTableError::InvalidTable);
    }

    let mut selected = Vec::new();
    for id in table.children() {
        let row = fdt.node(*id).ok_or(OppTableError::InvalidTable)?;
        if matches!(row.status(), Some(Status::Disabled)) {
            continue;
        }
        if !supported_by_hardware(row, selection)? {
            continue;
        }
        let frequency_hz = row
            .get_property("opp-hz")
            .and_then(|p| p.get_u64())
            .filter(|frequency| *frequency != 0)
            .ok_or(OppTableError::InvalidOpp)?;
        let voltage_property = format!("opp-microvolt-L{}", selection.voltage_grade);
        let voltages = row
            .get_property(&voltage_property)
            .or_else(|| row.get_property("opp-microvolt"))
            .ok_or(OppTableError::InvalidOpp)?;
        let (mut cpu, mut memory) = parse_voltages(&voltages.data)?;
        if selection.otp_opp_info.applies_to(frequency_hz) {
            let added_uv = u32::from(selection.otp_opp_info.added_mv) * 1000;
            cpu.target_uv = cpu.target_uv.saturating_add(added_uv).min(cpu.max_uv);
            if let Some(supply) = memory.as_mut() {
                supply.target_uv = supply.target_uv.saturating_add(added_uv).min(supply.max_uv);
            }
        }
        if !(675_000..=1_000_000).contains(&cpu.target_uv)
            || cpu.target_uv % 6_250 != 0
            || memory.is_some_and(|supply| {
                supply.target_uv != cpu.target_uv || supply.target_uv % 6_250 != 0
            })
        {
            return Err(OppTableError::InvalidOpp);
        }
        selected.push(CpuOpp {
            frequency_hz,
            cpu,
            memory,
        });
    }
    if selected.is_empty() {
        return Err(OppTableError::NoSupportedOpp);
    }
    selected.sort_unstable_by_key(|opp| opp.frequency_hz);
    if selected
        .windows(2)
        .any(|pair| pair[0].frequency_hz == pair[1].frequency_hz)
    {
        return Err(OppTableError::DuplicateFrequency);
    }
    if selected
        .windows(2)
        .any(|pair| pair[0].cpu.target_uv > pair[1].cpu.target_uv)
    {
        return Err(OppTableError::InvalidOpp);
    }
    Ok(selected)
}

fn supported_by_hardware(row: &Node, selection: HardwareSelection) -> Result<bool, OppTableError> {
    let masks = &row
        .get_property("opp-supported-hw")
        .ok_or(OppTableError::InvalidOpp)?
        .data;
    // Linux OPP supports multiple versions of the two-level mask and accepts
    // the row if either version matches both the SoC bin and voltage grade.
    if masks.is_empty() || masks.len() % 8 != 0 {
        return Err(OppTableError::InvalidOpp);
    }
    let bin_bit = 1u32 << selection.bin;
    let grade_bit = 1u32 << selection.voltage_grade;
    let (versions, remainder) = masks.as_chunks::<8>();
    if !remainder.is_empty() {
        return Err(OppTableError::InvalidOpp);
    }
    Ok(versions.iter().any(|version| {
        let bin_mask = u32::from_be_bytes(version[..4].try_into().unwrap());
        let grade_mask = u32::from_be_bytes(version[4..].try_into().unwrap());
        (bin_mask & bin_bit != 0) && (grade_mask & grade_bit != 0)
    }))
}

fn parse_voltages(data: &[u8]) -> Result<(OppVoltage, Option<OppVoltage>), OppTableError> {
    if data.len() != 12 && data.len() != 24 {
        return Err(OppTableError::InvalidOpp);
    }
    let voltage = |offset| -> Result<OppVoltage, OppTableError> {
        let cell = |index| u32::from_be_bytes(data[index..index + 4].try_into().unwrap());
        let voltage = OppVoltage {
            target_uv: cell(offset),
            min_uv: cell(offset + 4),
            max_uv: cell(offset + 8),
        };
        if voltage.target_uv == 0
            || voltage.min_uv > voltage.target_uv
            || voltage.target_uv > voltage.max_uv
        {
            return Err(OppTableError::InvalidOpp);
        }
        Ok(voltage)
    };
    Ok((
        voltage(0)?,
        (data.len() == 24).then(|| voltage(12)).transpose()?,
    ))
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use fdt_edit::{Fdt, Node, Property};

    use super::*;

    fn u32s(name: &str, values: &[u32]) -> Property {
        let mut property = Property::new(name, vec![]);
        property.set_u32_ls(values);
        property
    }

    fn fixture() -> (Fdt, fdt_edit::NodeId) {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        let cpu = fdt.add_node(root, Node::new("cpu@0"));
        fdt.node_mut(cpu)
            .unwrap()
            .add_property(u32s("operating-points-v2", &[1]));
        let mut table_node = Node::new("cluster0-opp-table");
        table_node.add_property(Property::new(
            "compatible",
            b"operating-points-v2\0".to_vec(),
        ));
        table_node.add_property(Property::new("rockchip,supported-hw", vec![]));
        table_node.add_property(u32s("phandle", &[1]));
        let table = fdt.add_node(root, table_node);
        for (name, masks, mhz, uv) in [
            ("standard-1200", [0xf9, 0xffff], 1200, 700_000),
            ("standard-1800", [0xf9, 0xffff], 1800, 900_000),
            ("standard-2208-grade2", [0xf9, 0x04], 2208, 950_000),
            ("j-m-1200", [0x06, 0xffff], 1200, 750_000),
            ("j-m-1704", [0x06, 0xffff], 1704, 925_000),
        ] {
            let id = fdt.add_node(table, Node::new(name));
            let row = fdt.node_mut(id).unwrap();
            row.add_property(u32s("opp-supported-hw", &masks));
            row.add_property(Property::new(
                "opp-hz",
                (mhz as u64 * 1_000_000).to_be_bytes().to_vec(),
            ));
            row.add_property(u32s(
                "opp-microvolt",
                &[uv, uv, 1_000_000, uv, uv, 1_000_000],
            ));
        }
        (fdt, cpu)
    }

    fn selection(serial: u8, grade: u8) -> HardwareSelection {
        HardwareSelection::from_otp(serial, grade, &[0; 6]).unwrap()
    }

    #[test]
    fn selects_the_bsp_sku_rows_and_voltage_grade() {
        let (mut fdt, cpu) = fixture();
        let table = fdt.get_by_phandle_id(Phandle::from(1)).unwrap();
        let high = fdt.node(table).unwrap().children()[1];
        fdt.node_mut(high).unwrap().set_property(u32s(
            "opp-microvolt-L3",
            &[850_000, 850_000, 1_000_000, 850_000, 850_000, 1_000_000],
        ));
        let standard = parse_domain_opps(&fdt, fdt.node(cpu).unwrap(), selection(0, 3)).unwrap();
        assert_eq!(standard.len(), 2);
        assert_eq!(standard[1].frequency_hz, 1_800_000_000);
        assert_eq!(standard[1].cpu.target_uv, 850_000);
        let grade2 = parse_domain_opps(&fdt, fdt.node(cpu).unwrap(), selection(0, 2)).unwrap();
        assert_eq!(grade2.len(), 3);
        assert_eq!(grade2[2].frequency_hz, 2_208_000_000);
        let j = parse_domain_opps(&fdt, fdt.node(cpu).unwrap(), selection(0x2a, 3)).unwrap();
        assert_eq!(j.len(), 2);
        assert_eq!(j[1].frequency_hz, 1_704_000_000);
        assert_eq!(j[0].cpu.target_uv, 750_000);
        assert_eq!(selection(0x2d, 3).bin, 1);
    }

    #[test]
    fn applies_otp_correction_to_both_supplies_with_bsp_clamp() {
        let (fdt, cpu) = fixture();
        let mut selection = selection(0, 0);
        selection.otp_opp_info = OtpOppInfo::from_cell(&[0xb0, 0x04, 0x08, 0x07, 200, 0]).unwrap();
        let opps = parse_domain_opps(&fdt, fdt.node(cpu).unwrap(), selection).unwrap();
        assert_eq!(opps[0].cpu.target_uv, 900_000);
        assert_eq!(opps[0].memory.unwrap().target_uv, 900_000);
        assert_eq!(opps[1].cpu.target_uv, 1_000_000);
    }

    #[test]
    fn rejects_missing_evidence_and_ambiguous_rows() {
        assert_eq!(
            HardwareSelection::from_otp(0, 32, &[0; 6]),
            Err(OppTableError::InvalidHardwareSelection)
        );
        assert_eq!(
            HardwareSelection::from_otp(0, 0, &[0; 5]),
            Err(OppTableError::InvalidOtp)
        );
        let (mut fdt, cpu) = fixture();
        let table = fdt.get_by_phandle_id(Phandle::from(1)).unwrap();
        let duplicate = fdt.node(table).unwrap().children()[3];
        fdt.node_mut(duplicate)
            .unwrap()
            .set_property(u32s("opp-supported-hw", &[0xff, 0xffff]));
        assert_eq!(
            parse_domain_opps(&fdt, fdt.node(cpu).unwrap(), selection(0, 0)),
            Err(OppTableError::DuplicateFrequency)
        );

        let (mut fdt, cpu) = fixture();
        let table = fdt.get_by_phandle_id(Phandle::from(1)).unwrap();
        let higher = fdt.node(table).unwrap().children()[1];
        fdt.node_mut(higher).unwrap().set_property(u32s(
            "opp-microvolt",
            &[675_000, 675_000, 1_000_000, 675_000, 675_000, 1_000_000],
        ));
        assert_eq!(
            parse_domain_opps(&fdt, fdt.node(cpu).unwrap(), selection(0, 0)),
            Err(OppTableError::InvalidOpp)
        );
    }
}
