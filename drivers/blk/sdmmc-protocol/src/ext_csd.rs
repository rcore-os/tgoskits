//! Extended CSD (EXT_CSD) register parsing for eMMC / MMC cards.
//!
//! EXT_CSD is a 512-byte register read with `CMD8` (`SEND_EXT_CSD`,
//! `R1` + 512-byte data block) on MMC cards. Only a small subset of
//! fields is consumed by this driver today; the parser exposes the
//! ones that drive bring-up decisions (capacity, supported timing,
//! current bus width).

use crate::cmd::ext_csd;

/// Lightly typed view over the 512-byte EXT_CSD payload.
///
/// Holding the full register lets later phases of the driver read
/// fields that aren't needed today (e.g. `BOOT_PARTITION_SIZE`,
/// `RPMB_SIZE_MULT`, `PARTITION_SETTING_COMPLETED`) without having to
/// re-issue CMD8.
#[derive(Debug, Clone)]
pub struct ExtCsd {
    raw: [u8; 512],
}

/// `DEVICE_TYPE` (EXT_CSD[196]) decoded into supported high-speed modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceType {
    pub raw: u8,
}

/// Currently selected MMC bus width, decoded from `BUS_WIDTH` (EXT_CSD[183]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmcBusWidth {
    /// 1-bit SDR.
    Sdr1,
    /// 4-bit SDR.
    Sdr4,
    /// 8-bit SDR.
    Sdr8,
    /// 4-bit DDR.
    Ddr4,
    /// 8-bit DDR.
    Ddr8,
    /// Reserved / unknown encoding — caller should treat as 1-bit.
    Unknown(u8),
}

/// Currently selected MMC timing mode, decoded from `HS_TIMING` (EXT_CSD[185]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmcTiming {
    /// Backwards-compatible (≤ 26 MHz).
    Compat,
    /// High-Speed SDR (≤ 52 MHz).
    HighSpeed,
    /// HS200 (≤ 200 MHz, 1.8 V or 1.2 V).
    Hs200,
    /// HS400 (≤ 200 MHz DDR, requires HS200 tuning first).
    Hs400,
    /// Reserved / unknown encoding.
    Unknown(u8),
}

impl ExtCsd {
    pub fn from_bytes(raw: [u8; 512]) -> Self {
        Self { raw }
    }

    /// Raw 512-byte payload (immutable view) for fields the typed API
    /// hasn't grown to cover yet.
    pub fn as_bytes(&self) -> &[u8; 512] {
        &self.raw
    }

    /// Authoritative sector count for cards ≥ 2 GB. Returns `None` when
    /// the field is zero, which means "use the legacy CSD `C_SIZE`
    /// instead" (small cards).
    pub fn sector_count(&self) -> Option<u32> {
        let s = ext_csd::SEC_COUNT;
        let v = u32::from_le_bytes([
            self.raw[s],
            self.raw[s + 1],
            self.raw[s + 2],
            self.raw[s + 3],
        ]);
        if v == 0 { None } else { Some(v) }
    }

    pub fn device_type(&self) -> DeviceType {
        DeviceType {
            raw: self.raw[ext_csd::DEVICE_TYPE],
        }
    }

    pub fn bus_width(&self) -> MmcBusWidth {
        match self.raw[ext_csd::BUS_WIDTH] {
            0 => MmcBusWidth::Sdr1,
            1 => MmcBusWidth::Sdr4,
            2 => MmcBusWidth::Sdr8,
            5 => MmcBusWidth::Ddr4,
            6 => MmcBusWidth::Ddr8,
            other => MmcBusWidth::Unknown(other),
        }
    }

    pub fn timing(&self) -> MmcTiming {
        match self.raw[ext_csd::HS_TIMING] & 0x0F {
            0 => MmcTiming::Compat,
            1 => MmcTiming::HighSpeed,
            2 => MmcTiming::Hs200,
            3 => MmcTiming::Hs400,
            other => MmcTiming::Unknown(other),
        }
    }
}

impl DeviceType {
    pub fn supports_hs_52(&self) -> bool {
        self.raw & ext_csd::device_type::HS_52 != 0
    }
    pub fn supports_hs_26(&self) -> bool {
        self.raw & ext_csd::device_type::HS_26 != 0
    }
    pub fn supports_hs200_18v(&self) -> bool {
        self.raw & ext_csd::device_type::HS200_18V != 0
    }
    pub fn supports_hs200_12v(&self) -> bool {
        self.raw & ext_csd::device_type::HS200_12V != 0
    }
    pub fn supports_hs200(&self) -> bool {
        self.supports_hs200_18v() || self.supports_hs200_12v()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ext_csd_with(field: usize, val: &[u8]) -> ExtCsd {
        let mut raw = [0u8; 512];
        raw[field..field + val.len()].copy_from_slice(val);
        ExtCsd::from_bytes(raw)
    }

    #[test]
    fn sector_count_from_ext_csd() {
        // 0x0080_0000 sectors = 4 GiB
        let e = ext_csd_with(ext_csd::SEC_COUNT, &[0x00, 0x00, 0x80, 0x00]);
        assert_eq!(e.sector_count(), Some(0x0080_0000));
    }

    #[test]
    fn sector_count_zero_means_use_csd() {
        let e = ExtCsd::from_bytes([0u8; 512]);
        assert_eq!(e.sector_count(), None);
    }

    #[test]
    fn device_type_decodes_known_bits() {
        let e = ext_csd_with(ext_csd::DEVICE_TYPE, &[0b0011_0011]);
        let dt = e.device_type();
        assert!(dt.supports_hs_26());
        assert!(dt.supports_hs_52());
        assert!(dt.supports_hs200_18v());
        assert!(dt.supports_hs200_12v());
        assert!(dt.supports_hs200());
    }

    #[test]
    fn bus_width_and_timing_round_trip() {
        let mut raw = [0u8; 512];
        raw[ext_csd::BUS_WIDTH] = 2; // 8-bit SDR
        raw[ext_csd::HS_TIMING] = 2; // HS200
        let e = ExtCsd::from_bytes(raw);
        assert_eq!(e.bus_width(), MmcBusWidth::Sdr8);
        assert_eq!(e.timing(), MmcTiming::Hs200);
    }
}
