pub use sdio_host2::{RawResponse, ResponseType};

use crate::error::{CardError, Error, ErrorContext, Phase};

/// Parsed response from the card
///
/// Marked `#[non_exhaustive]`: new response shapes (e.g. SDIO IO_RW
/// extensions) may be added before 1.0.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum Response {
    /// No response phase — emitted when the command's [`ResponseType`] is
    /// [`ResponseType::None`] (e.g. CMD0). Renamed from `Response::None` to
    /// avoid lexical confusion with [`ResponseType::None`]; the two now read
    /// at a glance as "no response type configured" vs "no response decoded".
    Empty,
    R1(R1Response),
    R1b(R1Response),
    R2([u8; 16]),
    R3(OcrResponse),
    R4(SdioOcrResponse),
    R5(SdioRwResponse),
    R6(RcaResponse),
    R7(IfCondResponse),
}

impl Response {
    /// Convert a typed protocol response into the normalized physical response
    /// words used by `sdio-host2`.
    pub fn to_raw_response(self, expected: ResponseType) -> RawResponse {
        let mut words = [0; 4];
        match self {
            Self::Empty => {}
            Self::R1(resp) | Self::R1b(resp) => words[0] = resp.raw,
            Self::R2(bytes) => {
                for (word, chunk) in words.iter_mut().zip(bytes.as_chunks::<4>().0) {
                    *word = u32::from_be_bytes(*chunk);
                }
            }
            Self::R3(resp) => words[0] = resp.raw,
            Self::R4(resp) => words[0] = resp.raw,
            Self::R5(resp) => words[0] = resp.raw,
            Self::R6(resp) => words[0] = resp.raw,
            Self::R7(resp) => words[0] = resp.raw,
        }
        RawResponse::new(expected, words)
    }
}

/// Parse normalized physical response words into the protocol response type.
pub fn response_from_raw(raw: RawResponse) -> Result<Response, Error> {
    Ok(match raw.ty {
        ResponseType::None => Response::Empty,
        ResponseType::R1 => Response::R1(R1Response::from_native_raw(raw.words[0])?),
        ResponseType::R1b => Response::R1b(R1Response::from_native_raw(raw.words[0])?),
        ResponseType::R2 => {
            let mut bytes = [0; 16];
            for (chunk, word) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(raw.words) {
                chunk.copy_from_slice(&word.to_be_bytes());
            }
            Response::R2(bytes)
        }
        ResponseType::R3 => Response::R3(OcrResponse::from_raw(raw.words[0])),
        ResponseType::R4 => Response::R4(SdioOcrResponse::from_raw(raw.words[0])),
        ResponseType::R5 => Response::R5(SdioRwResponse::from_raw(raw.words[0])),
        ResponseType::R6 => Response::R6(RcaResponse::from_raw(raw.words[0])),
        ResponseType::R7 => Response::R7(IfCondResponse::from_raw(raw.words[0])),
        _ => return Err(Error::UnsupportedCommand),
    })
}

/// R1: Standard response — contains status bits
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct R1Response {
    pub raw: u32,
}

impl R1Response {
    /// Parse a native (SDIO/SDHCI) 32-bit R1 response.
    ///
    /// SD Physical Layer spec section 4.10.1 (Table 4-42) places the card
    /// status error flags at bits 19..=31:
    ///
    /// | Bit | Name                |
    /// |-----|---------------------|
    /// | 31  | `OUT_OF_RANGE`      |
    /// | 30  | `ADDRESS_ERROR`     |
    /// | 29  | `BLOCK_LEN_ERROR`   |
    /// | 28  | `ERASE_SEQ_ERROR`   |
    /// | 27  | `ERASE_PARAM`       |
    /// | 26  | `WP_VIOLATION`      |
    /// | 25  | `CARD_IS_LOCKED`    |
    /// | 24  | `LOCK_UNLOCK_FAILED`|
    /// | 23  | `COM_CRC_ERROR`     |
    /// | 22  | `ILLEGAL_COMMAND`   |
    /// | 21  | `CARD_ECC_FAILED`   |
    /// | 20  | `CC_ERROR`          |
    /// | 19  | `ERROR`             |
    ///
    /// If **any** of those 13 bits is set this returns
    /// `Err(Error::CardError(..))`. Otherwise the raw value is preserved so
    /// callers can inspect informational state bits (`current_state`,
    /// `ready_for_data`, ...).
    ///
    /// Note: earlier versions only looked at bits 19..=24 and silently
    /// dropped `OUT_OF_RANGE`, `ADDRESS_ERROR`, `BLOCK_LEN_ERROR`,
    /// `ERASE_PARAM`, `WP_VIOLATION`, `CARD_IS_LOCKED`, and `COM_CRC_ERROR`.
    /// Callers that used to see `Ok` for one of those now correctly see
    /// `Err(CardError::..)`.
    pub fn from_native_raw(raw: u32) -> Result<Self, Error> {
        let err_bits = raw & R1_NATIVE_ERROR_MASK;
        if err_bits != 0 {
            return Err(Error::CardError(decode_native_card_error(err_bits)));
        }
        Ok(Self { raw })
    }

