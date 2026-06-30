//! SPI mode transport layer for SD/MMC cards
//!
//! Usage: implement [`SpiTransport`] for your platform's SPI peripheral,
//! then use [`SpiSdmmc`] to interact with the card. A [`DelayNs`]
//! implementation is also required so the driver can apply wall-clock
//! timeouts to busy/wait loops.

use embedded_hal::{delay::DelayNs, spi::SpiDevice};
use log::{debug, info, warn};

use crate::{
    cmd::Command,
    common::{block_addr_of, crc16_ccitt},
    error::{Error, ErrorContext, Phase},
    response::{
        CidResponse, CsdResponse, IfCondResponse, OcrResponse, R1Response, Response, ResponseType,
        SwitchStatus,
    },
};

/// Token markers for SPI mode data transfer
const TOKEN_START_BLOCK: u8 = 0xFE;
const TOKEN_START_MULTI_BLOCK: u8 = 0xFC;
const TOKEN_STOP_TRAN: u8 = 0xFD;

/// SPI transport trait — users implement this for their platform
pub trait SpiTransport {
    /// Assert chip select before a command/data transaction.
    fn select(&mut self) -> Result<(), Error> {
        Ok(())
    }

    /// Deassert chip select after a command/data transaction.
    fn deselect(&mut self) -> Result<(), Error> {
        Ok(())
    }

    /// Send and receive a single byte
    fn transfer_byte(&mut self, byte: u8) -> Result<u8, Error>;
    /// Send a byte (ignore response)
    fn send_byte(&mut self, byte: u8) -> Result<(), Error> {
        self.transfer_byte(byte)?;
        Ok(())
    }
    /// Send 8 clock cycles (write 0xFF)
    fn clock(&mut self) -> Result<(), Error> {
        self.transfer_byte(0xFF)?;
        Ok(())
    }
}

/// Blanket impl for embedded-hal v1 `SpiDevice<u8>`
impl<SPI> SpiTransport for SpiDeviceWrapper<SPI>
where
    SPI: SpiDevice<u8>,
{
    fn transfer_byte(&mut self, byte: u8) -> Result<u8, Error> {
        let mut buf = [byte];
        self.spi
            .transfer(&mut buf, &[byte])
            .map_err(|_| Error::BusError(ErrorContext::new(Phase::Unspecified)))?;
        Ok(buf[0])
    }
}

/// Wrapper that owns an `SpiDevice`
pub struct SpiDeviceWrapper<SPI> {
    spi: SPI,
}

impl<SPI> SpiDeviceWrapper<SPI> {
    pub fn new(spi: SPI) -> Self {
        Self { spi }
    }
}

/// SPI mode SD/MMC driver
pub struct SpiSdmmc<T: SpiTransport, D: DelayNs> {
    transport: T,
    delay: D,
    sd_v2: bool,
    high_capacity: bool,
    verify_data_crc: bool,
}

impl<T: SpiTransport, D: DelayNs> SpiSdmmc<T, D> {
    /// Maximum time to wait for an R1 byte after sending a command.
    const RESPONSE_TIMEOUT_US: u32 = 100_000;
    /// Maximum time to wait for a data start token.
    const READ_TIMEOUT_US: u32 = 100_000;
    /// Maximum time to wait for the card to leave busy state after a write.
    /// SD spec gives ~250 ms as the worst-case write-busy time.
    const WRITE_BUSY_TIMEOUT_US: u32 = 250_000;
    /// Maximum time to wait for ACMD41 to clear the idle bit.
    const INIT_TIMEOUT_US: u32 = 1_000_000;
    /// How often we sleep between polls.
    const POLL_INTERVAL_US: u32 = 50;

    pub fn new(transport: T, delay: D) -> Self {
        Self {
            transport,
            delay,
            sd_v2: false,
            high_capacity: false,
            verify_data_crc: true,
        }
    }

    /// Enable or disable verification of the CRC16 trailer that follows
    /// every data block read from the card.
    ///
    /// SPI mode generates CRC16 bytes on transmit and the card emits them on
    /// receive, but the SD spec allows the host to ignore them. Verification
    /// is on by default; disable it only if you know your bus is reliable
    /// and want to skip the per-block computation.
    pub fn set_verify_data_crc(&mut self, on: bool) {
        self.verify_data_crc = on;
    }

