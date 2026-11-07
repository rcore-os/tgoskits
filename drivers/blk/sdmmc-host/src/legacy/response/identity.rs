/// OCR register (R3/CMD58 response)
#[derive(Debug, Clone, Copy)]
pub struct OcrResponse {
    pub raw: u32,
}

impl OcrResponse {
    pub fn from_raw(raw: u32) -> Self {
        Self { raw }
    }

    /// Card power up status — true if card has completed power-up
    pub fn card_powered_up(&self) -> bool {
        self.raw & (1 << 31) != 0
    }

    /// Card Capacity Status (CCS): true = SDHC/SDXC, false = SDSC
    pub fn ccs(&self) -> bool {
        self.raw & (1 << 30) != 0
    }

    /// Supported voltage range (bits 23:0)
    pub fn voltage_window(&self) -> u32 {
        self.raw & 0x00FF_FF00
    }

    /// Supports 3.5–3.6V
    pub fn vdd_35_36(&self) -> bool {
        self.raw & (1 << 23) != 0
    }

    /// Supports 3.4–3.5V
    pub fn vdd_34_35(&self) -> bool {
        self.raw & (1 << 22) != 0
    }

    /// Supports 3.3–3.4V
    pub fn vdd_33_34(&self) -> bool {
        self.raw & (1 << 21) != 0
    }

    /// Supports 3.2–3.3V
    pub fn vdd_32_33(&self) -> bool {
        self.raw & (1 << 20) != 0
    }

    /// Supports 2.7–3.6V (typical operating range)
    pub fn supports_2v7_to_3v6(&self) -> bool {
        self.raw & 0x00FF_8000 != 0
    }

    /// UHS-II supported
    pub fn uhs2(&self) -> bool {
        self.raw & (1 << 29) != 0
    }

    /// Switching to 1.8 V was accepted during SD ACMD41 negotiation.
    pub fn s18a(&self) -> bool {
        self.raw & (1 << 24) != 0
    }
}

/// R6: Published RCA response
#[derive(Debug, Clone, Copy)]
pub struct RcaResponse {
    pub raw: u32,
}

impl RcaResponse {
    pub fn from_raw(raw: u32) -> Self {
        Self { raw }
    }

    /// Relative card address (bits 31:16)
    pub fn rca(&self) -> u16 {
        ((self.raw >> 16) & 0xFFFF) as u16
    }

    /// Status bits (bits 15:0) — subset of R1 status
    pub fn status(&self) -> u16 {
        (self.raw & 0xFFFF) as u16
    }
}

/// R7: Interface condition response
#[derive(Debug, Clone, Copy)]
pub struct IfCondResponse {
    pub raw: u32,
}

impl IfCondResponse {
    pub fn from_raw(raw: u32) -> Self {
        Self { raw }
    }

    /// Supported voltage (bits 11:8)
    pub fn voltage(&self) -> u8 {
        ((self.raw >> 8) & 0xF) as u8
    }

    /// Echo-back check pattern (bits 7:0)
    pub fn check_pattern(&self) -> u8 {
        (self.raw & 0xFF) as u8
    }

    /// Verify response matches expected voltage and pattern
    pub fn verify(&self, voltage: u8, pattern: u8) -> bool {
        self.voltage() == voltage && self.check_pattern() == pattern
    }
}

/// CSD register (CMD9 response, raw 16 bytes MSB-first as delivered by both
/// SPI and SDIO transports).
#[derive(Debug, Clone, Copy)]
pub struct CsdResponse {
    pub raw: [u8; 16],
}

impl CsdResponse {
    pub fn from_raw(raw: [u8; 16]) -> Self {
        Self { raw }
    }

    /// CSD structure version: 0 = v1 (SDSC), 1 = v2 (SDHC/SDXC), 2 = v3 (SDUC)
    pub fn version(&self) -> u8 {
        (self.raw[0] >> 6) & 0x03
    }

