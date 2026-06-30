pub use sdio_host2::Command;

use crate::response::ResponseType;

/// Direction of the data phase that follows a command, if any.
///
/// Marked `#[non_exhaustive]`: bidirectional / control-stream variants may be
/// added before 1.0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DataDirection {
    /// No data phase follows this command.
    None,
    /// The host reads data from the card after the command response.
    Read,
    /// The host writes data to the card after the command response.
    Write,
}

impl DataDirection {
    /// Returns true if this command has no data phase.
    pub const fn is_none(self) -> bool {
        matches!(self, DataDirection::None)
    }
}

// ── Standard SD/MMC Commands ─────────────────────────────────────────

// ── Broadcast commands (bc: no response, bcr: response) ──

/// CMD0: GO_IDLE_STATE — Reset all cards to idle
pub const CMD0: Command = Command::new(0, 0, ResponseType::None);

/// CMD2: ALL_SEND_CID — Request CID from all cards
pub const CMD2: Command = Command::new(2, 0, ResponseType::R2);

/// CMD3: SEND_RELATIVE_ADDR (SD) or SET_RELATIVE_ADDR (MMC)
pub const CMD3_SD: Command = Command::new(3, 0, ResponseType::R6);
/// CMD3 MMC variant: arg contains the desired RCA
pub fn cmd3_mmc(rca: u16) -> Command {
    Command::new(3, (rca as u32) << 16, ResponseType::R1)
}

/// CMD4: SET_DSR — Program driver stage register
pub fn cmd4(dsr: u16) -> Command {
    Command::new(4, (dsr as u32) << 16, ResponseType::None)
}

/// CMD6: SWITCH_FUNC — Switch card function
pub fn cmd6(arg: u32) -> Command {
    Command::new(6, arg, ResponseType::R1)
}

/// CMD6 helper: switch function group 1 to high speed (50 MHz, function 1).
///
/// The card responds with R1 followed by a 64-byte status data block. Use
/// `mode=true` to actually switch; `mode=false` to query support without
/// changing the configuration.
pub fn cmd6_high_speed(switch: bool) -> Command {
    cmd6_sd_access_mode(switch, 1)
}

/// CMD6 helper: select SD access mode function in group 1.
///
/// Function numbers follow the SD Physical Layer access-mode group:
/// 0 = default/SDR12, 1 = high-speed/SDR25, 2 = SDR50,
/// 3 = SDR104, 4 = DDR50. Groups 6..2 are set to "no change".
pub fn cmd6_sd_access_mode(switch: bool, function: u8) -> Command {
    let mode = if switch { 1u32 << 31 } else { 0 };
    // groups 6..2 are 0xF (no change), group 1 selects access mode.
    let groups = 0x00FF_FFF0u32 | u32::from(function & 0xF);
    Command::new(6, mode | groups, ResponseType::R1)
}

/// CMD7: SELECT/DESELECT CARD
pub fn cmd7(rca: u16) -> Command {
    Command::new(7, (rca as u32) << 16, ResponseType::R1b)
}

/// CMD8: SEND_IF_COND — Send interface condition (SD)
pub fn cmd8(voltage: u8, check_pattern: u8) -> Command {
    let arg = ((voltage as u32) << 8) | check_pattern as u32;
    Command::new(8, arg, ResponseType::R7)
}

/// CMD9: SEND_CSD — Get CSD register
pub fn cmd9(rca: u16) -> Command {
    Command::new(9, (rca as u32) << 16, ResponseType::R2)
}

/// CMD10: SEND_CID — Get CID register
pub fn cmd10(rca: u16) -> Command {
    Command::new(10, (rca as u32) << 16, ResponseType::R2)
}

/// CMD11: VOLTAGE_SWITCH — switch the bus to 1.8 V signaling.
///
/// SD 3.0 / UHS-I cards and eMMC HS200 share this command. The card
/// responds with R1; the actual voltage transition is then driven by
/// the host controller (gate SD clock → switch IO domain → wait t_VSW
/// → re-enable clock). Implementations live in the host layer.
pub const CMD11: Command = Command::new(11, 0, ResponseType::R1);

/// CMD12: STOP_TRANSMISSION — Stop read/write
pub const CMD12: Command = Command::new(12, 0, ResponseType::R1b);