    // ── Initialization ──────────────────────────────────────────

    /// Initialize the card. Must be called before any other operation.
    ///
    /// Performs the standard SD card initialization sequence:
    /// 1. Send 80+ clock cycles (CMD0 preamble)
    /// 2. CMD0 → idle
    /// 3. CMD8 → detect SD v2
    /// 4. ACMD41 → wait for card ready
    /// 5. CMD58 → determine capacity type (SDHC vs SDSC)
    pub fn init(&mut self) -> Result<CardInfo, Error> {
        debug!("spi: init starting");
        for _ in 0..10 {
            self.transport.clock()?;
        }

        self.send_command(&crate::cmd::CMD0)?;
        self.sd_v2 = self.check_cmd8()?;
        debug!("spi: sd_v2={}", self.sd_v2);
        self.wait_ready()?;

        let ocr = self.read_ocr()?;
        self.high_capacity = ocr.ccs() || self.sd_v2;
        let csd = self.read_csd()?;
        let capacity_blocks = csd.capacity_blocks();
        let cid = self.read_cid().ok();
        if !self.high_capacity {
            self.send_command(&crate::cmd::cmd16(512))?;
        }

        info!(
            "spi: init done sd_v2={} high_capacity={} ocr={:#x}",
            self.sd_v2, self.high_capacity, ocr.raw
        );
        Ok(CardInfo {
            sd_v2: self.sd_v2,
            high_capacity: self.high_capacity,
            ocr: ocr.raw,
            capacity_blocks,
            cid,
        })
    }

    fn check_cmd8(&mut self) -> Result<bool, Error> {
        let cmd = crate::cmd::cmd8(0x01, 0xAA);
        match self.send_command_raw(&cmd) {
            Ok(Response::R7(resp)) => Ok(resp.verify(0x01, 0xAA)),
            Ok(Response::R1(resp)) if resp.illegal_command() => Ok(false),
            Ok(_) => Err(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 8))),
            Err(Error::Timeout(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn wait_ready(&mut self) -> Result<(), Error> {
        let mut elapsed = 0u32;
        loop {
            let cmd55 = crate::cmd::cmd55(0);
            self.send_command(&cmd55)?;

            // ACMD41 returns R3 (R1 + OCR) in native mode but a single-byte
            // R1 in SPI mode. Override the response type for this transport.
            let acmd41 = crate::cmd::cmd41(self.sd_v2, 0xFF8000).with_resp_type(ResponseType::R1);
            match self.send_command_raw(&acmd41)? {
                Response::R1(r1) => {
                    if !r1.idle() {
                        return Ok(());
                    }
                }
                _ => return Err(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 41))),
            }

            if elapsed >= Self::INIT_TIMEOUT_US {
                warn!("spi: ACMD41 timed out after {}us", elapsed);
                return Err(Error::Timeout(ErrorContext::for_cmd(Phase::Init, 41)));
            }
            self.delay.delay_us(1_000);
            elapsed = elapsed.saturating_add(1_000);
        }
    }