    /// Parse a single-byte SPI R1 response.
    ///
    /// SPI R1 has a fixed `0` start bit (the high bit must be clear). The
    /// remaining bits encode informational state (idle, erase reset) and
    /// soft error flags (illegal command, CRC error, ...). Because some flags
    /// — especially `illegal_command` — are *expected* during initialization
    /// (e.g. CMD8 on SD v1 cards), this function does NOT itself convert
    /// flag bits into `Err`. Callers should inspect the helpers
    /// ([`R1Response::illegal_command`] etc.) to decide what to do.
    ///
    /// Returns `Err(Error::BadResponse(_))` when the high bit is set, which
    /// indicates a malformed response or that no R1 byte arrived.
    pub fn from_spi_byte(byte: u8) -> Result<Self, Error> {
        if byte & 0x80 != 0 {
            return Err(Error::BadResponse(ErrorContext::new(Phase::ResponseWait)));
        }
        Ok(Self { raw: byte as u32 })
    }

    /// Decode error flag bits in a SPI R1 response into a [`CardError`].
    ///
    /// Returns `None` when no error bits are set. Only meaningful for values
    /// produced by [`R1Response::from_spi_byte`]; native R1 layouts use a
    /// different bit mapping and report errors directly through
    /// [`R1Response::from_native_raw`].
    pub fn spi_card_error(&self) -> Option<CardError> {
        let bits = (self.raw as u8) & 0b0111_1110;
        if bits == 0 {
            None
        } else {
            Some(decode_spi_card_error(bits))
        }
    }

    /// Card is in idle state
    pub fn idle(&self) -> bool {
        self.raw & (1 << 0) != 0
    }

    /// Erase reset
    pub fn erase_reset(&self) -> bool {
        self.raw & (1 << 1) != 0
    }

    /// Illegal command
    pub fn illegal_command(&self) -> bool {
        self.raw & (1 << 2) != 0
    }

    /// Command CRC failed
    pub fn command_crc_failed(&self) -> bool {
        self.raw & (1 << 3) != 0
    }

    /// Current state of the card state machine (bits 12:15).
    ///
    /// Only meaningful for native (SDIO) R1 responses; SPI R1 bytes do not
    /// encode card state.
    pub fn current_state(&self) -> CardState {
        match ((self.raw >> 9) & 0xF) as u8 {
            0 => CardState::Idle,
            1 => CardState::Ready,
            2 => CardState::Identification,
            3 => CardState::Standby,
            4 => CardState::Transfer,
            5 => CardState::SendingData,
            6 => CardState::ReceiveData,
            7 => CardState::Programming,
            8 => CardState::Disconnect,
            other => CardState::Reserved(other),
        }
    }

    /// Card is locked (native R1 only)
    pub fn card_is_locked(&self) -> bool {
        self.raw & (1 << 19) != 0
    }

    /// `READY_FOR_DATA` (bit 8): card buffer is empty and the next data
    /// transfer can be issued. Used after R1b commands (CMD7, CMD12,
    /// MMC CMD6 SWITCH) to know when the busy line has cleared.
    ///
    /// Only meaningful for native (SDIO) R1 responses.
    pub fn ready_for_data(&self) -> bool {
        self.raw & (1 << 8) != 0
    }