/// CMD13: SEND_STATUS
pub fn cmd13(rca: u16) -> Command {
    Command::new(13, (rca as u32) << 16, ResponseType::R1)
}

/// CMD16: SET_BLOCKLEN
pub fn cmd16(block_len: u32) -> Command {
    Command::new(16, block_len, ResponseType::R1)
}

/// CMD17: READ_SINGLE_BLOCK
pub fn cmd17(addr: u32) -> Command {
    Command::new(17, addr, ResponseType::R1)
}

/// CMD18: READ_MULTIPLE_BLOCK
pub fn cmd18(addr: u32) -> Command {
    Command::new(18, addr, ResponseType::R1)
}

/// CMD24: WRITE_BLOCK
pub fn cmd24(addr: u32) -> Command {
    Command::new(24, addr, ResponseType::R1)
}

/// CMD25: WRITE_MULTIPLE_BLOCK
pub fn cmd25(addr: u32) -> Command {
    Command::new(25, addr, ResponseType::R1)
}

/// CMD19 (SD): SEND_TUNING_BLOCK — request a 64-byte tuning pattern.
///
/// Used by SD UHS-I (SDR50 / SDR104). Response is R1, immediately
/// followed by a 64-byte data phase the host samples to find a working
/// clock phase. Tuning is iterated up to 40 times by the host
/// controller; the protocol layer just issues this command.
pub const CMD19: Command = Command::new(19, 0, ResponseType::R1);

/// CMD21 (MMC): SEND_TUNING_BLOCK_HS200 — request the HS200 tuning
/// pattern.
///
/// 64 bytes for 4-bit bus, 128 bytes for 8-bit bus. Same role as CMD19
/// but on eMMC. Host controllers typically exercise this in a tight
/// loop while sweeping their internal sampling clock.
pub const CMD21: Command = Command::new(21, 0, ResponseType::R1);

/// Tuning block size for SD CMD19 (always 64 bytes).
pub const SD_TUNING_BLOCK_SIZE: u32 = 64;
/// Tuning block size for MMC CMD21 over a 4-bit bus.
pub const MMC_TUNING_BLOCK_SIZE_4BIT: u32 = 64;
/// Tuning block size for MMC CMD21 over an 8-bit bus.
pub const MMC_TUNING_BLOCK_SIZE_8BIT: u32 = 128;

/// CMD32: ERASE_WR_BLK_START
pub fn cmd32(addr: u32) -> Command {
    Command::new(32, addr, ResponseType::R1)
}

/// CMD33: ERASE_WR_BLK_END
pub fn cmd33(addr: u32) -> Command {
    Command::new(33, addr, ResponseType::R1)
}

/// CMD38: ERASE
pub const CMD38: Command = Command::new(38, 0, ResponseType::R1b);

/// CMD41: SD_SEND_OP_COND — Send operating condition (SD only)
pub fn cmd41(hcs: bool, voltage_window: u32) -> Command {
    cmd41_with_s18r(hcs, voltage_window, false)
}

/// CMD41 variant that can request SD 1.8 V signaling through S18R.
pub fn cmd41_with_s18r(hcs: bool, voltage_window: u32, s18r: bool) -> Command {
    let arg = if hcs { 0x4000_0000 } else { 0 }
        | if s18r { 1 << 24 } else { 0 }
        | (voltage_window & 0x00FF_F800);
    Command::new(41, arg, ResponseType::R3)
}

/// CMD55: APP_CMD — Next command is application-specific
pub fn cmd55(rca: u16) -> Command {
    Command::new(55, (rca as u32) << 16, ResponseType::R1)
}

/// CMD58: READ_OCR — Read OCR register
pub const CMD58: Command = Command::new(58, 0, ResponseType::R3);

// ── MMC specific ──

/// CMD1: SEND_OP_COND (MMC)
pub fn cmd1(voltage_window: u32) -> Command {
    Command::new(1, voltage_window, ResponseType::R3)
}