    fn read_ocr(&mut self) -> Result<OcrResponse, Error> {
        match self.send_command_raw(&crate::cmd::CMD58)? {
            Response::R3(ocr) => Ok(ocr),
            _ => Err(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 58))),
        }
    }

    fn read_csd(&mut self) -> Result<CsdResponse, Error> {
        match self.send_command_raw(&crate::cmd::cmd9(0))? {
            Response::R2(raw) => Ok(CsdResponse::from_raw(raw)),
            _ => Err(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 9))),
        }
    }

    fn read_cid(&mut self) -> Result<CidResponse, Error> {
        match self.send_command_raw(&crate::cmd::cmd10(0))? {
            Response::R2(raw) => Ok(CidResponse::from_raw(raw)),
            _ => Err(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 10))),
        }
    }

    // ── Data Transfer ───────────────────────────────────────────

    /// Read a single 512-byte block at the given address
    pub fn read_block(&mut self, addr: u32, buf: &mut [u8; 512]) -> Result<(), Error> {
        let block_addr = block_addr_of(addr, self.high_capacity);
        let cmd = crate::cmd::cmd17(block_addr);
        self.send_command(&cmd)?;
        self.read_data_block(buf)
    }

    /// Write a single 512-byte block at the given address
    pub fn write_block(&mut self, addr: u32, buf: &[u8; 512]) -> Result<(), Error> {
        let block_addr = block_addr_of(addr, self.high_capacity);
        let cmd = crate::cmd::cmd24(block_addr);
        self.send_command(&cmd)?;
        self.write_data_block(buf)
    }

    /// Read multiple blocks starting at `addr`
    pub fn read_blocks<F>(&mut self, addr: u32, count: u32, mut handler: F) -> Result<(), Error>
    where
        F: FnMut(u32, &[u8; 512]),
    {
        let block_addr = block_addr_of(addr, self.high_capacity);
        let cmd = crate::cmd::cmd18(block_addr);
        self.send_command(&cmd)?;

        let mut buf = [0u8; 512];
        for i in 0..count {
            self.read_data_block(&mut buf)?;
            handler(addr + i, &buf);
        }

        self.send_command(&crate::cmd::CMD12)?;
        self.wait_not_busy()?;
        self.transport.deselect()?;
        Ok(())
    }

    /// Write multiple blocks starting at `addr`
    pub fn write_blocks(&mut self, addr: u32, blocks: &[[u8; 512]]) -> Result<(), Error> {
        let block_addr = block_addr_of(addr, self.high_capacity);
        let cmd = crate::cmd::cmd25(block_addr);
        self.send_command(&cmd)?;

        for block in blocks {
            self.transport.send_byte(TOKEN_START_MULTI_BLOCK)?;
            for &b in block {
                self.transport.send_byte(b)?;
            }
            let crc = crc16_ccitt(block).to_be_bytes();
            self.transport.send_byte(crc[0])?;
            self.transport.send_byte(crc[1])?;

            let resp = self.wait_for_response(Self::RESPONSE_TIMEOUT_US)?;
            if (resp & 0x1F) != 0x05 {
                return Err(Error::WriteError(ErrorContext::for_cmd(
                    Phase::DataWrite,
                    25,
                )));
            }
            self.wait_not_busy()?;
        }

        self.transport.send_byte(TOKEN_STOP_TRAN)?;
        self.transport.clock()?;
        self.wait_not_busy()?;
        self.transport.deselect()?;
        Ok(())
    }

    // ── Low-level helpers ───────────────────────────────────────

    fn send_command(&mut self, cmd: &Command) -> Result<R1Response, Error> {
        let resp = self.send_command_raw(cmd)?;
        match resp {
            Response::R1(r1) | Response::R1b(r1) => Ok(r1),
            _ => Err(Error::BadResponse(ErrorContext::for_cmd(
                Phase::ResponseWait,
                cmd.index,
            ))),
        }
    }

    fn send_command_raw(&mut self, cmd: &Command) -> Result<Response, Error> {
        self.transport.select()?;
        let bytes = cmd.to_spi_bytes();
        for &b in &bytes {
            self.transport.send_byte(b)?;
        }

        let response = match cmd.response {
            ResponseType::None => {
                let r1 = self.read_r1()?;
                Ok(Response::R1(r1))
            }
            ResponseType::R1 => {
                let r1 = self.read_r1()?;
                Ok(Response::R1(r1))
            }
            ResponseType::R1b => {
                let r1 = self.read_r1()?;
                self.wait_not_busy()?;
                Ok(Response::R1b(r1))
            }
            ResponseType::R3 => {
                self.read_r1()?;
                let mut ocr = [0u8; 4];
                for b in &mut ocr {
                    *b = self.transport.transfer_byte(0xFF)?;
                }
                let raw = u32::from_be_bytes(ocr);
                Ok(Response::R3(OcrResponse::from_raw(raw)))
            }
            ResponseType::R7 => {
                let r1 = self.read_r1()?;
                if r1.illegal_command() {
                    return Ok(Response::R1(r1));
                }
                let mut data = [0u8; 4];
                for b in &mut data {
                    *b = self.transport.transfer_byte(0xFF)?;
                }
                let raw = u32::from_be_bytes(data);
                Ok(Response::R7(IfCondResponse::from_raw(raw)))
            }
            ResponseType::R2 => {
                let r1 = self.read_r1()?;
                if r1.raw != 0 {
                    return Ok(Response::R1(r1));
                }
                let mut buf = [0u8; 16];
                self.read_data_register(&mut buf)?;
                Ok(Response::R2(buf))
            }
            _ => {
                let r1 = self.read_r1()?;
                Ok(Response::R1(r1))
            }
        };
        // Hold CS asserted across the data phase that follows the command.
        if cmd.data_direction().is_none() {
            self.transport.deselect()?;
        }
        response
    }

    fn read_r1(&mut self) -> Result<R1Response, Error> {
        let raw = self.wait_for_response(Self::RESPONSE_TIMEOUT_US)?;
        R1Response::from_spi_byte(raw)
    }

    /// Poll the bus for the first non-`0xFF` byte, up to `timeout_us`.
    fn wait_for_response(&mut self, timeout_us: u32) -> Result<u8, Error> {
        let mut elapsed = 0u32;
        loop {
            let b = self.transport.transfer_byte(0xFF)?;
            if b != 0xFF {
                return Ok(b);
            }
            if elapsed >= timeout_us {
                return Err(Error::Timeout(ErrorContext::new(Phase::ResponseWait)));
            }
            self.delay.delay_us(Self::POLL_INTERVAL_US);
            elapsed = elapsed.saturating_add(Self::POLL_INTERVAL_US);
        }
    }

    /// Wait for the data start token (0xFE) and read `buf.len()` payload bytes.
    fn read_data_into(&mut self, buf: &mut [u8]) -> Result<(), Error> {
        let mut elapsed = 0u32;
        loop {
            let b = self.transport.transfer_byte(0xFF)?;
            if b == TOKEN_START_BLOCK {
                break;
            }
            if b != 0xFF {
                return Err(Error::ReadError(ErrorContext::new(Phase::DataRead)));
            }
            if elapsed >= Self::READ_TIMEOUT_US {
                return Err(Error::Timeout(ErrorContext::new(Phase::DataRead)));
            }
            self.delay.delay_us(Self::POLL_INTERVAL_US);
            elapsed = elapsed.saturating_add(Self::POLL_INTERVAL_US);
        }

        for b in buf.iter_mut() {
            *b = self.transport.transfer_byte(0xFF)?;
        }

        let crc_high = self.transport.transfer_byte(0xFF)?;
        let crc_low = self.transport.transfer_byte(0xFF)?;
        if self.verify_data_crc {
            let received = u16::from_be_bytes([crc_high, crc_low]);
            let computed = crc16_ccitt(buf);
            if received != computed {
                warn!(
                    "spi: data CRC mismatch (received={:#x} computed={:#x})",
                    received, computed
                );
                return Err(Error::Crc(ErrorContext::new(Phase::DataRead)));
            }
        }
        Ok(())
    }

    fn read_data_register(&mut self, buf: &mut [u8; 16]) -> Result<(), Error> {
        self.read_data_into(buf)
    }

    fn read_data_block(&mut self, buf: &mut [u8; 512]) -> Result<(), Error> {
        self.read_data_into(buf)?;
        self.transport.deselect()?;
        Ok(())
    }

    fn write_data_block(&mut self, buf: &[u8; 512]) -> Result<(), Error> {
        self.transport.send_byte(TOKEN_START_BLOCK)?;
        for &b in buf {
            self.transport.send_byte(b)?;
        }
        let crc = crc16_ccitt(buf).to_be_bytes();
        self.transport.send_byte(crc[0])?;
        self.transport.send_byte(crc[1])?;

        let resp = self.wait_for_response(Self::RESPONSE_TIMEOUT_US)?;
        if (resp & 0x1F) != 0x05 {
            return Err(Error::WriteError(ErrorContext::new(Phase::DataWrite)));
        }

        self.wait_not_busy()?;
        self.transport.deselect()?;
        Ok(())
    }

    fn wait_not_busy(&mut self) -> Result<(), Error> {
        let mut elapsed = 0u32;
        loop {
            if self.transport.transfer_byte(0xFF)? == 0xFF {
                return Ok(());
            }
            if elapsed >= Self::WRITE_BUSY_TIMEOUT_US {
                return Err(Error::Timeout(ErrorContext::new(Phase::BusyWait)));
            }
            self.delay.delay_us(Self::POLL_INTERVAL_US);
            elapsed = elapsed.saturating_add(Self::POLL_INTERVAL_US);
        }
    }

    /// Issue a CMD6 SWITCH_FUNC and read back the 64-byte status block.
    pub fn switch_function(&mut self, cmd: &Command) -> Result<SwitchStatus, Error> {
        // CMD6 has no inherent data direction (ACMD6 vs SWITCH_FUNC overlap),
        // so we drive the bus manually here to keep CS asserted across the
        // R1 byte and the 64-byte data phase that follows.
        self.transport.select()?;
        let bytes = cmd.to_spi_bytes();
        for &b in &bytes {
            self.transport.send_byte(b)?;
        }
        let _r1 = self.read_r1()?;

        let mut buf = [0u8; 64];
        self.read_data_into(&mut buf)?;
        self.transport.deselect()?;
        Ok(SwitchStatus::from_raw(buf))
    }

    /// Switch the card to high speed (50 MHz) by sending CMD6 with mode=1
    /// and group 1 = 1. Returns `Ok(true)` if the status block confirms
    /// high-speed selected; `Ok(false)` otherwise.
    ///
    /// The host is responsible for actually raising the SPI clock after this
    /// returns success.
    pub fn switch_to_high_speed(&mut self) -> Result<bool, Error> {
        let status = self.switch_function(&crate::cmd::cmd6_high_speed(true))?;
        let active = status.high_speed_active();
        if active {
            info!("spi: switched to high-speed mode");
        } else {
            warn!("spi: high-speed switch did not take effect");
        }
        Ok(active)
    }
}