    /// `SWITCH_ERROR` (bit 7): the previous MMC CMD6 SWITCH was rejected
    /// (e.g. invalid EXT_CSD field, value out of range). Surfaces here
    /// because CMD6 itself returns R1b with this bit, but most error
    /// reporters hide bits 0..15.
    pub fn switch_error(&self) -> bool {
        self.raw & (1 << 7) != 0
    }
}

/// Card state machine states
///
/// Marked `#[non_exhaustive]`: SD/MMC specs may carve new state values out of
/// the reserved range, and downstream match sites must keep a `_ => ...` arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CardState {
    Idle,
    Ready,
    Identification,
    Standby,
    Transfer,
    SendingData,
    ReceiveData,
    Programming,
    Disconnect,
    Reserved(u8),
}

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
}

/// Bitmask covering every native R1 error flag (bits 19..=31 of the 32-bit
/// response, per SD spec section 4.10.1). `from_native_raw` ANDs the raw
/// response against this and routes any non-zero result through
/// `decode_native_card_error`.
const R1_NATIVE_ERROR_MASK: u32 = 0xFFF8_0000;

const R1_BIT_OUT_OF_RANGE: u32 = 1 << 31;
const R1_BIT_ADDRESS_ERROR: u32 = 1 << 30;
const R1_BIT_BLOCK_LEN_ERROR: u32 = 1 << 29;
const R1_BIT_ERASE_SEQ_ERROR: u32 = 1 << 28;
const R1_BIT_ERASE_PARAM: u32 = 1 << 27;
const R1_BIT_WP_VIOLATION: u32 = 1 << 26;
const R1_BIT_CARD_IS_LOCKED: u32 = 1 << 25;
const R1_BIT_LOCK_UNLOCK_FAILED: u32 = 1 << 24;
const R1_BIT_COM_CRC_ERROR: u32 = 1 << 23;
const R1_BIT_ILLEGAL_COMMAND: u32 = 1 << 22;
const R1_BIT_CARD_ECC_FAILED: u32 = 1 << 21;
const R1_BIT_CC_ERROR: u32 = 1 << 20;
const R1_BIT_ERROR: u32 = 1 << 19;

/// Decode SPI R1 byte error bits (bits 1..=6 of the byte).
///
/// SPI R1 layout (SD spec, simplified):
///   bit 1 = erase reset
///   bit 2 = illegal command
///   bit 3 = command CRC error
///   bit 4 = erase sequence error
///   bit 5 = address error
///   bit 6 = parameter error
///
/// When multiple bits are set we return the first known error in priority
/// order (CRC > illegal command > address > parameter > erase sequence >
/// erase reset). If no known bit is set we preserve the raw pattern.
fn decode_spi_card_error(bits: u8) -> CardError {
    if bits & 0b0000_1000 != 0 {
        CardError::CommandCrcFailed
    } else if bits & 0b0000_0100 != 0 {
        CardError::IllegalCommand
    } else if bits & 0b0010_0000 != 0 {
        CardError::AddressError
    } else if bits & 0b0100_0000 != 0 {
        // SPI PARAMETER_ERROR maps to native BLOCK_LEN_ERROR/parameter family.
        CardError::BlockLenError
    } else if bits & (0b0001_0000 | 0b0000_0010) != 0 {
        // ERASE_SEQ_ERROR or ERASE_RESET — both fall under EraseSequence.
        CardError::EraseSequence
    } else {
        CardError::Unknown(bits as u32)
    }
}

