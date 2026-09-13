/// 64-byte SD function-switch status, returned in the data phase of CMD6.
///
/// See SD Physical Layer spec section 4.3.10 (Switch Function). Field
/// numbering uses the spec's bit-435..=0 convention but accessors here are
/// expressed in byte offsets within `raw[0..64]` for clarity.
#[derive(Debug, Clone, Copy)]
pub struct SwitchStatus {
    pub raw: [u8; 64],
}

impl SwitchStatus {
    pub fn from_raw(raw: [u8; 64]) -> Self {
        Self { raw }
    }

    /// Selected function for `group` (1-based, 1..=6) after a switch
    /// operation. `0xF` means the group is not supported by the card.
    ///
    /// Group 1 selection lives in the low nibble of byte 16; group 2 in the
    /// high nibble of the same byte; group 3 in the low nibble of byte 15;
    /// and so on, paired big-endian over bytes 14..=16.
    pub fn selected_function(&self, group: u8) -> u8 {
        match group {
            1 => self.raw[16] & 0x0F,
            2 => self.raw[16] >> 4,
            3 => self.raw[15] & 0x0F,
            4 => self.raw[15] >> 4,
            5 => self.raw[14] & 0x0F,
            6 => self.raw[14] >> 4,
            _ => 0xF,
        }
    }

    /// Returns true iff group 1 reports high-speed (function 1) selected.
    pub fn high_speed_active(&self) -> bool {
        self.selected_function(1) == 1
    }

    /// Returns true iff SD access-mode group 1 advertises `function`.
    ///
    /// The support bitmap for group 1 is carried in byte 13 in the 64-byte
    /// switch status block; bit `n` means function `n` is selectable.
    pub fn access_mode_supported(&self, function: u8) -> bool {
        function < 8 && (self.raw[13] & (1 << function)) != 0
    }
}

/// SDIO OCR (R4/CMD5 response)
#[derive(Debug, Clone, Copy)]
pub struct SdioOcrResponse {
    pub raw: u32,
}

impl SdioOcrResponse {
    pub fn from_raw(raw: u32) -> Self {
        Self { raw }
    }

    /// Number of I/O functions (bits 27:28)
    pub fn io_functions(&self) -> u8 {
        ((self.raw >> 28) & 0x7) as u8
    }

    /// Memory present
    pub fn memory_present(&self) -> bool {
        self.raw & (1 << 27) != 0
    }

    /// I/O ready
    pub fn io_ready(&self) -> bool {
        self.raw & (1 << 31) != 0
    }

    /// Card-supported voltage window carried in OCR bits 23:0.
    pub fn voltage_window(&self) -> u32 {
        self.raw & 0x00ff_ffff
    }
}

/// SDIO R5 response
#[derive(Debug, Clone, Copy)]
pub struct SdioRwResponse {
    pub raw: u32,
}

impl SdioRwResponse {
    pub fn from_raw(raw: u32) -> Self {
        Self { raw }
    }

    /// Read/write data (bits 7:0)
    pub fn data(&self) -> u8 {
        (self.raw & 0xFF) as u8
    }

    /// Response flags (bits 15:8)
    pub fn flags(&self) -> u8 {
        ((self.raw >> 8) & 0xFF) as u8
    }

    /// Validate the R5 status flags before exposing the returned byte.
    pub fn checked_data(&self) -> Result<u8, crate::error::Error> {
        const COM_CRC_ERROR: u8 = 1 << 7;
        const ILLEGAL_COMMAND: u8 = 1 << 6;
        const GENERAL_ERROR: u8 = 1 << 3;
        const INVALID_FUNCTION: u8 = 1 << 1;
        const OUT_OF_RANGE: u8 = 1;

        let flags = self.flags();
        if flags & COM_CRC_ERROR != 0 {
            return Err(crate::error::Error::Crc(
                crate::error::ErrorContext::for_cmd(crate::error::Phase::ResponseWait, 52),
            ));
        }
        if flags & ILLEGAL_COMMAND != 0 {
            return Err(crate::error::Error::UnsupportedCommand);
        }
        if flags & (GENERAL_ERROR | INVALID_FUNCTION | OUT_OF_RANGE) != 0 {
            return Err(crate::error::Error::BadResponse(
                crate::error::ErrorContext::for_cmd(crate::error::Phase::ResponseWait, 52),
            ));
        }
        Ok(self.data())
    }
}