/// Card information obtained during initialization
#[derive(Debug, Clone, Copy)]
pub struct CardInfo {
    pub sd_v2: bool,
    pub high_capacity: bool,
    pub ocr: u32,
    /// User-data capacity in 512-byte blocks, parsed from the CSD.
    /// `None` if the CSD reports a structure version we do not yet support.
    pub capacity_blocks: Option<u64>,
    /// Card identification register, parsed via CMD10. `None` if the card
    /// did not return a valid R2 response.
    pub cid: Option<CidResponse>,
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec::Vec;

    use super::*;
    use crate::cmd;

    /// `DelayNs` that does nothing — fine for unit tests against a scripted
    /// transport because no real time is being measured.
    struct NullDelay;

    impl DelayNs for NullDelay {
        fn delay_ns(&mut self, _ns: u32) {}
    }

    struct ScriptedTransport {
        rx: Vec<u8>,
        tx: Vec<u8>,
    }

    impl ScriptedTransport {
        fn new(rx: Vec<u8>) -> Self {
            Self { rx, tx: Vec::new() }
        }

        fn push_ignored(rx: &mut Vec<u8>, count: usize) {
            for _ in 0..count {
                rx.push(0xFF);
            }
        }

        fn push_command_response(rx: &mut Vec<u8>, r1: u8, extra: &[u8]) {
            Self::push_ignored(rx, 6);
            rx.push(r1);
            rx.extend_from_slice(extra);
        }

