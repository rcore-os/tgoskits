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
///
/// Marked `#[non_exhaustive]`: HS400 / strobe encodings may be classified out
/// of `Unknown(_)` over time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
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
///
/// Marked `#[non_exhaustive]`: HS400_ES and future timing modes may be
/// classified out of `Unknown(_)` over time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
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

    /// Extended CSD structure revision.
    pub fn revision(&self) -> u8 {
        self.raw[ext_csd::REV]
    }

    /// Volatile write-cache capacity in KiB.
    ///
    /// The cache-size field was introduced with eMMC 4.5 (EXT_CSD revision
    /// 6). Older cards must be treated as having no controllable cache.
    pub fn cache_size_kib(&self) -> u32 {
        if self.revision() < 6 {
            return 0;
        }
        let offset = ext_csd::CACHE_SIZE;
        u32::from_le_bytes([
            self.raw[offset],
            self.raw[offset + 1],
            self.raw[offset + 2],
            self.raw[offset + 3],
        ])
    }

    /// Whether a non-empty volatile write cache is currently enabled.
    pub fn cache_enabled(&self) -> bool {
        self.cache_size_kib() != 0 && self.raw[ext_csd::CACHE_CTRL] & 1 != 0
    }

    /// Whether the card implements the eMMC command queue protocol.
    pub fn command_queue_supported(&self) -> bool {
        self.revision() >= 8
            && self.raw[ext_csd::CMDQ_SUPPORT] & 1 != 0
            && self.command_queue_depth() > 2
    }

    /// Number of data-task slots implemented by the card.
    ///
    /// JESD84 encodes the maximum zero-based task index. CQHCI reserves its
    /// final host slot for direct commands, so callers still need to clamp
    /// this value to the host's data-slot count.
    pub fn command_queue_depth(&self) -> usize {
        usize::from(self.raw[ext_csd::CMDQ_DEPTH] & 0x1f) + 1
    }

    /// Whether command queue mode is currently selected on the card.
    pub fn command_queue_enabled(&self) -> bool {
        self.raw[ext_csd::CMDQ_MODE_EN] & 1 != 0
    }

    /// Whether standard insecure TRIM is declared by SEC_GB_CL_EN.
    pub fn supports_trim(&self) -> bool {
        self.revision() >= 4 && self.raw[ext_csd::SEC_FEATURE_SUPPORT] & (1 << 4) != 0
    }

    /// Explicitly erased byte value; reserved encodings remain unknown.
    pub fn erased_byte(&self) -> Option<u8> {
        match self.raw[ext_csd::ERASED_MEM_CONT] {
            0 => Some(0),
            1 => Some(0xff),
            _ => None,
        }
    }

    /// Active HC erase-group size in 512-byte blocks. Legacy CSD geometry
    /// and a zero HC size are deliberately unsupported by this accessor.
    pub fn hc_erase_group_blocks(&self) -> Option<u32> {
        let size = self.raw[ext_csd::HC_ERASE_GRP_SIZE];
        if self.raw[ext_csd::ERASE_GROUP_DEF] & 1 == 0 || size == 0 {
            return None;
        }
        Some(u32::from(size) * 1024)
    }

    /// Busy budget for restoring the volatile HC erase definition, if needed.
    /// Old cards, missing HC geometry, and already selected geometry are left
    /// unchanged. Undefined CMD6 timing uses Linux's conservative 500 ms default.
    #[cfg(feature = "sdio")]
    pub(crate) fn hc_erase_group_switch_timeout_ms(&self) -> Option<u64> {
        if self.revision() < 3
            || self.raw[ext_csd::HC_ERASE_GRP_SIZE] == 0
            || self.raw[ext_csd::ERASE_GROUP_DEF] & 1 != 0
        {
            return None;
        }
        let multiplier = self.raw[ext_csd::GENERIC_CMD6_TIME];
        Some(if self.revision() >= 6 && multiplier != 0 {
            u64::from(multiplier) * 10
        } else {
            500
        })
    }

    /// Records ERASE_GROUP_DEF=1 only after CMD6 and ready-state confirmation.
    #[cfg(feature = "sdio")]
    pub(crate) fn set_hc_erase_group_enabled(&mut self) {
        self.raw[ext_csd::ERASE_GROUP_DEF] = 1;
    }

    /// Maximum standard TRIM busy time per affected erase group in ms.
    /// Zero is unspecified and must not be used as an immediate deadline.
    pub fn trim_timeout_ms(&self) -> Option<u32> {
        let multiplier = self.raw[ext_csd::TRIM_MULT];
        (self.revision() >= 4 && multiplier != 0).then_some(u32::from(multiplier) * 300)
    }

    #[cfg(any(feature = "sdio", test))]
    pub(crate) fn set_cache_enabled(&mut self, enabled: bool) {
        if enabled {
            self.raw[ext_csd::CACHE_CTRL] |= 1;
        } else {
            self.raw[ext_csd::CACHE_CTRL] &= !1;
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

    #[test]
    fn cache_flush_requires_a_nonzero_enabled_cache() {
        let mut raw = [0u8; 512];
        raw[192] = 6;
        raw[249..253].copy_from_slice(&1024u32.to_le_bytes());
        let mut ext_csd = ExtCsd::from_bytes(raw);

        assert_eq!(ext_csd.cache_size_kib(), 1024);
        assert!(!ext_csd.cache_enabled());

        ext_csd.set_cache_enabled(true);
        assert!(ext_csd.cache_enabled());
    }

    #[test]
    fn command_queue_fields_decode_capability_depth_and_mode() {
        let mut raw = [0u8; 512];
        raw[ext_csd::REV] = 8;
        raw[ext_csd::CMDQ_SUPPORT] = 1;
        raw[ext_csd::CMDQ_DEPTH] = 30;
        raw[ext_csd::CMDQ_MODE_EN] = 1;
        let ext_csd = ExtCsd::from_bytes(raw);

        assert!(ext_csd.command_queue_supported());
        assert_eq!(ext_csd.command_queue_depth(), 31);
        assert!(ext_csd.command_queue_enabled());
    }

    #[test]
    fn command_queue_rejects_pre_51_and_inefficient_depths() {
        let mut raw = [0u8; 512];
        raw[ext_csd::REV] = 7;
        raw[ext_csd::CMDQ_SUPPORT] = 1;
        raw[ext_csd::CMDQ_DEPTH] = 31;
        assert!(!ExtCsd::from_bytes(raw).command_queue_supported());

        raw[ext_csd::REV] = 8;
        raw[ext_csd::CMDQ_DEPTH] = 1;
        assert!(!ExtCsd::from_bytes(raw).command_queue_supported());
    }
}