/// Decode the native R1 error bits (bits 19..=31 of the 32-bit response).
///
/// Caller passes `raw & R1_NATIVE_ERROR_MASK` (non-zero). When multiple bits
/// are set we surface the most-severe-first variant per SD spec convention:
/// argument/addressing errors first (so a write to an invalid LBA is reported
/// as `OutOfRange` even if the card also raises lower-priority companions),
/// then bus-integrity errors, then card-state errors, then catch-all
/// erase/generic. Unknown patterns preserve the raw 13-bit error nibble
/// (shifted to bit 0) so callers can log the exact bits.
fn decode_native_card_error(err_bits: u32) -> CardError {
    if err_bits & R1_BIT_OUT_OF_RANGE != 0 {
        CardError::OutOfRange
    } else if err_bits & R1_BIT_ADDRESS_ERROR != 0 {
        CardError::AddressError
    } else if err_bits & R1_BIT_BLOCK_LEN_ERROR != 0 {
        CardError::BlockLenError
    } else if err_bits & R1_BIT_WP_VIOLATION != 0 {
        CardError::WriteProtect
    } else if err_bits & R1_BIT_COM_CRC_ERROR != 0 {
        CardError::CommandCrcFailed
    } else if err_bits & R1_BIT_ILLEGAL_COMMAND != 0 {
        CardError::IllegalCommand
    } else if err_bits & R1_BIT_CARD_ECC_FAILED != 0 {
        CardError::CardEccFailed
    } else if err_bits & R1_BIT_CC_ERROR != 0 {
        CardError::ControllerError
    } else if err_bits & R1_BIT_LOCK_UNLOCK_FAILED != 0 {
        CardError::LockUnlockFailed
    } else if err_bits & R1_BIT_CARD_IS_LOCKED != 0 {
        CardError::CardIsLocked
    } else if err_bits & (R1_BIT_ERASE_SEQ_ERROR | R1_BIT_ERASE_PARAM) != 0 {
        CardError::EraseSequence
    } else if err_bits & R1_BIT_ERROR != 0 {
        CardError::GenericError
    } else {
        CardError::Unknown(err_bits >> 19)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spi_r1_idle_uses_bit_zero() {
        let response = R1Response::from_spi_byte(0x01).unwrap();
        assert!(response.idle());
        assert!(!response.illegal_command());
        assert!(response.spi_card_error().is_none());
    }

    #[test]
    fn spi_r1_illegal_command_sets_flag_and_card_error() {
        let response = R1Response::from_spi_byte(0x04).unwrap();
        assert!(response.illegal_command());
        assert_eq!(response.spi_card_error(), Some(CardError::IllegalCommand));
    }

    #[test]
    fn spi_r1_idle_plus_illegal_command_preserves_both() {
        let response = R1Response::from_spi_byte(0x05).unwrap();
        assert!(response.idle());
        assert!(response.illegal_command());
        assert_eq!(response.spi_card_error(), Some(CardError::IllegalCommand));
    }

    #[test]
    fn spi_r1_high_bit_is_bus_error() {
        assert!(matches!(
            R1Response::from_spi_byte(0x80),
            Err(Error::BadResponse(_))
        ));
        assert!(matches!(
            R1Response::from_spi_byte(0xFF),
            Err(Error::BadResponse(_))
        ));
    }

    #[test]
    fn native_r1_status_bits_decoded() {
        // status = card in transfer state (bits 12..=9 = 4)
        let r1 = R1Response::from_native_raw(4 << 9).unwrap();
        assert_eq!(r1.current_state(), CardState::Transfer);
    }

    #[test]
    fn native_r1_with_illegal_command_returns_error() {
        // illegal command = bit 22 in native R1
        let err = R1Response::from_native_raw(1 << 22).unwrap_err();
        assert_eq!(err, Error::CardError(CardError::IllegalCommand));
    }

    /// Regression: bits 25..=31 used to be silently dropped because
    /// `from_native_raw` only masked bits 19..=24. A write to an LBA past
    /// the end of the card raises `OUT_OF_RANGE` (bit 31) and used to be
    /// reported as `Ok`. After the mask widening it must surface as an
    /// `Err(CardError::OutOfRange)`.
    #[test]
    fn native_r1_out_of_range_was_previously_dropped() {
        let err = R1Response::from_native_raw(1 << 31).unwrap_err();
        assert_eq!(err, Error::CardError(CardError::OutOfRange));
    }

    #[test]
    fn native_r1_decodes_each_priority_class() {
        let cases = [
            (1u32 << 31, CardError::OutOfRange),
            (1 << 30, CardError::AddressError),
            (1 << 29, CardError::BlockLenError),
            (1 << 26, CardError::WriteProtect),
            (1 << 25, CardError::CardIsLocked),
            (1 << 24, CardError::LockUnlockFailed),
            (1 << 23, CardError::CommandCrcFailed),
            (1 << 22, CardError::IllegalCommand),
            (1 << 21, CardError::CardEccFailed),
            (1 << 20, CardError::ControllerError),
            (1 << 19, CardError::GenericError),
            (1 << 28, CardError::EraseSequence),
            (1 << 27, CardError::EraseSequence),
        ];
        for (raw, expected) in cases {
            let err = R1Response::from_native_raw(raw).unwrap_err();
            assert_eq!(err, Error::CardError(expected), "raw={raw:#010x}");
        }
    }

    /// OUT_OF_RANGE outranks WP_VIOLATION when the card sets both — exercises
    /// the priority ordering in `decode_native_card_error`.
    #[test]
    fn native_r1_priority_picks_argument_errors_first() {
        let err = R1Response::from_native_raw((1 << 31) | (1 << 26)).unwrap_err();
        assert_eq!(err, Error::CardError(CardError::OutOfRange));
    }

    /// Informational status bits (bit 8 READY_FOR_DATA, current_state nibble)
    /// must not be treated as errors. Regression guard against accidentally
    /// extending the mask too far.
    #[test]
    fn native_r1_status_only_response_is_ok() {
        let raw = (1u32 << 8) | (4u32 << 9); // READY_FOR_DATA + Transfer state
        let r1 = R1Response::from_native_raw(raw).unwrap();
        assert!(r1.ready_for_data());
        assert_eq!(r1.current_state(), CardState::Transfer);
    }

    #[test]
    fn decode_spi_card_error_priority_handles_multiple_bits() {
        // Both illegal command (0x04) + crc failed (0x08) bits set. CRC wins.
        assert_eq!(
            decode_spi_card_error(0b0000_1100),
            CardError::CommandCrcFailed
        );
    }

    #[test]
    fn decode_spi_card_error_unknown_for_unrecognized_bits() {
        // bit 7 cannot occur after our mask; this exercises the fallback.
        assert_eq!(decode_spi_card_error(0b0000_0000), CardError::Unknown(0));
    }

    #[test]
    fn csd_v2_decodes_2gib_capacity() {
        // CSD v2 with C_SIZE = 0x000F0F (3855) ⇒ (3855 + 1) * 1024 blocks
        // = 3,948,544 blocks ≈ 1.88 GiB. Layout: byte 0 high bits = 0x40
        // (CSD_STRUCTURE = 1), byte 7 low 6 bits + byte 8 + byte 9 = C_SIZE.
        let mut raw = [0u8; 16];
        raw[0] = 0x40;
        raw[7] = 0x00;
        raw[8] = 0x0F;
        raw[9] = 0x0F;
        let csd = CsdResponse::from_raw(raw);
        assert_eq!(csd.version(), 1);
        assert_eq!(csd.capacity_blocks(), Some((0x0F0F + 1) * 1024));
    }

    #[test]
    fn csd_v1_decodes_known_capacity() {
        // CSD v1 example: READ_BL_LEN = 9, C_SIZE = 0x0EFF, C_SIZE_MULT = 7
        // ⇒ blocks = (0x0EFF+1) * 2^(7+2) * 2^9 / 512
        //          = 3840 * 512 * 512 / 512 = 3840 * 512 = 1,966,080 blocks
        let mut raw = [0u8; 16];
        raw[0] = 0x00; // CSD v1
        raw[5] = 0x09; // low nibble = READ_BL_LEN = 9
        // C_SIZE = 0x0EFF stored across bytes 6 (low 2 bits) | 7 | 8 (high 2 bits)
        // 0x0EFF = 0b0000_1110_1111_1111
        // bits 11:10 = 00 → byte6 low 2 = 0
        // bits 9:2  = 0b0011_1011 = 0x3B → byte7 = 0x3B
        // bits 1:0  = 0b11 → byte8 high 2 = 0b11_xx_xxxx
        raw[6] = 0b0000_0011; // low 2 bits = top 2 of C_SIZE = 11 → wait, recompute
        // Actually: C_SIZE bits 11:10 → byte6[1:0]; bits 9:2 → byte7[7:0]; bits 1:0 → byte8[7:6]
        // For C_SIZE = 0x0EFF = 0b1110_1111_1111:
        //   bits 11:10 = 11
        //   bits 9:2  = 0b1011_1111 = 0xBF
        //   bits 1:0  = 0b11
        raw[6] = 0b0000_0011;
        raw[7] = 0xBF;
        raw[8] = 0b1100_0000;
        // C_SIZE_MULT = 7 = 0b111 stored in byte9[1:0] (top 2 bits of MULT)
        // and byte10[7] (low bit of MULT)
        raw[9] = 0b0000_0011;
        raw[10] = 0b1000_0000;
        let csd = CsdResponse::from_raw(raw);
        assert_eq!(csd.version(), 0);
        let expected = (0x0EFFu64 + 1) * (1 << (7 + 2)) * (1 << 9) / 512;
        assert_eq!(csd.capacity_blocks(), Some(expected));
    }

    #[test]
    fn csd_unknown_version_returns_none() {
        let mut raw = [0u8; 16];
        raw[0] = 0x80; // CSD_STRUCTURE = 2 (SDUC v3) — not yet supported
        let csd = CsdResponse::from_raw(raw);
        assert_eq!(csd.version(), 2);
        assert_eq!(csd.capacity_blocks(), None);
    }

    #[test]
    fn cid_decodes_manufacturer_oem_product_serial_and_date() {
        // Hand-rolled CID: MID=0x03, OID="SD", PNM="ABC12", PRV=2.7,
        //   PSN=0xDEAD_BEEF, MDT year=2026 (offset 26 = 0x1A) month=5.
        let mut raw = [0u8; 16];
        raw[0] = 0x03;
        raw[1] = b'S';
        raw[2] = b'D';
        raw[3] = b'A';
        raw[4] = b'B';
        raw[5] = b'C';
        raw[6] = b'1';
        raw[7] = b'2';
        raw[8] = (2 << 4) | 7;
        raw[9] = 0xDE;
        raw[10] = 0xAD;
        raw[11] = 0xBE;
        raw[12] = 0xEF;
        // MDT bits 19:8 = year[7:0] (8 bits) + month[3:0] (4 bits)
        // year = 0x1A = 0001 1010: high nibble in raw[13][3:0], low nibble in raw[14][7:4]
        raw[13] = 0x01; // year high nibble = 1
        raw[14] = 0xA5; // year low nibble = A, month nibble = 5

        let cid = CidResponse::from_raw(raw);
        assert_eq!(cid.manufacturer_id(), 0x03);
        assert_eq!(&cid.oem_id(), b"SD");
        assert_eq!(&cid.product_name(), b"ABC12");
        assert_eq!(cid.product_revision(), (2, 7));
        assert_eq!(cid.serial_number(), 0xDEAD_BEEF);
        assert_eq!(cid.manufacture_date(), (2026, 5));
    }

    #[test]
    fn switch_status_reports_high_speed_when_group_one_function_one() {
        let mut raw = [0u8; 64];
        raw[16] = 0x01; // group 2 = 0, group 1 = 1 (high speed)
        let status = SwitchStatus::from_raw(raw);
        assert_eq!(status.selected_function(1), 1);
        assert!(status.high_speed_active());
    }

    #[test]
    fn switch_status_reports_access_mode_support_bits() {
        let mut raw = [0u8; 64];
        raw[13] = (1 << 1) | (1 << 3);
        let status = SwitchStatus::from_raw(raw);

        assert!(status.access_mode_supported(1));
        assert!(status.access_mode_supported(3));
        assert!(!status.access_mode_supported(2));
        assert!(!status.access_mode_supported(8));
    }

    #[test]
    fn switch_status_reports_default_when_group_one_function_zero() {
        let raw = [0u8; 64];
        let status = SwitchStatus::from_raw(raw);
        assert_eq!(status.selected_function(1), 0);
        assert!(!status.high_speed_active());
    }

    #[test]
    fn switch_status_unsupported_group_returns_0xf() {
        let mut raw = [0u8; 64];
        raw[16] = 0xF0; // group 2 unsupported, group 1 = 0
        let status = SwitchStatus::from_raw(raw);
        assert_eq!(status.selected_function(2), 0xF);
        assert_eq!(status.selected_function(7), 0xF); // out of range
    }
}