        fn tx_contains(&self, bytes: &[u8]) -> bool {
            self.tx.windows(bytes.len()).any(|window| window == bytes)
        }
    }

    impl SpiTransport for ScriptedTransport {
        fn transfer_byte(&mut self, byte: u8) -> Result<u8, Error> {
            self.tx.push(byte);
            if self.rx.is_empty() {
                return Err(Error::Timeout(ErrorContext::default()));
            }
            Ok(self.rx.remove(0))
        }
    }

    fn driver(rx: Vec<u8>) -> SpiSdmmc<ScriptedTransport, NullDelay> {
        SpiSdmmc::new(ScriptedTransport::new(rx), NullDelay)
    }

    fn push_csd_v2_response(rx: &mut Vec<u8>) {
        // R2 wrapper: R1=0, then start token + 16 CSD bytes + 2 CRC
        ScriptedTransport::push_command_response(rx, 0x00, &[]);
        rx.push(TOKEN_START_BLOCK);
        let mut csd = [0u8; 16];
        csd[0] = 0x40; // CSD v2
        csd[7] = 0x00;
        csd[8] = 0x0F;
        csd[9] = 0x0F;
        rx.extend_from_slice(&csd);
        rx.extend_from_slice(&crc16_ccitt(&csd).to_be_bytes());
    }

