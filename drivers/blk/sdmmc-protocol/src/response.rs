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

mod card;
pub use card::{CardState, R1Response};

mod identity;
pub use identity::{CidResponse, CsdResponse, IfCondResponse, OcrResponse, RcaResponse};

mod switch;
pub use switch::{SdioOcrResponse, SdioRwResponse, SwitchStatus};

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