/// CMD6 (MMC): SWITCH — modify a single byte of EXT_CSD.
///
/// `access` selects how the value is applied (`0b11` = `WRITE_BYTE`,
/// `0b10` = `SET_BITS`, `0b01` = `CLEAR_BITS`). `index` is the EXT_CSD
/// byte offset (0..511). After issuing this the host must wait for the
/// busy line to clear (R1b) and then poll CMD13 to confirm the card
/// returned to `tran` and did not set `SWITCH_ERROR`.
pub fn cmd6_mmc_switch(access: u8, index: u8, value: u8) -> Command {
    let arg = ((access as u32) << 24) | ((index as u32) << 16) | ((value as u32) << 8);
    Command::new(6, arg, ResponseType::R1b)
}

/// CMD8 (MMC): SEND_EXT_CSD — read the 512-byte extended CSD register.
///
/// **Important**: this is a *different* CMD8 than the SD `SEND_IF_COND`.
/// MMC CMD8 carries a data phase (R1 followed by a 512-byte read),
/// while SD CMD8 has no data and uses R7. The protocol layer picks the
/// right one based on the card kind.
pub const CMD8_MMC: Command = Command::new(8, 0, ResponseType::R1);

/// EXT_CSD byte offsets the driver currently consumes. Full register is
/// 512 bytes; only document the ones we read.
pub mod ext_csd {
    /// Card type (HS / HS200 / HS400 support bitmap).
    pub const DEVICE_TYPE: usize = 196;
    /// Selected timing mode after CMD6 (0 = backwards compat,
    /// 1 = HS, 2 = HS200, 3 = HS400). Same byte is also written to
    /// switch modes.
    pub const HS_TIMING: usize = 185;
    /// Selected bus width (0 = 1-bit, 1 = 4-bit, 2 = 8-bit;
    /// 5 = 4-bit DDR, 6 = 8-bit DDR).
    pub const BUS_WIDTH: usize = 183;
    /// Sector count (LE u32) — authoritative capacity for ≥2 GB cards.
    pub const SEC_COUNT: usize = 212;

    pub mod device_type {
        /// Supports HS @ 26 MHz.
        pub const HS_26: u8 = 1 << 0;
        /// Supports HS @ 52 MHz.
        pub const HS_52: u8 = 1 << 1;
        /// Supports HS200 @ 200 MHz, 1.8 V.
        pub const HS200_18V: u8 = 1 << 4;
        /// Supports HS200 @ 200 MHz, 1.2 V.
        pub const HS200_12V: u8 = 1 << 5;
    }
}

// ── SDIO specific commands ──

/// CMD5: IO_SEND_OP_COND (SDIO)
pub const CMD5: Command = Command::new(5, 0, ResponseType::R4);

/// CMD52: IO_RW_DIRECT
///
/// `addr` is a 17-bit SDIO register address (bits 25:9 of the command argument).
pub fn cmd52(write: bool, function: u8, raw: bool, addr: u32, data: u8) -> Command {
    let arg = (write as u32) << 31
        | ((function as u32) & 0x7) << 28
        | (raw as u32) << 27
        | (addr & 0x1_FFFF) << 9
        | (data as u32);
    Command::new(52, arg, ResponseType::R5)
}