    fn push_cid_response(rx: &mut Vec<u8>) {
        ScriptedTransport::push_command_response(rx, 0x00, &[]);
        rx.push(TOKEN_START_BLOCK);
        let mut cid = [0u8; 16];
        cid[0] = 0x03;
        cid[1] = b'S';
        cid[2] = b'D';
        cid[3] = b'A';
        cid[4] = b'B';
        cid[5] = b'C';
        cid[6] = b'1';
        cid[7] = b'2';
        rx.extend_from_slice(&cid);
        rx.extend_from_slice(&crc16_ccitt(&cid).to_be_bytes());
    }

    #[test]
    fn init_polls_acmd41_until_spi_r1_leaves_idle() {
        let mut rx = Vec::new();
        ScriptedTransport::push_ignored(&mut rx, 10);
        ScriptedTransport::push_command_response(&mut rx, 0x01, &[]);
        ScriptedTransport::push_command_response(&mut rx, 0x01, &[0x00, 0x00, 0x01, 0xAA]);
        ScriptedTransport::push_command_response(&mut rx, 0x01, &[]);
        ScriptedTransport::push_command_response(&mut rx, 0x01, &[]);
        ScriptedTransport::push_command_response(&mut rx, 0x01, &[]);
        ScriptedTransport::push_command_response(&mut rx, 0x00, &[]);
        ScriptedTransport::push_command_response(&mut rx, 0x00, &[0xC0, 0xFF, 0x80, 0x00]);
        push_csd_v2_response(&mut rx);
        push_cid_response(&mut rx);

        let mut driver = driver(rx);
        let info = driver.init().unwrap();

        assert!(info.sd_v2);
        assert!(info.high_capacity);
        assert_eq!(info.ocr, 0xC0FF_8000);
        assert_eq!(info.capacity_blocks, Some((0x0F0F + 1) * 1024));
        let cid = info.cid.expect("CID parsed during init");
        assert_eq!(cid.manufacturer_id(), 0x03);
        assert_eq!(&cid.oem_id(), b"SD");
        assert_eq!(&cid.product_name(), b"ABC12");
        assert!(driver.transport.tx_contains(&cmd::CMD0.to_spi_bytes()));
        assert!(
            driver
                .transport
                .tx_contains(&cmd::cmd8(0x01, 0xAA).to_spi_bytes())
        );
        assert!(driver.transport.tx_contains(&cmd::CMD58.to_spi_bytes()));
        assert!(driver.transport.tx_contains(&cmd::cmd10(0).to_spi_bytes()));
    }

    #[test]
    fn read_block_times_out_when_start_token_never_arrives() {
        let mut rx = Vec::new();
        ScriptedTransport::push_command_response(&mut rx, 0x00, &[]);
        // Provide enough 0xFF bytes so the wait loop hits its timeout, plus
        // padding that would have been the block payload had it arrived.
        ScriptedTransport::push_ignored(&mut rx, 10_000);
        rx.extend_from_slice(&[0xAA; 512]);
        rx.extend_from_slice(&[0xFF; 2]);

        let mut driver = driver(rx);
        driver.high_capacity = true;
        let mut buf = [0u8; 512];

        assert!(matches!(
            driver.read_block(7, &mut buf),
            Err(Error::Timeout(_))
        ));
        assert!(driver.transport.tx_contains(&cmd::cmd17(7).to_spi_bytes()));
    }

