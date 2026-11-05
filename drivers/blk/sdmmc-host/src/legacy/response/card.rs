use super::*;

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