    /// User-data capacity in 512-byte blocks.
    ///
    /// Returns `None` for unknown / unsupported CSD structures (e.g. SDUC v3,
    /// which encodes a 28-bit C_SIZE that does not fit the v2 formula).
    pub fn capacity_blocks(&self) -> Option<u64> {
        match self.version() {
            0 => Some(self.csd_v1_capacity_blocks()),
            1 => Some(self.csd_v2_capacity_blocks()),
            _ => None,
        }
    }

    fn csd_v1_capacity_blocks(&self) -> u64 {
        // CSD v1 fields (bit numbering as in SD spec, MSB = bit 127):
        //   READ_BL_LEN [83:80]   — log2 of read block length
        //   C_SIZE      [73:62]   — 12-bit
        //   C_SIZE_MULT [49:47]   — 3-bit
        // capacity_bytes = (C_SIZE + 1) * 2^(C_SIZE_MULT + 2) * 2^READ_BL_LEN
        let read_bl_len = (self.raw[5] & 0x0F) as u32;
        let c_size = (((self.raw[6] & 0x03) as u32) << 10)
            | ((self.raw[7] as u32) << 2)
            | ((self.raw[8] as u32) >> 6);
        let c_size_mult = (((self.raw[9] & 0x03) as u32) << 1) | ((self.raw[10] as u32) >> 7);
        let mult = 1u64 << (c_size_mult + 2);
        let block_len = 1u64 << read_bl_len;
        let bytes = (c_size as u64 + 1) * mult * block_len;
        bytes / 512
    }

    fn csd_v2_capacity_blocks(&self) -> u64 {
        // CSD v2 (SDHC/SDXC):
        //   C_SIZE [69:48] — 22-bit
        //   capacity_bytes = (C_SIZE + 1) * 512 KiB
        //   capacity_blocks = (C_SIZE + 1) * 1024
        let c_size = (((self.raw[7] & 0x3F) as u32) << 16)
            | ((self.raw[8] as u32) << 8)
            | (self.raw[9] as u32);
        (c_size as u64 + 1) * 1024
    }
}

/// CID register (CMD2/CMD10 response). Identifies the card's manufacturer,
/// product, serial number, and manufacturing date.
///
/// Field layout follows SD Physical Layer spec section 5.2; only SD cards are
/// decoded here. eMMC uses a different field layout and is not supported.
#[derive(Debug, Clone, Copy)]
pub struct CidResponse {
    pub raw: [u8; 16],
}

impl CidResponse {
    pub fn from_raw(raw: [u8; 16]) -> Self {
        Self { raw }
    }

    /// Manufacturer ID (MID) — 8-bit code assigned by the SD Association.
    pub fn manufacturer_id(&self) -> u8 {
        self.raw[0]
    }

    /// OEM/Application ID (OID) — two ASCII characters identifying the card
    /// OEM. Returned as a `[u8; 2]`; bytes outside printable ASCII are
    /// preserved verbatim so callers can detect non-conforming firmware.
    pub fn oem_id(&self) -> [u8; 2] {
        [self.raw[1], self.raw[2]]
    }

    /// Product name (PNM) — 5 ASCII characters.
    pub fn product_name(&self) -> [u8; 5] {
        [
            self.raw[3],
            self.raw[4],
            self.raw[5],
            self.raw[6],
            self.raw[7],
        ]
    }

    /// Product revision (PRV) as a `(major, minor)` pair, both 4-bit BCD.
    pub fn product_revision(&self) -> (u8, u8) {
        (self.raw[8] >> 4, self.raw[8] & 0x0F)
    }

    /// Product serial number (PSN) — 32-bit big-endian.
    pub fn serial_number(&self) -> u32 {
        u32::from_be_bytes([self.raw[9], self.raw[10], self.raw[11], self.raw[12]])
    }

    /// Manufacturing date as `(year, month)` where year is the absolute
    /// 4-digit year (SD spec offsets year by 2000).
    ///
    /// Layout: bits 19:8 of bytes 13..=14 hold the date — 12 bits split as
    /// year (8 bits) and month (4 bits).
    pub fn manufacture_date(&self) -> (u16, u8) {
        let year = ((self.raw[13] & 0x0F) << 4) | (self.raw[14] >> 4);
        let month = self.raw[14] & 0x0F;
        (2000 + year as u16, month)
    }
}