    #[test]
    fn read_block_returns_payload_when_crc_matches() {
        let mut payload = [0u8; 512];
        for (i, b) in payload.iter_mut().enumerate() {
            *b = (i & 0xFF) as u8;
        }
        let crc = crc16_ccitt(&payload).to_be_bytes();

        let mut rx = Vec::new();
        ScriptedTransport::push_command_response(&mut rx, 0x00, &[]);
        rx.push(0xFF);
        rx.push(TOKEN_START_BLOCK);
        rx.extend_from_slice(&payload);
        rx.extend_from_slice(&crc);

        let mut driver = driver(rx);
        driver.high_capacity = true;
        let mut buf = [0u8; 512];
        driver.read_block(0, &mut buf).unwrap();
        assert_eq!(&buf[..], &payload[..]);
    }

    #[test]
    fn read_block_reports_crc_error_when_trailer_mismatches() {
        let payload = [0u8; 512];
        let mut bad_crc = crc16_ccitt(&payload).to_be_bytes();
        bad_crc[0] ^= 0xFF;

        let mut rx = Vec::new();
        ScriptedTransport::push_command_response(&mut rx, 0x00, &[]);
        rx.push(0xFF);
        rx.push(TOKEN_START_BLOCK);
        rx.extend_from_slice(&payload);
        rx.extend_from_slice(&bad_crc);

        let mut driver = driver(rx);
        driver.high_capacity = true;
        let mut buf = [0u8; 512];
        assert!(matches!(driver.read_block(0, &mut buf), Err(Error::Crc(_))));
    }

    #[test]
    fn write_block_emits_correct_crc16_after_payload() {
        let mut rx = Vec::new();
        // R1=0x00 ack from CMD24…
        ScriptedTransport::push_command_response(&mut rx, 0x00, &[]);
        // …then echo bytes for the start token, the 512-byte payload, and
        // the 2 CRC bytes the driver is about to push out.
        ScriptedTransport::push_ignored(&mut rx, 1 + 512 + 2);
        rx.push(0x05); // data response: accepted
        rx.push(0xFF); // not busy

        let mut driver = driver(rx);
        driver.high_capacity = true;
        let payload = [0xA5u8; 512];
        driver.write_block(0, &payload).unwrap();

        let crc = crc16_ccitt(&payload).to_be_bytes();
        assert!(driver.transport.tx_contains(&payload));
        assert!(driver.transport.tx_contains(&crc));
    }

    #[test]
    fn switch_to_high_speed_returns_true_when_status_confirms() {
        let mut status = [0u8; 64];
        status[16] = 0x01; // group 1 = high-speed
        let crc = crc16_ccitt(&status).to_be_bytes();

        let mut rx = Vec::new();
        // CMD6 ack: 6 padding + R1=0x00.
        ScriptedTransport::push_command_response(&mut rx, 0x00, &[]);
        rx.push(0xFF); // gap before start token
        rx.push(TOKEN_START_BLOCK);
        rx.extend_from_slice(&status);
        rx.extend_from_slice(&crc);

        let mut driver = driver(rx);
        let active = driver.switch_to_high_speed().unwrap();
        assert!(active);

        // Confirm CMD6 with mode=1, group1=1 was actually sent.
        let cmd = cmd::cmd6_high_speed(true);
        assert!(driver.transport.tx_contains(&cmd.to_spi_bytes()));
    }

    #[test]
    fn switch_to_high_speed_returns_false_when_card_keeps_default() {
        let status = [0u8; 64]; // group 1 = 0 (default speed)
        let crc = crc16_ccitt(&status).to_be_bytes();

        let mut rx = Vec::new();
        ScriptedTransport::push_command_response(&mut rx, 0x00, &[]);
        rx.push(0xFF);
        rx.push(TOKEN_START_BLOCK);
        rx.extend_from_slice(&status);
        rx.extend_from_slice(&crc);

        let mut driver = driver(rx);
        let active = driver.switch_to_high_speed().unwrap();
        assert!(!active);
    }
}