/// CMD53: IO_RW_EXTENDED
///
/// `addr` is a 17-bit SDIO register address (bits 25:9 of the command argument).
/// `count` is a 9-bit byte/block count (bits 8:0).
pub fn cmd53(
    write: bool,
    function: u8,
    block_mode: bool,
    addr: u32,
    op_code: bool,
    count: u16,
) -> Command {
    let arg = (write as u32) << 31
        | ((function as u32) & 0x7) << 28
        | (block_mode as u32) << 27
        | (op_code as u32) << 26
        | (addr & 0x1_FFFF) << 9
        | (count as u32 & 0x1FF);
    Command::new(53, arg, ResponseType::R5)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cmd0_crc() {
        let bytes = CMD0.to_spi_bytes();
        // CMD0 with arg=0: 0x40 0x00 0x00 0x00 0x00, CRC should be 0x95
        assert_eq!(bytes[0], 0x40);
        assert_eq!(bytes[5], 0x95);
    }

    #[test]
    fn test_cmd8_spi_bytes() {
        let cmd = cmd8(0x01, 0xAA);
        let bytes = cmd.to_spi_bytes();
        assert_eq!(bytes[0], 0x48); // 0x40 | 8
        assert_eq!(bytes[1], 0x00);
        assert_eq!(bytes[2], 0x00);
        assert_eq!(bytes[3], 0x01);
        assert_eq!(bytes[4], 0xAA);
    }

    #[test]
    fn cmd52_encodes_full_17_bit_address() {
        let cmd = cmd52(true, 1, false, 0x1_ABCD, 0x55);
        // write=1, function=001, raw=0, addr=0x1ABCD (bits 25:9), stuff=0, data=0x55
        let expected = (1u32 << 31) | (1u32 << 28) | (0x1_ABCDu32 << 9) | 0x55;
        assert_eq!(cmd.argument, expected);
        assert_eq!(cmd.index, 52);
    }

    #[test]
    fn cmd53_encodes_full_17_bit_address_and_count() {
        let cmd = cmd53(false, 2, true, 0x1_FFFF, true, 0x1FF);
        // write=0, function=010, block_mode=1, op_code=1, addr=0x1FFFF, count=0x1FF
        let expected = (2u32 << 28) | (1u32 << 27) | (1u32 << 26) | (0x1_FFFFu32 << 9) | 0x1FF;
        assert_eq!(cmd.argument, expected);
        assert_eq!(cmd.index, 53);
    }

    #[test]
    fn data_direction_classifies_block_commands() {
        assert_eq!(
            cmd17(0).data_direction(),
            Some(sdio_host2::DataDirection::Read)
        );
        assert_eq!(
            cmd18(0).data_direction(),
            Some(sdio_host2::DataDirection::Read)
        );
        assert_eq!(
            cmd24(0).data_direction(),
            Some(sdio_host2::DataDirection::Write)
        );
        assert_eq!(
            cmd25(0).data_direction(),
            Some(sdio_host2::DataDirection::Write)
        );
        // CMD6 is overloaded (ACMD6 vs SWITCH_FUNC); drivers tell the host
        // explicitly rather than relying on the index alone.
        assert_eq!(cmd6(0).data_direction(), None);
        assert_eq!(CMD0.data_direction(), None);
        assert_eq!(CMD12.data_direction(), None);
        assert!(CMD0.data_direction().is_none());
    }

    #[test]
    fn data_block_size_reports_known_lengths() {
        assert_eq!(cmd17(0).data_block_size(), Some(512));
        assert_eq!(cmd18(0).data_block_size(), Some(512));
        assert_eq!(cmd24(0).data_block_size(), Some(512));
        assert_eq!(cmd25(0).data_block_size(), Some(512));
        assert_eq!(cmd6(0).data_block_size(), None);
        assert_eq!(CMD0.data_block_size(), None);
        assert_eq!(CMD12.data_block_size(), None);
    }

    #[test]
    fn cmd6_high_speed_arg_matches_spec() {
        let switch = cmd6_high_speed(true);
        assert_eq!(switch.index, 6);
        assert_eq!(switch.argument, 0x80FF_FFF1);
        let check = cmd6_high_speed(false);
        assert_eq!(check.argument, 0x00FF_FFF1);
    }

    #[test]
    fn cmd6_sd_access_mode_arg_selects_group1_function() {
        let sdr104 = cmd6_sd_access_mode(true, 3);
        assert_eq!(sdr104.index, 6);
        assert_eq!(sdr104.argument, 0x80FF_FFF3);

        let ddr50 = cmd6_sd_access_mode(false, 4);
        assert_eq!(ddr50.argument, 0x00FF_FFF4);
    }

    #[test]
    fn cmd41_with_s18r_sets_1v8_request_bit() {
        let cmd = cmd41_with_s18r(true, 0xFF8000, true);
        assert_eq!(cmd.argument, 0x4100_0000 | 0x00FF_8000);
    }

    #[test]
    fn with_resp_type_overrides_only_resp_type() {
        let original = cmd41(true, 0xFF8000);
        let overridden = original.with_resp_type(ResponseType::R1);
        assert_eq!(overridden.index, original.index);
        assert_eq!(overridden.argument, original.argument);
        assert_eq!(overridden.response, ResponseType::R1);
        assert_eq!(original.response, ResponseType::R3);
    }
}
