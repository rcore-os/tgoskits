//! SDIO (Secure Digital Input Output) mode transport layer
//!
//! SDIO mode uses a dedicated host controller with 1-bit or 4-bit data bus.
//! Implement [`SdioHost`] for your platform's SDIO peripheral, and supply a
//! [`DelayNs`] implementation so the driver can apply wall-clock timeouts.

pub use embedded_hal::delay::DelayNs;
use log::{debug, info, warn};

pub use crate::cmd::DataDirection;
#[allow(unused_imports)]
use crate::error::{Error, ErrorContext, Phase};
use crate::{
    cmd::Command,
    common::block_addr_of,
    response::{
        CardState, CidResponse, CsdResponse, OcrResponse, Response, ResponseType, SwitchStatus,
    },
};

/// SDIO bus width
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusWidth {
    /// 1-bit bus
    Bit1,
    /// 4-bit bus
    Bit4,
    /// 8-bit bus (eMMC). Configured via the MMC `CMD6 SWITCH` flow which is
    /// outside the scope of the SD ACMD6 path used by this driver.
    Bit8,
}

/// SDIO clock speed
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockSpeed {
    /// Identification clock used during card reset / OCR negotiation.
    Identification,
    /// Default speed: up to 25 MHz
    Default,
    /// High speed: up to 50 MHz
    HighSpeed,
    /// SDR12: 12.5 MB/s
    Sdr12,
    /// SDR25: 25 MB/s
    Sdr25,
    /// SDR50: 50 MB/s
    Sdr50,
    /// SDR104: 104 MB/s
    Sdr104,
    /// DDR50: 50 MB/s (DDR)
    Ddr50,
    /// HS200: 200 MHz SDR, eMMC HS200 mode. Distinct from SDR104
    /// because the host typically routes eMMC and SD UHS-I through
    /// different timing tables.
    Hs200,
}

/// Bus signaling voltage. Default-speed and HS modes use 3.3 V; UHS-I
/// and HS200/HS400 require switching to 1.8 V via CMD11.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalVoltage {
    /// 3.3 V (or 3.0 V — they share an IO domain on most controllers).
    /// The bus comes up here at power-on.
    V330,
    /// 1.8 V — required for SDR50 / SDR104 / DDR50 / HS200 / HS400.
    V180,
    /// 1.2 V — only relevant on certain HS200_12V eMMC parts. Most
    /// hosts don't implement it; treated as opt-in.
    V120,
}

/// Trait that the platform must implement for the SDIO host controller.
///
/// The driver tracks the published RCA itself, so host implementations no
/// longer need to snoop R6 responses or expose a `rca()` accessor.
pub trait SdioHost {
    /// Send a command and receive the response
    fn send_command(&mut self, cmd: &Command) -> Result<Response, Error>;

    /// Issue a read-data command and complete its data phase.
    fn read_data(
        &mut self,
        cmd: &Command,
        buf: &mut [u8],
        block_size: u32,
        block_count: u32,
    ) -> Result<Response, Error>;

    /// Issue a write-data command and complete its data phase.
    fn write_data(
        &mut self,
        cmd: &Command,
        buf: &[u8],
        block_size: u32,
        block_count: u32,
    ) -> Result<Response, Error>;

    /// Set the bus width
    fn set_bus_width(&mut self, width: BusWidth) -> Result<(), Error>;

    /// Set the clock speed
    fn set_clock(&mut self, speed: ClockSpeed) -> Result<(), Error>;

    /// Tell the host how many data blocks the next multi-block transfer will
    /// move (CMD18 read or CMD25 write). Single-block commands always pass 1
    /// — hosts can ignore this for that case. Default is a no-op for hosts
    /// that derive the count internally.
    fn set_block_count(&mut self, _count: u32) -> Result<(), Error> {
        Ok(())
    }

    /// Tell the host the shape of the data phase that the *next* command
    /// will trigger.
    ///
    /// This is needed because some commands (notably CMD6, which is reused
    /// for ACMD6 SET_BUS_WIDTH and CMD6 SWITCH_FUNC) can't be classified
    /// from the index alone. The driver always calls this before issuing a
    /// data-bearing command and passes `direction = None` to clear any
    /// previous hint. Default is a no-op for hosts that derive the data
    /// shape from the command index themselves.
    fn prepare_data_transfer(
        &mut self,
        _direction: DataDirection,
        _block_size: u32,
        _block_count: u32,
    ) -> Result<(), Error> {
        Ok(())
    }

    /// Switch the bus signaling voltage (typically 3.3 V → 1.8 V for
    /// UHS-I or HS200 entry). The protocol layer issues CMD11 *before*
    /// calling this; the host is responsible for the controller-side
    /// transition (gate SD clock → flip the IO domain → wait t_VSW
    /// (≥ 5 ms) → re-enable SD clock at the new level → confirm
    /// `DAT[3:0]` is high).
    ///
    /// Default returns `UnsupportedCommand` so hosts that don't implement
    /// 1.8 V signaling get a clean fallback path instead of silently
    /// keeping the bus at 3.3 V.
    fn switch_voltage(&mut self, _voltage: SignalVoltage) -> Result<(), Error> {
        Err(Error::UnsupportedCommand)
    }

    /// Run the controller's tuning state machine for the given command
    /// index (CMD19 for SD UHS-I, CMD21 for eMMC HS200). The host is
    /// responsible for issuing tuning blocks in a loop, comparing
    /// against the expected pattern, and reporting back whether a
    /// stable sampling phase was found.
    ///
    /// Default returns `UnsupportedCommand`. Hosts that report success
    /// without actually tuning are silently lying to the caller — only
    /// implement this when the controller can validate the result.
    fn execute_tuning(&mut self, _cmd_index: u8) -> Result<(), Error> {
        Err(Error::UnsupportedCommand)
    }
}

/// SDIO mode SD/MMC driver
pub struct SdioSdmmc<H: SdioHost, D: DelayNs> {
    host: H,
    delay: D,
    rca: u16,
    high_capacity: bool,
    bus_width: BusWidth,
    kind: CardKind,
    sd_speed_selection_enabled: bool,
}

#[derive(Debug, Clone, Copy)]
enum SdAccessMode {
    HighSpeed,
    Sdr50,
    Sdr104,
    Ddr50,
}

impl SdAccessMode {
    fn function(self) -> u8 {
        match self {
            Self::HighSpeed => 1,
            Self::Sdr50 => 2,
            Self::Sdr104 => 3,
            Self::Ddr50 => 4,
        }
    }

    fn clock(self) -> ClockSpeed {
        match self {
            Self::HighSpeed => ClockSpeed::HighSpeed,
            Self::Sdr50 => ClockSpeed::Sdr50,
            Self::Sdr104 => ClockSpeed::Sdr104,
            Self::Ddr50 => ClockSpeed::Ddr50,
        }
    }

    fn needs_tuning(self) -> bool {
        matches!(self, Self::Sdr50 | Self::Sdr104)
    }

    const fn name(self) -> &'static str {
        match self {
            Self::HighSpeed => "HighSpeed",
            Self::Sdr50 => "SDR50",
            Self::Sdr104 => "SDR104",
            Self::Ddr50 => "DDR50",
        }
    }
}

impl<H: SdioHost, D: DelayNs> SdioSdmmc<H, D> {
    /// Maximum total time to wait for ACMD41 to report card power-up.
    const INIT_TIMEOUT_MS: u32 = 1_000;
    /// Interval between ACMD41 polls.
    const INIT_POLL_MS: u32 = 10;

    pub fn new(host: H, delay: D) -> Self {
        Self {
            host,
            delay,
            rca: 0,
            high_capacity: false,
            bus_width: BusWidth::Bit1,
            kind: CardKind::Sd,
            sd_speed_selection_enabled: true,
        }
    }

    /// Returns mutable access to the underlying SDIO host controller.
    pub fn host_mut(&mut self) -> &mut H {
        &mut self.host
    }

    /// Returns whether the initialized card uses sector addressing.
    pub fn is_high_capacity(&self) -> bool {
        self.high_capacity
    }

    /// Enable or disable optional SD CMD6 speed-mode selection.
    ///
    /// When disabled, SD cards still leave identification mode and run at
    /// default speed, but the driver does not switch the card to HighSpeed or
    /// UHS-I timing.
    pub fn set_sd_speed_selection_enabled(&mut self, enabled: bool) {
        self.sd_speed_selection_enabled = enabled;
    }

    /// Which card family the driver detected. Meaningful only after a
    /// successful [`init`](Self::init); defaults to [`CardKind::Sd`].
    pub fn kind(&self) -> CardKind {
        self.kind
    }

    /// Currently published Relative Card Address. `0` until [`init`](Self::init)
    /// has run successfully.
    pub fn rca(&self) -> u16 {
        self.rca
    }

    /// Initialize the card in SDIO mode.
    ///
    /// Detects SD vs eMMC at runtime:
    ///
    /// 1. CMD0 — global reset
    /// 2. CMD8 — if it echoes back, the card is SD v2 (SDHC/SDXC); if it
    ///    times out, the card is either SD v1 or eMMC
    /// 3. ACMD41 — if it powers the card up, that confirms SD; if it
    ///    fails too, fall back to CMD1 (MMC SEND_OP_COND)
    /// 4. CMD2 / CMD3 — get CID, publish RCA. SD has the card pick the
    ///    RCA via R6; MMC has the host assign it via R1.
    /// 5. CMD9 / CMD7 — read CSD, then select the card
    /// 6. SD only: ACMD6 to switch to 4-bit. eMMC bus widening is left
    ///    to a follow-up that wires CMD6 SWITCH + EXT_CSD.
    pub fn init(&mut self) -> Result<CardInfo, Error> {
        info!("sdio: init starting");
        self.host.set_bus_width(BusWidth::Bit1)?;
        self.host.set_clock(ClockSpeed::Identification)?;

        // CMD0: reset
        info!("sdio: CMD0 reset");
        self.host.send_command(&crate::cmd::CMD0)?;

        // CMD8: detect SD v2. eMMC ignores CMD8 (or times out depending on
        // host); we treat any non-success as "not SD v2" and let ACMD41
        // arbitrate further.
        let sd_v2 = self.check_cmd8().unwrap_or(false);
        info!("sdio: CMD8 sd_v2={}", sd_v2);

        // ACMD41 first; if the card never responds at all (typical eMMC),
        // try CMD1 instead.
        let (kind, ocr) = match self.wait_ready_sd(sd_v2, true) {
            Ok(ocr) => (CardKind::Sd, ocr),
            Err(_sd_err) => {
                info!("sdio: ACMD41 failed ({:?}), trying MMC CMD1", _sd_err);
                let ocr = self.wait_ready_mmc()?;
                (CardKind::Mmc, ocr)
            }
        };
        self.kind = kind;
        info!("sdio: detected {:?} ocr={:#010x}", kind, ocr.raw);

        // CMD2: get CID (identical for SD and MMC)
        info!("sdio: CMD2 read CID");
        let cid = match self.host.send_command(&crate::cmd::CMD2)? {
            Response::R2(raw) => Some(CidResponse::from_raw(raw)),
            _ => None,
        };

        // CMD3: get RCA — SD lets the card pick (R6); MMC has the host
        // assign one and the card just acks with R1. We pick `1` as a
        // sensible default; drivers that want to talk to multiple eMMC
        // devices on the same bus would need to extend this.
        self.rca = match kind {
            CardKind::Sd => self.get_rca_sd()?,
            CardKind::Mmc => self.assign_rca_mmc(1)?,
        };
        info!("sdio: CMD3 rca={:#x}", self.rca);

        // CMD9: get CSD → derive capacity
        info!("sdio: CMD9 read CSD");
        let cmd9 = crate::cmd::cmd9(self.rca);
        let csd_response = self.host.send_command(&cmd9)?;
        let mut capacity_blocks = match csd_response {
            Response::R2(raw) => CsdResponse::from_raw(raw).capacity_blocks(),
            _ => None,
        };
        info!("sdio: CSD capacity_blocks={:?}", capacity_blocks);

        // CMD7: select card
        info!("sdio: CMD7 select card");
        let cmd7 = crate::cmd::cmd7(self.rca);
        self.host.send_command(&cmd7)?;

        // OCR bit 30 means "high capacity" on both SD (CCS) and MMC
        // (sector mode), so the same accessor works.
        self.high_capacity = ocr.ccs();

        // Bus widening + speed switch:
        //   SD: ACMD6 → 4-bit, well-supported and required for any decent
        //       throughput.
        //   MMC: read EXT_CSD, write BUS_WIDTH = 8-bit (with 4-bit
        //       fallback if the host refuses 8-bit), then opt into HS @
        //       52 MHz when the card advertises it. The EXT_CSD also
        //       provides the authoritative sector count, which we
        //       prefer over the legacy CSD value for ≥ 2 GB devices.
        let mut ext_csd = None;
        match kind {
            CardKind::Sd => {
                info!("sdio: switch SD bus width to 4-bit");
                self.set_bus_width_sd(BusWidth::Bit4)?;
                if self.sd_speed_selection_enabled {
                    self.try_sd_best_speed(ocr);
                } else {
                    self.host.set_clock(ClockSpeed::Default)?;
                    info!("sdio: SD speed selection disabled; staying at default speed");
                }
            }
            CardKind::Mmc => {
                info!("sdio: read MMC EXT_CSD");
                let csd = self.read_ext_csd()?;

                if let Some(sectors) = csd.sector_count() {
                    capacity_blocks = Some(sectors as u64);
                    info!("sdio: EXT_CSD sector_count={}", sectors);
                }

                // Prefer 8-bit; fall back to 4-bit if the host can't
                // (e.g. SDHCI MVP rejects Bit8). 1-bit is the last
                // resort and matches the controller's reset default.
                if let Err(_e8) = self.set_bus_width_mmc(BusWidth::Bit8) {
                    info!("sdio: 8-bit refused ({:?}), trying 4-bit", _e8);
                    if let Err(_e4) = self.set_bus_width_mmc(BusWidth::Bit4) {
                        info!("sdio: 4-bit refused ({:?}), staying at 1-bit", _e4);
                    }
                }

                // Speed selection ladder, fastest-first:
                //   1. HS200 (200 MHz, 1.8 V, requires tuning)
                //   2. HS @ 52 MHz (no tuning, no voltage switch)
                //   3. Default speed (no work needed)
                //
                // Each step probes the *card* via DEVICE_TYPE first and
                // then asks the host whether it can drive the timing.
                // Any host-side rejection drops to the next step rather
                // than failing init, so a card on a PIO-only controller
                // still comes up at default speed.
                let dt = csd.device_type();
                let want_hs200 = dt.supports_hs200() && self.try_hs200(self.bus_width).is_ok();
                if !want_hs200 && dt.supports_hs_52() {
                    // EXT_CSD.HS_TIMING = 1 (high speed)
                    if let Err(_e) =
                        self.mmc_switch_write_byte(crate::cmd::ext_csd::HS_TIMING as u8, 1)
                    {
                        info!("sdio: MMC HS_TIMING switch refused ({:?})", _e);
                    } else if let Err(_e) = self.host.set_clock(ClockSpeed::HighSpeed) {
                        info!("sdio: host refused HighSpeed clock ({:?})", _e);
                    }
                }

                ext_csd = Some(csd);
            }
        }

        info!(
            "sdio: init done kind={:?} sd_v2={} high_capacity={} rca={:#x} ocr={:#x}",
            kind, sd_v2, self.high_capacity, self.rca, ocr.raw
        );
        Ok(CardInfo {
            kind,
            sd_v2,
            high_capacity: self.high_capacity,
            ocr: ocr.raw,
            rca: self.rca,
            capacity_blocks,
            cid,
            ext_csd,
        })
    }

    /// Check CMD8 response. Returns `Ok(true)` when the card echoes the
    /// expected check pattern (SD v2). Any timeout / bad response /
    /// mismatch returns `Ok(false)` so the caller can fall through to
    /// ACMD41 / CMD1; only catastrophic bus errors propagate.
    fn check_cmd8(&mut self) -> Result<bool, Error> {
        let cmd = crate::cmd::cmd8(0x01, 0xAA);
        match self.host.send_command(&cmd) {
            Ok(Response::R7(resp)) => Ok(resp.verify(0x01, 0xAA)),
            Ok(_) => Ok(false),
            Err(Error::Timeout(_)) | Err(Error::BadResponse(_)) | Err(Error::Crc(_)) => {
                // No response / garbage CRC is the canonical "not SD v2"
                // signal. Don't propagate so `init` can try ACMD41 anyway
                // (SD v1 cards behave this way too).
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    /// Send ACMD41 until card is ready (SD path).
    fn wait_ready_sd(&mut self, sd_v2: bool, request_1v8: bool) -> Result<OcrResponse, Error> {
        let mut elapsed = 0u32;
        loop {
            debug!("sdio: CMD55 before ACMD41 elapsed={}ms", elapsed);
            let cmd55 = crate::cmd::cmd55(0);
            self.host.send_command(&cmd55)?;

            let acmd41 = crate::cmd::cmd41_with_s18r(sd_v2, 0xFF8000, request_1v8);
            match self.host.send_command(&acmd41)? {
                Response::R3(ocr) => {
                    debug!(
                        "sdio: ACMD41 ocr={:#010x} ready={}",
                        ocr.raw,
                        ocr.card_powered_up()
                    );
                    if ocr.card_powered_up() {
                        return Ok(ocr);
                    }
                }
                _ => return Err(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 41))),
            }

            if elapsed >= Self::INIT_TIMEOUT_MS {
                warn!("sdio: ACMD41 timed out after {}ms", elapsed);
                return Err(Error::Timeout(ErrorContext::for_cmd(Phase::Init, 41)));
            }
            self.delay.delay_ms(Self::INIT_POLL_MS);
            elapsed = elapsed.saturating_add(Self::INIT_POLL_MS);
        }
    }

    /// Send CMD1 until the eMMC reports power-up complete (MMC path).
    ///
    /// Sets the HCS bit (bit 30) in the operating-condition argument so
    /// cards larger than 2 GB switch to sector addressing — same idea as
    /// SD's HCS bit in ACMD41. Bits 23..15 cover the standard 2.7–3.6 V
    /// window.
    fn wait_ready_mmc(&mut self) -> Result<OcrResponse, Error> {
        const MMC_HCS: u32 = 1 << 30;
        const MMC_VOLTAGE_MASK: u32 = 0x00FF_8000;
        const MMC_ACCESS_MODE_MASK: u32 = 0x6000_0000;

        let first = match self.host.send_command(&crate::cmd::cmd1(0))? {
            Response::R3(ocr) => {
                info!("sdio: CMD1 initial ocr={:#010x}", ocr.raw);
                ocr
            }
            _ => return Err(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 1))),
        };
        if first.card_powered_up() {
            return Ok(first);
        }
        let voltage = first.raw & MMC_VOLTAGE_MASK;
        let voltage = if voltage == 0 {
            MMC_VOLTAGE_MASK
        } else {
            voltage
        };
        let ocr_arg = MMC_HCS | voltage | (first.raw & MMC_ACCESS_MODE_MASK);
        let mut elapsed = 0u32;
        loop {
            debug!("sdio: CMD1 arg={:#010x} elapsed={}ms", ocr_arg, elapsed);
            let cmd1 = crate::cmd::cmd1(ocr_arg);
            match self.host.send_command(&cmd1)? {
                Response::R3(ocr) => {
                    debug!(
                        "sdio: CMD1 ocr={:#010x} ready={}",
                        ocr.raw,
                        ocr.card_powered_up()
                    );
                    if ocr.card_powered_up() {
                        return Ok(ocr);
                    }
                }
                _ => return Err(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 1))),
            }

            if elapsed >= Self::INIT_TIMEOUT_MS {
                warn!("sdio: CMD1 timed out after {}ms", elapsed);
                return Err(Error::Timeout(ErrorContext::for_cmd(Phase::Init, 1)));
            }
            self.delay.delay_ms(Self::INIT_POLL_MS);
            elapsed = elapsed.saturating_add(Self::INIT_POLL_MS);
        }
    }

    /// CMD3 (SD): card publishes its RCA via R6.
    fn get_rca_sd(&mut self) -> Result<u16, Error> {
        match self.host.send_command(&crate::cmd::CMD3_SD)? {
            Response::R6(resp) => Ok(resp.rca()),
            _ => Err(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 3))),
        }
    }

    /// CMD3 (MMC): host assigns the RCA, card returns R1.
    fn assign_rca_mmc(&mut self, rca: u16) -> Result<u16, Error> {
        let cmd = crate::cmd::cmd3_mmc(rca);
        match self.host.send_command(&cmd)? {
            Response::R1(_) => Ok(rca),
            _ => Err(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 3))),
        }
    }

    /// Read the 512-byte EXT_CSD via MMC `CMD8 SEND_EXT_CSD`.
    ///
    /// Caller must have selected the card (CMD7) before this. The data
    /// phase is read-only; the host is informed via
    /// [`SdioHost::read_data`] so the controller can set up the right
    /// transfer mode before issuing the command.
    fn read_ext_csd(&mut self) -> Result<crate::ext_csd::ExtCsd, Error> {
        info!("sdio: prepare EXT_CSD read");
        let mut buf = [0u8; 512];
        let _r1 = self
            .host
            .read_data(&crate::cmd::CMD8_MMC, &mut buf, 512, 1)?;
        Ok(crate::ext_csd::ExtCsd::from_bytes(buf))
    }

    /// Issue MMC `CMD6 SWITCH` to write `value` into EXT_CSD byte
    /// `index`, then poll CMD13 until the card returns to `tran` state.
    ///
    /// `access` selects the SWITCH access mode (`0b11` = WRITE_BYTE,
    /// `0b10` = SET_BITS, `0b01` = CLEAR_BITS). For most use cases —
    /// flipping a single byte to a known value — `WRITE_BYTE` is what
    /// you want; this is what [`mmc_switch_write_byte`](Self::mmc_switch_write_byte)
    /// exposes.
    ///
    /// Surfaces `Error::CardError(IllegalCommand)` if the post-switch
    /// CMD13 reports `SWITCH_ERROR`.
    fn mmc_switch(&mut self, access: u8, index: u8, value: u8) -> Result<(), Error> {
        let cmd = crate::cmd::cmd6_mmc_switch(access, index, value);
        self.host.send_command(&cmd)?;

        // Poll CMD13 for busy clear + SWITCH_ERROR check. The MMC spec
        // bounds programming time at 100 ms in the worst case; we're a
        // little more generous (250 ms) and use the same poll cadence
        // as ACMD41 to keep the bring-up path simple.
        const SWITCH_TIMEOUT_MS: u32 = 250;
        let mut elapsed = 0u32;
        loop {
            let r = self.host.send_command(&crate::cmd::cmd13(self.rca))?;
            if let Response::R1(r1) = r {
                if r1.switch_error() {
                    warn!("sdio: SWITCH_ERROR after CMD6 idx={} val={}", index, value);
                    return Err(Error::CardError(crate::error::CardError::IllegalCommand));
                }
                if r1.ready_for_data()
                    && matches!(r1.current_state(), crate::response::CardState::Transfer)
                {
                    return Ok(());
                }
            }
            if elapsed >= SWITCH_TIMEOUT_MS {
                return Err(Error::Timeout(ErrorContext::for_cmd(Phase::Init, 6)));
            }
            self.delay.delay_ms(Self::INIT_POLL_MS);
            elapsed = elapsed.saturating_add(Self::INIT_POLL_MS);
        }
    }

    /// Convenience over [`mmc_switch`](Self::mmc_switch) using the most
    /// common access mode (`WRITE_BYTE`).
    fn mmc_switch_write_byte(&mut self, index: u8, value: u8) -> Result<(), Error> {
        self.mmc_switch(0b11, index, value)
    }

    /// Switch the eMMC bus width through CMD6 SWITCH (writes
    /// `EXT_CSD.BUS_WIDTH`) after the host accepts the requested width.
    fn set_bus_width_mmc(&mut self, width: BusWidth) -> Result<(), Error> {
        let value: u8 = match width {
            BusWidth::Bit1 => 0,
            BusWidth::Bit4 => 1,
            BusWidth::Bit8 => 2,
        };
        self.host.set_bus_width(width)?;
        self.mmc_switch_write_byte(crate::cmd::ext_csd::BUS_WIDTH as u8, value)?;
        self.bus_width = width;
        Ok(())
    }

    /// Try to bring the eMMC up in HS200 mode at the current bus width.
    ///
    /// Sequence (JEDEC eMMC 5.0 §6.6.4):
    ///
    /// 1. **Voltage**: ensure the bus is at 1.8 V. Most platforms wire
    ///    eMMC permanently at 1.8 V; we still call `switch_voltage` so
    ///    hosts that *do* manage the IO domain can flip it. eMMC has no
    ///    CMD11 — the protocol layer skips it, only the host moves.
    /// 2. **Timing select**: write `EXT_CSD.HS_TIMING = 0x02`
    ///    (low nibble = HS200, upper nibble = driver strength 0). After
    ///    this write the card immediately drives the bus at HS200
    ///    timings, so the host clock must follow within the spec's
    ///    transition window.
    /// 3. **Clock**: `host.set_clock(Hs200)` — bring the controller to
    ///    200 MHz with HS200 IO timing.
    /// 4. **Tuning**: `host.execute_tuning(21)` — the controller
    ///    iterates CMD21 to find a stable sampling phase.
    /// 5. **Verify**: CMD13 must report `tran` + READY_FOR_DATA so we
    ///    know the card is responsive at the new timings.
    ///
    /// Any step returning an error rolls the speed back to default
    /// (the caller then drops to HS @ 52 MHz). `width` must already be
    /// 4-bit or 8-bit — HS200 is undefined for 1-bit.
    fn try_hs200(&mut self, width: BusWidth) -> Result<(), Error> {
        if matches!(width, BusWidth::Bit1) {
            return Err(Error::UnsupportedCommand);
        }

        // 1. Ask the host to make sure we're at 1.8 V. Hosts that hard-wire
        //    1.8 V can no-op; hosts without IO-domain control return
        //    UnsupportedCommand and we abort HS200.
        match self.host.switch_voltage(SignalVoltage::V180) {
            Ok(()) => {}
            Err(Error::UnsupportedCommand) => {
                // Caller (the SoC integrator) is on the hook to have
                // wired eMMC at 1.8 V at boot. We can't verify, so just
                // proceed — if the IO domain is still 3.3 V, tuning will
                // fail and we'll roll back.
                debug!("sdio: switch_voltage(V180) unsupported, proceeding");
            }
            Err(e) => {
                debug!("sdio: switch_voltage(V180) failed ({:?})", e);
                return Err(e);
            }
        }

        // 2. EXT_CSD.HS_TIMING = 0x02 (HS200 in low nibble, drv str 0).
        self.mmc_switch_write_byte(crate::cmd::ext_csd::HS_TIMING as u8, 0x02)?;

        // 3. Spin the controller up to 200 MHz, then run tuning. On
        //    failure of either step we have to roll the card back to
        //    a safe timing — HS_TIMING=2 means it's expecting HS200
        //    timings, which won't work if the controller couldn't tune
        //    in. Roll the card back to HS @ 52 MHz timing first, then
        //    drop the controller clock with it; the outer `init` will
        //    repeat the HS_TIMING=1 write through the HS-fallback
        //    branch but writing the same value twice is harmless.
        if let Err(e) = self.host.set_clock(ClockSpeed::Hs200) {
            self.rollback_to_hs_compat();
            return Err(e);
        }
        if let Err(e) = self.host.execute_tuning(21) {
            self.rollback_to_hs_compat();
            return Err(e);
        }

        // 4. Verify the card actually came along — a successful tuning
        //    plus CMD13 returning `tran` is the canonical "we're in
        //    HS200" check.
        let r = self.host.send_command(&crate::cmd::cmd13(self.rca))?;
        if let Response::R1(r1) = r
            && r1.ready_for_data()
            && matches!(r1.current_state(), crate::response::CardState::Transfer)
        {
            info!("sdio: HS200 entry succeeded");
            return Ok(());
        }
        self.rollback_to_hs_compat();
        Err(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 13)))
    }

    /// Best-effort rollback after a failed HS200 attempt. Drops the
    /// controller clock back to default speed; the outer `init` will
    /// then re-program HS_TIMING=1 + HighSpeed in its fallback branch.
    /// Errors are deliberately swallowed — we're already on the error
    /// path and want to give the rest of `init` the best shot at
    /// recovering.
    fn rollback_to_hs_compat(&mut self) {
        let _ = self.host.set_clock(ClockSpeed::Default);
    }

    /// Switch SD bus width via ACMD6. eMMC must use CMD6 SWITCH instead;
    /// see the discussion above [`SdioSdmmc::init`].
    fn set_bus_width_sd(&mut self, width: BusWidth) -> Result<(), Error> {
        // CMD55
        let cmd55 = crate::cmd::cmd55(self.rca);
        self.host.send_command(&cmd55)?;

        // ACMD6: set bus width — ACMD6 only encodes 1-bit or 4-bit. 8-bit
        // bus configuration on eMMC is done through MMC CMD6 (SWITCH) and is
        // not part of this driver's ACMD6 path.
        let arg = match width {
            BusWidth::Bit1 => 0,
            BusWidth::Bit4 => 2,
            BusWidth::Bit8 => return Err(Error::UnsupportedCommand),
        };
        let acmd6 = Command::new(6, arg, ResponseType::R1);
        self.host.send_command(&acmd6)?;

        self.host.set_bus_width(width)?;
        self.bus_width = width;
        Ok(())
    }

    fn try_sd_best_speed(&mut self, ocr: OcrResponse) {
        match self.try_sd_best_speed_inner(ocr) {
            Ok(Some(speed)) => info!("sdio: SD speed selected {:?}", speed),
            Ok(None) => info!("sdio: SD card stayed at default speed"),
            Err(e) => warn!("sdio: SD speed selection skipped ({:?})", e),
        }
    }

    fn try_sd_best_speed_inner(&mut self, ocr: OcrResponse) -> Result<Option<ClockSpeed>, Error> {
        let status = self.switch_function(&crate::cmd::cmd6_sd_access_mode(false, 0))?;
        info!(
            "sdio: SD access mode support hs={} sdr50={} sdr104={} ddr50={} s18a={}",
            status.access_mode_supported(SdAccessMode::HighSpeed.function()),
            status.access_mode_supported(SdAccessMode::Sdr50.function()),
            status.access_mode_supported(SdAccessMode::Sdr104.function()),
            status.access_mode_supported(SdAccessMode::Ddr50.function()),
            ocr.s18a()
        );

        if ocr.s18a() {
            for candidate in [
                SdAccessMode::Sdr104,
                SdAccessMode::Sdr50,
                SdAccessMode::Ddr50,
            ] {
                if !status.access_mode_supported(candidate.function()) {
                    continue;
                }
                info!("sdio: trying SD {}", candidate.name());
                match self.try_sd_uhs_mode(candidate) {
                    Ok(()) => return Ok(Some(candidate.clock())),
                    Err(e) => warn!("sdio: SD {} failed ({:?})", candidate.name(), e),
                }
            }
        } else {
            info!("sdio: SD UHS-I skipped because ACMD41 did not report S18A");
        }

        if status.access_mode_supported(SdAccessMode::HighSpeed.function()) {
            info!("sdio: trying SD HighSpeed");
            match self.try_sd_access_mode(SdAccessMode::HighSpeed) {
                Ok(()) => return Ok(Some(ClockSpeed::HighSpeed)),
                Err(e) => warn!("sdio: SD HighSpeed failed ({:?})", e),
            }
        } else {
            info!("sdio: SD HighSpeed unsupported by CMD6 status");
        }

        Ok(None)
    }

    fn try_sd_uhs_mode(&mut self, mode: SdAccessMode) -> Result<(), Error> {
        self.host.send_command(&crate::cmd::CMD11)?;
        self.host.switch_voltage(SignalVoltage::V180)?;
        self.try_sd_access_mode(mode)?;
        if mode.needs_tuning() {
            self.host.execute_tuning(19)?;
        }
        Ok(())
    }

    fn try_sd_access_mode(&mut self, mode: SdAccessMode) -> Result<(), Error> {
        let status =
            self.switch_function(&crate::cmd::cmd6_sd_access_mode(true, mode.function()))?;
        if status.selected_function(1) != mode.function() {
            return Err(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 6)));
        }
        self.host.set_clock(mode.clock())?;
        let status = self.status()?;
        if matches!(status, CardState::Transfer) {
            Ok(())
        } else {
            Err(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 13)))
        }
    }

    // ── Data Transfer ───────────────────────────────────────────

    /// Read a single 512-byte block
    pub fn read_block(&mut self, addr: u32, buf: &mut [u8; 512]) -> Result<(), Error> {
        let block_addr = block_addr_of(addr, self.high_capacity);
        let cmd = crate::cmd::cmd17(block_addr);
        self.host.read_data(&cmd, buf, 512, 1)?;
        Ok(())
    }

    /// Write a single 512-byte block
    pub fn write_block(&mut self, addr: u32, buf: &[u8; 512]) -> Result<(), Error> {
        let block_addr = block_addr_of(addr, self.high_capacity);
        let cmd = crate::cmd::cmd24(block_addr);
        self.host.write_data(&cmd, buf, 512, 1)?;
        Ok(())
    }

    /// Read multiple blocks
    pub fn read_blocks<F>(&mut self, addr: u32, count: u32, mut handler: F) -> Result<(), Error>
    where
        F: FnMut(u32, &[u8; 512]),
    {
        let mut buf = [0u8; 512];
        for i in 0..count {
            self.read_block(addr + i, &mut buf)?;
            handler(addr + i, &buf);
        }
        Ok(())
    }

    /// Read one or more 512-byte blocks into a contiguous caller buffer.
    ///
    /// `buf.len()` must be a non-zero multiple of 512 bytes. For multi-block
    /// reads the complete buffer is handed to the host in one data phase, so
    /// host backends can use a single PIO/DMA setup instead of bouncing
    /// through a temporary `[u8; 512]` per block.
    pub fn read_blocks_into(&mut self, addr: u32, buf: &mut [u8]) -> Result<(), Error> {
        let count = block_count_from_len(buf.len())?;
        let block_addr = block_addr_of(addr, self.high_capacity);
        let cmd = if count == 1 {
            crate::cmd::cmd17(block_addr)
        } else {
            crate::cmd::cmd18(block_addr)
        };
        self.host.read_data(&cmd, buf, 512, count)?;
        if count > 1 {
            self.host.send_command(&crate::cmd::CMD12)?;
        }
        Ok(())
    }

    /// Write multiple blocks
    pub fn write_blocks(&mut self, addr: u32, blocks: &[[u8; 512]]) -> Result<(), Error> {
        if blocks.is_empty() {
            return Err(Error::InvalidArgument);
        }
        if blocks.len() == 1 {
            return self.write_block(addr, &blocks[0]);
        }

        let block_addr = block_addr_of(addr, self.high_capacity);
        let count = blocks.len() as u32;
        let cmd = crate::cmd::cmd25(block_addr);
        let buf = blocks.as_flattened();
        self.host.write_data(&cmd, buf, 512, count)?;
        self.host.send_command(&crate::cmd::CMD12)?;
        Ok(())
    }

    /// Write one or more 512-byte blocks from a contiguous caller buffer.
    ///
    /// `buf.len()` must be a non-zero multiple of 512 bytes. For multi-block
    /// writes the complete buffer is handed to the host in one data phase.
    pub fn write_blocks_from(&mut self, addr: u32, buf: &[u8]) -> Result<(), Error> {
        let count = block_count_from_len(buf.len())?;
        let block_addr = block_addr_of(addr, self.high_capacity);
        let cmd = if count == 1 {
            crate::cmd::cmd24(block_addr)
        } else {
            crate::cmd::cmd25(block_addr)
        };
        self.host.write_data(&cmd, buf, 512, count)?;
        if count > 1 {
            self.host.send_command(&crate::cmd::CMD12)?;
        }
        Ok(())
    }

    /// Erase a range of blocks
    pub fn erase(&mut self, start: u32, end: u32) -> Result<(), Error> {
        let start_addr = block_addr_of(start, self.high_capacity);
        let end_addr = block_addr_of(end, self.high_capacity);

        let cmd32 = crate::cmd::cmd32(start_addr);
        self.host.send_command(&cmd32)?;

        let cmd33 = crate::cmd::cmd33(end_addr);
        self.host.send_command(&cmd33)?;

        self.host.send_command(&crate::cmd::CMD38)?;
        Ok(())
    }

    /// Get card status
    pub fn status(&mut self) -> Result<CardState, Error> {
        let cmd13 = crate::cmd::cmd13(self.rca);
        match self.host.send_command(&cmd13)? {
            Response::R1(r1) => Ok(r1.current_state()),
            _ => Err(Error::BadResponse(ErrorContext::for_cmd(
                Phase::ResponseWait,
                13,
            ))),
        }
    }

    /// Issue a CMD6 SWITCH_FUNC and read back the 64-byte status block.
    ///
    /// Use [`SdioSdmmc::switch_to_high_speed`] for the most common case
    /// (group 1 → high-speed). This lower-level entry point exposes the
    /// raw [`SwitchStatus`] for callers that need to inspect other groups.
    pub fn switch_function(&mut self, cmd: &Command) -> Result<SwitchStatus, Error> {
        let mut buf = [0u8; 64];
        self.host.read_data(cmd, &mut buf, 64, 1)?;
        Ok(SwitchStatus::from_raw(buf))
    }

    /// Switch the card to high-speed (50 MHz) by issuing CMD6 with mode=1
    /// and group 1 = 1. Returns `Ok(true)` if the card reports high-speed
    /// active; `Ok(false)` if it acknowledged the command but didn't switch
    /// (e.g. unsupported); `Err` if the bus transaction itself failed.
    ///
    /// The host is responsible for actually raising the bus clock after this
    /// returns success; this driver only handles the protocol-level switch.
    pub fn switch_to_high_speed(&mut self) -> Result<bool, Error> {
        let status = self.switch_function(&crate::cmd::cmd6_high_speed(true))?;
        let active = status.high_speed_active();
        if active {
            info!("sdio: switched to high-speed mode");
        } else {
            warn!("sdio: high-speed switch did not take effect");
        }
        Ok(active)
    }
}

fn block_count_from_len(len: usize) -> Result<u32, Error> {
    if len == 0 || !len.is_multiple_of(512) {
        return Err(Error::Misaligned);
    }
    u32::try_from(len / 512).map_err(|_| Error::InvalidArgument)
}

/// Card information obtained during SDIO initialization
#[derive(Debug, Clone)]
pub struct CardInfo {
    /// Which physical-layer protocol the card speaks. SD vs eMMC matters
    /// for follow-up steps the protocol layer can't generalize over —
    /// e.g. EXT_CSD reads, 8-bit bus switching, HS200 tuning.
    pub kind: CardKind,
    /// True when the card responded to CMD8 with a valid R7 echo
    /// (SD physical layer 2.0+). Always `false` for eMMC.
    pub sd_v2: bool,
    pub high_capacity: bool,
    pub ocr: u32,
    pub rca: u16,
    /// User-data capacity in 512-byte blocks, parsed from the CSD.
    /// `None` if the CSD reports a structure version we do not yet support.
    pub capacity_blocks: Option<u64>,
    /// Card identification register (manufacturer / OEM / serial / date).
    /// `None` if the host returned an unexpected response type to CMD2.
    pub cid: Option<CidResponse>,
    /// Decoded EXT_CSD register, present only for [`CardKind::Mmc`]
    /// after a successful init. Lets callers introspect HS200/HS400
    /// support, partition geometry, etc., without re-reading the card.
    pub ext_csd: Option<crate::ext_csd::ExtCsd>,
}

/// Which physical-layer family the card belongs to.
///
/// The SD vs MMC split is decided during `init()`:
///
/// - CMD8 echoes a valid R7 → SD v2 (SDHC/SDXC)
/// - CMD8 has no response, but ACMD41 succeeds → SD v1 (legacy SDSC)
/// - CMD8 has no response and ACMD41 also fails, but CMD1 reports
///   power-up → eMMC / MMC
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardKind {
    /// SD memory card (SDSC / SDHC / SDXC).
    Sd,
    /// Embedded MMC or removable MMC card.
    Mmc,
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec::Vec;

    use super::*;
    use crate::response::{IfCondResponse, OcrResponse, R1Response, RcaResponse};

    struct NullDelay;

    impl DelayNs for NullDelay {
        fn delay_ns(&mut self, _ns: u32) {}
    }

    /// Mock host that replays canned responses in order. Used to verify the
    /// init sequence and that the driver tracks RCA on its own.
    struct MockHost {
        replies: Vec<Result<Response, Error>>,
        commands: Vec<Command>,
        bus_width: Option<BusWidth>,
        prepared_transfers: Vec<(DataDirection, u32, u32)>,
        next_read_payload: Option<Vec<u8>>,
        read_payloads: Vec<Vec<u8>>,
        writes: Vec<Vec<u8>>,
        /// When set, `set_bus_width(Bit8)` returns `UnsupportedCommand`
        /// to mimic a host (e.g. the SDHCI MVP backend) that hasn't
        /// wired up 8-bit operation yet.
        reject_bit8: bool,
        /// Last clock the protocol layer asked for. Lets HS200 tests
        /// confirm the host was driven up to 200 MHz.
        last_clock: Option<ClockSpeed>,
        /// Last voltage the protocol layer asked for. `None` means the
        /// driver never called `switch_voltage`.
        last_voltage: Option<SignalVoltage>,
        /// When `Some`, `switch_voltage` returns this error instead of
        /// succeeding. `Some(UnsupportedCommand)` exercises the
        /// "host has eMMC hard-wired at 1.8 V" path.
        voltage_switch_result: Option<Error>,
        /// When `Some`, `execute_tuning` returns this error. Lets the
        /// HS200-fallback test simulate a controller that can't tune.
        tuning_result: Option<Error>,
        /// Records the cmd_index passed to the most recent
        /// `execute_tuning` call.
        last_tuning_cmd: Option<u8>,
    }

    impl MockHost {
        fn new(replies: Vec<Response>) -> Self {
            Self {
                replies: replies.into_iter().map(Ok).collect(),
                commands: Vec::new(),
                bus_width: None,
                prepared_transfers: Vec::new(),
                next_read_payload: None,
                read_payloads: Vec::new(),
                writes: Vec::new(),
                reject_bit8: false,
                last_clock: None,
                last_voltage: None,
                voltage_switch_result: None,
                tuning_result: None,
                last_tuning_cmd: None,
            }
        }

        /// Build a host where any response slot can be a synthesized
        /// error (e.g. a CMD8 timeout to simulate an eMMC card).
        fn with_results(replies: Vec<Result<Response, Error>>) -> Self {
            Self {
                replies,
                commands: Vec::new(),
                bus_width: None,
                prepared_transfers: Vec::new(),
                next_read_payload: None,
                read_payloads: Vec::new(),
                writes: Vec::new(),
                reject_bit8: false,
                last_clock: None,
                last_voltage: None,
                voltage_switch_result: None,
                tuning_result: None,
                last_tuning_cmd: None,
            }
        }
    }

    impl SdioHost for MockHost {
        fn send_command(&mut self, cmd: &Command) -> Result<Response, Error> {
            self.commands.push(*cmd);
            if self.replies.is_empty() {
                return Err(Error::Timeout(ErrorContext::default()));
            }
            self.replies.remove(0)
        }

        fn read_data(
            &mut self,
            cmd: &Command,
            buf: &mut [u8],
            block_size: u32,
            block_count: u32,
        ) -> Result<Response, Error> {
            self.prepared_transfers
                .push((DataDirection::Read, block_size, block_count));
            let response = self.send_command(cmd)?;
            let payload = if self.read_payloads.is_empty() {
                self.next_read_payload.take()
            } else {
                Some(self.read_payloads.remove(0))
            };
            match payload {
                Some(data) if data.len() == buf.len() => {
                    buf.copy_from_slice(&data);
                    Ok(response)
                }
                _ => Err(Error::UnsupportedCommand),
            }
        }

        fn write_data(
            &mut self,
            cmd: &Command,
            buf: &[u8],
            block_size: u32,
            block_count: u32,
        ) -> Result<Response, Error> {
            self.prepared_transfers
                .push((DataDirection::Write, block_size, block_count));
            let response = self.send_command(cmd)?;
            self.writes.push(buf.to_vec());
            Ok(response)
        }

        fn set_bus_width(&mut self, width: BusWidth) -> Result<(), Error> {
            if self.reject_bit8 && matches!(width, BusWidth::Bit8) {
                return Err(Error::UnsupportedCommand);
            }
            self.bus_width = Some(width);
            Ok(())
        }

        fn set_clock(&mut self, speed: ClockSpeed) -> Result<(), Error> {
            self.last_clock = Some(speed);
            Ok(())
        }

        fn prepare_data_transfer(
            &mut self,
            direction: DataDirection,
            block_size: u32,
            block_count: u32,
        ) -> Result<(), Error> {
            self.prepared_transfers
                .push((direction, block_size, block_count));
            Ok(())
        }

        fn switch_voltage(&mut self, v: SignalVoltage) -> Result<(), Error> {
            self.last_voltage = Some(v);
            if let Some(e) = self.voltage_switch_result {
                return Err(e);
            }
            Ok(())
        }

        fn execute_tuning(&mut self, cmd_index: u8) -> Result<(), Error> {
            self.last_tuning_cmd = Some(cmd_index);
            if let Some(e) = self.tuning_result {
                return Err(e);
            }
            Ok(())
        }
    }

    fn ok_r1() -> Response {
        Response::R1(R1Response::from_native_raw(0).unwrap())
    }

    fn rca_response(rca: u16) -> Response {
        Response::R6(RcaResponse::from_raw((rca as u32) << 16))
    }

    fn ocr_ready_sdhc() -> Response {
        // bit 31 = power-up done, bit 30 = CCS (high capacity)
        Response::R3(OcrResponse::from_raw(0xC0FF_8000))
    }

    fn ocr_ready_sdhc_s18a() -> Response {
        // bit 31 = power-up done, bit 30 = CCS, bit 24 = S18A
        Response::R3(OcrResponse::from_raw(0xC1FF_8000))
    }

    fn csd_v2_response() -> Response {
        let mut raw = [0u8; 16];
        raw[0] = 0x40;
        raw[7] = 0x00;
        raw[8] = 0x0F;
        raw[9] = 0x0F;
        Response::R2(raw)
    }

    fn cid_response() -> Response {
        let mut raw = [0u8; 16];
        raw[0] = 0x03;
        raw[1] = b'S';
        raw[2] = b'D';
        raw[3] = b'A';
        raw[4] = b'B';
        raw[5] = b'C';
        raw[6] = b'1';
        raw[7] = b'2';
        Response::R2(raw)
    }

    fn sd_init_replies() -> Vec<Result<Response, Error>> {
        sd_init_replies_with_ocr(ocr_ready_sdhc())
    }

    fn sd_init_replies_with_ocr(ocr: Response) -> Vec<Result<Response, Error>> {
        std::vec![
            Ok(ok_r1()),                                             // CMD0
            Ok(Response::R7(IfCondResponse::from_raw(0x0000_01AA))), // CMD8
            Ok(ok_r1()),                                             // CMD55 (ACMD41 prologue)
            Ok(ocr),                                                 // ACMD41
            Ok(cid_response()),                                      // CMD2
            Ok(rca_response(0x1234)),                                // CMD3
            Ok(csd_v2_response()),                                   // CMD9
            Ok(ok_r1()),                                             // CMD7 (select)
            Ok(ok_r1()),                                             // CMD55 (ACMD6 prologue)
            Ok(ok_r1()),                                             // ACMD6
        ]
    }

    fn switch_status_payload(function: u8, supported: u8) -> Vec<u8> {
        let mut status = std::vec![0u8; 64];
        status[13] = supported;
        status[16] = function & 0x0f;
        status
    }

    #[test]
    fn init_records_rca_in_driver_state() {
        let replies = sd_init_replies();
        let host = MockHost::with_results(replies);
        let mut driver = SdioSdmmc::new(host, NullDelay);
        let info = driver.init().unwrap();

        assert_eq!(info.rca, 0x1234);
        assert_eq!(driver.rca(), 0x1234);
        assert!(info.high_capacity);
        assert_eq!(info.kind, CardKind::Sd);
        assert_eq!(info.capacity_blocks, Some((0x0F0F + 1) * 1024));
        let cid = info.cid.expect("CID captured in init");
        assert_eq!(cid.manufacturer_id(), 0x03);
        assert_eq!(&cid.product_name(), b"ABC12");
        assert_eq!(driver.host.bus_width, Some(BusWidth::Bit4));

        // Verify CMD7 / CMD55 / ACMD6 used the recorded RCA, not 0.
        let cmd7 = driver
            .host
            .commands
            .iter()
            .find(|c| c.cmd == 7)
            .expect("CMD7 issued");
        assert_eq!(cmd7.arg, (0x1234u32) << 16);
    }

    #[test]
    fn sd_init_automatically_selects_sdr104_when_card_and_host_agree() {
        let mut replies = sd_init_replies_with_ocr(ocr_ready_sdhc_s18a());
        replies.extend([
            Ok(ok_r1()),         // CMD6 query access modes
            Ok(ok_r1()),         // CMD11 voltage switch command
            Ok(ok_r1()),         // CMD6 switch SDR104
            Ok(r1_tran_ready()), // CMD13 verify
        ]);
        let mut host = MockHost::with_results(replies);
        host.read_payloads = std::vec![
            switch_status_payload(0, 1 << 3),
            switch_status_payload(3, 1 << 3),
        ];

        let mut driver = SdioSdmmc::new(host, NullDelay);
        driver.init().expect("SD init succeeds with SDR104");

        assert_eq!(driver.host.last_voltage, Some(SignalVoltage::V180));
        assert_eq!(driver.host.last_clock, Some(ClockSpeed::Sdr104));
        assert_eq!(driver.host.last_tuning_cmd, Some(19));
        assert!(
            driver.host.commands.iter().any(|c| c.cmd == 11),
            "CMD11 issued before host voltage switch"
        );
        assert!(
            driver
                .host
                .commands
                .iter()
                .any(|c| c.cmd == 6 && c.arg == 0x80FF_FFF3),
            "CMD6 switched group 1 to SDR104"
        );
    }

    #[test]
    fn sd_init_falls_back_to_high_speed_when_uhs_voltage_switch_fails() {
        let mut replies = sd_init_replies_with_ocr(ocr_ready_sdhc_s18a());
        replies.extend([
            Ok(ok_r1()),         // CMD6 query access modes
            Ok(ok_r1()),         // CMD11 voltage switch command
            Ok(ok_r1()),         // CMD6 switch HighSpeed
            Ok(r1_tran_ready()), // CMD13 verify
        ]);
        let mut host = MockHost::with_results(replies);
        host.read_payloads = std::vec![
            switch_status_payload(0, (1 << 3) | (1 << 1)),
            switch_status_payload(1, 1 << 1),
        ];
        host.voltage_switch_result = Some(Error::UnsupportedCommand);

        let mut driver = SdioSdmmc::new(host, NullDelay);
        driver
            .init()
            .expect("SD init falls back when UHS voltage switch fails");

        assert_eq!(driver.host.last_voltage, Some(SignalVoltage::V180));
        assert_eq!(driver.host.last_clock, Some(ClockSpeed::HighSpeed));
        assert_eq!(driver.host.last_tuning_cmd, None);
        assert!(
            driver
                .host
                .commands
                .iter()
                .any(|c| c.cmd == 6 && c.arg == 0x80FF_FFF1),
            "CMD6 switched group 1 to HighSpeed after UHS fallback"
        );
    }

    #[test]
    fn sd_speed_selection_can_be_disabled_for_default_speed_bringup() {
        let replies = sd_init_replies_with_ocr(ocr_ready_sdhc_s18a());
        let host = MockHost::with_results(replies);
        let mut driver = SdioSdmmc::new(host, NullDelay);
        driver.set_sd_speed_selection_enabled(false);

        driver
            .init()
            .expect("SD init succeeds without CMD6 speed switching");

        assert_eq!(driver.host.bus_width, Some(BusWidth::Bit4));
        assert_eq!(driver.host.last_clock, Some(ClockSpeed::Default));
        assert!(
            driver
                .host
                .commands
                .iter()
                .filter(|c| c.cmd == 6)
                .all(|c| c.arg == 2),
            "only ACMD6 bus-width switch is issued; no CMD6 SWITCH_FUNC"
        );
        assert_eq!(driver.host.last_voltage, None);
        assert_eq!(driver.host.last_tuning_cmd, None);
    }

    fn ocr_ready_mmc_sector() -> Response {
        // bit 31 = power-up done, bit 30 = sector mode (high capacity)
        Response::R3(OcrResponse::from_raw(0xC0FF_8000))
    }

    fn cmd8_timeout() -> Result<Response, Error> {
        Err(Error::Timeout(ErrorContext::for_cmd(Phase::CommandSend, 8)))
    }

    fn acmd41_timeout() -> Result<Response, Error> {
        Err(Error::Timeout(ErrorContext::for_cmd(
            Phase::CommandSend,
            41,
        )))
    }

    /// CMD13 R1 with `READY_FOR_DATA` set and the card in `tran` state.
    /// What `mmc_switch` polls for after a CMD6 SWITCH.
    fn r1_tran_ready() -> Response {
        // bit 8 = READY_FOR_DATA, bits 12..9 = 4 (Transfer)
        Response::R1(R1Response::from_native_raw((1 << 8) | (4 << 9)).unwrap())
    }

    /// Build an EXT_CSD payload that advertises 8-bit, HS @ 52 MHz, and
    /// a sector count.
    fn ext_csd_blob() -> Vec<u8> {
        use crate::cmd::ext_csd as e;
        let mut buf = std::vec![0u8; 512];
        // SEC_COUNT = 0x0080_0000 (4 GiB) little-endian
        buf[e::SEC_COUNT] = 0x00;
        buf[e::SEC_COUNT + 1] = 0x00;
        buf[e::SEC_COUNT + 2] = 0x80;
        buf[e::SEC_COUNT + 3] = 0x00;
        // DEVICE_TYPE = HS_26 | HS_52
        buf[e::DEVICE_TYPE] = e::device_type::HS_26 | e::device_type::HS_52;
        // Currently selected: 1-bit, compat (matches reset state)
        buf[e::BUS_WIDTH] = 0;
        buf[e::HS_TIMING] = 0;
        buf
    }

    #[test]
    fn init_falls_back_to_mmc_when_cmd8_and_acmd41_fail() {
        // Canonical eMMC bring-up: CMD8 returns nothing (host reports
        // timeout), ACMD41 also fails (eMMC ignores it), then CMD1 takes
        // over and reports the card ready immediately. After CMD7 the
        // driver reads EXT_CSD, then issues CMD6 SWITCH twice (8-bit
        // bus width, HS_TIMING=1) — each followed by CMD13 polling for
        // tran state.
        let replies = std::vec![
            Ok(ok_r1()),                // CMD0
            cmd8_timeout(),             // CMD8 — eMMC ignores
            Ok(ok_r1()),                // CMD55 (ACMD41 prologue)
            acmd41_timeout(),           // ACMD41 — eMMC ignores
            Ok(ocr_ready_mmc_sector()), // CMD1 — card reports ready
            Ok(cid_response()),         // CMD2
            Ok(ok_r1()),                // CMD3 (host-assigned RCA, R1 ack)
            Ok(csd_v2_response()),      // CMD9
            Ok(ok_r1()),                // CMD7 (select)
            Ok(ok_r1()),                // CMD8 MMC SEND_EXT_CSD — R1 (data follows)
            Ok(ok_r1()),                // CMD6 SWITCH — BUS_WIDTH=2 (8-bit)
            Ok(r1_tran_ready()),        // CMD13 — tran + ready
            Ok(ok_r1()),                // CMD6 SWITCH — HS_TIMING=1
            Ok(r1_tran_ready()),        // CMD13 — tran + ready
        ];
        let mut host = MockHost::with_results(replies);
        host.next_read_payload = Some(ext_csd_blob());
        let mut driver = SdioSdmmc::new(host, NullDelay);
        let info = driver.init().expect("eMMC init succeeds");

        assert_eq!(info.kind, CardKind::Mmc);
        assert_eq!(driver.kind(), CardKind::Mmc);
        assert!(!info.sd_v2);
        assert!(info.high_capacity, "OCR bit 30 set → sector mode");
        assert_eq!(info.rca, 1);
        // Capacity should come from EXT_CSD.SEC_COUNT, not the legacy CSD.
        assert_eq!(info.capacity_blocks, Some(0x0080_0000));
        // EXT_CSD got captured.
        assert!(info.ext_csd.is_some());

        let cmds = &driver.host.commands;
        let cmd3 = cmds.iter().find(|c| c.cmd == 3).expect("CMD3 issued");
        assert_eq!(cmd3.arg, 1u32 << 16);
        assert!(cmds.iter().any(|c| c.cmd == 1), "CMD1 issued");

        // Two CMD6 SWITCHes — one for BUS_WIDTH, one for HS_TIMING.
        let cmd6s: Vec<&Command> = cmds.iter().filter(|c| c.cmd == 6).collect();
        assert_eq!(cmd6s.len(), 2, "two CMD6 SWITCHes (BUS_WIDTH + HS_TIMING)");
        // First: WRITE_BYTE | BUS_WIDTH(183) | value=2 (8-bit)
        let bw_arg = (0b11u32 << 24) | ((183u32) << 16) | (2u32 << 8);
        assert_eq!(cmd6s[0].arg, bw_arg, "BUS_WIDTH=8-bit");
        // Second: WRITE_BYTE | HS_TIMING(185) | value=1 (HS)
        let hs_arg = (0b11u32 << 24) | ((185u32) << 16) | (1u32 << 8);
        assert_eq!(cmd6s[1].arg, hs_arg, "HS_TIMING=1");

        // Host should have ended up at 8-bit (Bit8 was accepted).
        assert_eq!(driver.host.bus_width, Some(BusWidth::Bit8));
    }

    #[test]
    fn mmc_init_falls_back_to_4bit_when_host_refuses_8bit() {
        // Same as the canonical path but the host's set_bus_width
        // rejects Bit8. The driver must retry with Bit4 and end up
        // settled there, not silently leave the card at 8-bit.
        let replies = std::vec![
            Ok(ok_r1()),                // CMD0
            cmd8_timeout(),             // CMD8
            Ok(ok_r1()),                // CMD55
            acmd41_timeout(),           // ACMD41
            Ok(ocr_ready_mmc_sector()), // CMD1
            Ok(cid_response()),         // CMD2
            Ok(ok_r1()),                // CMD3
            Ok(csd_v2_response()),      // CMD9
            Ok(ok_r1()),                // CMD7
            Ok(ok_r1()),                // CMD8 MMC (R1)
            Ok(ok_r1()),                // CMD6 SWITCH (8-bit)
            Ok(r1_tran_ready()),        // CMD13 — tran (card *did* switch)
            // host.set_bus_width(Bit8) returns UnsupportedCommand, so the
            // driver retries with Bit4. No additional CMD6 needed for
            // the current implementation? Actually, yes — set_bus_width_mmc
            // re-issues CMD6 with BUS_WIDTH=1 first.
            Ok(ok_r1()),         // CMD6 SWITCH (4-bit)
            Ok(r1_tran_ready()), // CMD13 — tran
            Ok(ok_r1()),         // CMD6 SWITCH (HS_TIMING=1)
            Ok(r1_tran_ready()), // CMD13 — tran
        ];
        let mut host = MockHost::with_results(replies);
        host.next_read_payload = Some(ext_csd_blob());
        host.reject_bit8 = true;
        let mut driver = SdioSdmmc::new(host, NullDelay);
        let _info = driver
            .init()
            .expect("eMMC init succeeds with 4-bit fallback");

        assert_eq!(driver.host.bus_width, Some(BusWidth::Bit4));
    }

    #[test]
    fn init_treats_sd_v1_correctly_when_cmd8_times_out_but_acmd41_succeeds() {
        // SD v1 cards (legacy SDSC) don't recognize CMD8 either, but
        // *do* answer ACMD41. The driver must not promote them to MMC
        // just because CMD8 timed out.
        let replies = std::vec![
            Ok(ok_r1()),    // CMD0
            cmd8_timeout(), // CMD8 — SD v1 no echo
            Ok(ok_r1()),    // CMD55 (ACMD41 prologue)
            // bit 31 set, bit 30 clear → SDSC, ready
            Ok(Response::R3(OcrResponse::from_raw(0x80FF_8000))),
            Ok(cid_response()),       // CMD2
            Ok(rca_response(0x4321)), // CMD3 (R6, card picks)
            Ok(csd_v2_response()),    // CMD9
            Ok(ok_r1()),              // CMD7
            Ok(ok_r1()),              // CMD55 (ACMD6 prologue)
            Ok(ok_r1()),              // ACMD6
        ];
        let host = MockHost::with_results(replies);
        let mut driver = SdioSdmmc::new(host, NullDelay);
        let info = driver.init().expect("SD v1 init succeeds");

        assert_eq!(info.kind, CardKind::Sd, "ACMD41 success → SD, not MMC");
        assert!(!info.sd_v2);
        assert!(!info.high_capacity);
        assert_eq!(info.rca, 0x4321);
        assert_eq!(driver.host.bus_width, Some(BusWidth::Bit4));
    }

    /// Build an EXT_CSD payload that *also* advertises HS200 @ 1.8 V.
    fn ext_csd_blob_hs200() -> Vec<u8> {
        use crate::cmd::ext_csd as e;
        let mut buf = ext_csd_blob();
        // OR in HS200_18V on top of HS_26 | HS_52 already present.
        buf[e::DEVICE_TYPE] |= e::device_type::HS200_18V;
        buf
    }

    #[test]
    fn mmc_init_picks_hs200_when_card_and_host_agree() {
        // Sequence after CMD7:
        //   CMD8_MMC (R1) + 512B EXT_CSD
        //   CMD6 BUS_WIDTH=8 + CMD13 ready
        //   try_hs200:
        //     switch_voltage(V180)            ← host hook
        //     CMD6 HS_TIMING=0x02 + CMD13 ready
        //     set_clock(Hs200)                ← host hook
        //     execute_tuning(21)              ← host hook
        //     CMD13 ready (final verify)
        let replies = std::vec![
            Ok(ok_r1()),                // CMD0
            cmd8_timeout(),             // CMD8
            Ok(ok_r1()),                // CMD55
            acmd41_timeout(),           // ACMD41
            Ok(ocr_ready_mmc_sector()), // CMD1
            Ok(cid_response()),         // CMD2
            Ok(ok_r1()),                // CMD3
            Ok(csd_v2_response()),      // CMD9
            Ok(ok_r1()),                // CMD7
            Ok(ok_r1()),                // CMD8 MMC R1
            Ok(ok_r1()),                // CMD6 SWITCH BUS_WIDTH=8
            Ok(r1_tran_ready()),        // CMD13
            Ok(ok_r1()),                // CMD6 SWITCH HS_TIMING=2 (HS200)
            Ok(r1_tran_ready()),        // CMD13 (post-switch)
            Ok(r1_tran_ready()),        // CMD13 (HS200 verify)
        ];
        let mut host = MockHost::with_results(replies);
        host.next_read_payload = Some(ext_csd_blob_hs200());
        let mut driver = SdioSdmmc::new(host, NullDelay);
        let _info = driver.init().expect("HS200 init succeeds");

        // HS_TIMING write should carry value 0x02, not 0x01.
        let cmd6s: Vec<&Command> = driver.host.commands.iter().filter(|c| c.cmd == 6).collect();
        // Two CMD6: BUS_WIDTH(=2) and HS_TIMING(=2)
        assert_eq!(cmd6s.len(), 2);
        let hs_timing_arg = (0b11u32 << 24) | ((185u32) << 16) | (0x02u32 << 8);
        assert_eq!(cmd6s[1].arg, hs_timing_arg, "HS_TIMING=2 (HS200)");

        // Host hooks were exercised.
        assert_eq!(driver.host.last_voltage, Some(SignalVoltage::V180));
        assert_eq!(driver.host.last_clock, Some(ClockSpeed::Hs200));
        assert_eq!(driver.host.last_tuning_cmd, Some(21));
    }

    #[test]
    fn mmc_init_falls_back_to_hs52_when_tuning_fails() {
        // Card advertises HS200 + HS @ 52 MHz, but the host's
        // execute_tuning rejects (e.g. controller couldn't lock onto a
        // sampling phase). The driver must then re-enter the HS @ 52
        // MHz path: CMD6 HS_TIMING=1 + set_clock(HighSpeed). The card
        // ends up in HighSpeed, not Hs200.
        let replies = std::vec![
            Ok(ok_r1()),                // CMD0
            cmd8_timeout(),             // CMD8
            Ok(ok_r1()),                // CMD55
            acmd41_timeout(),           // ACMD41
            Ok(ocr_ready_mmc_sector()), // CMD1
            Ok(cid_response()),         // CMD2
            Ok(ok_r1()),                // CMD3
            Ok(csd_v2_response()),      // CMD9
            Ok(ok_r1()),                // CMD7
            Ok(ok_r1()),                // CMD8 MMC R1
            Ok(ok_r1()),                // CMD6 BUS_WIDTH=8
            Ok(r1_tran_ready()),        // CMD13
            // try_hs200 attempts HS_TIMING=2 + tuning, then fails:
            Ok(ok_r1()),         // CMD6 HS_TIMING=2
            Ok(r1_tran_ready()), // CMD13 (post-switch)
            // tuning fails — driver falls through to HS @ 52 MHz:
            Ok(ok_r1()),         // CMD6 HS_TIMING=1
            Ok(r1_tran_ready()), // CMD13 (post-switch)
        ];
        let mut host = MockHost::with_results(replies);
        host.next_read_payload = Some(ext_csd_blob_hs200());
        host.tuning_result = Some(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 21)));
        let mut driver = SdioSdmmc::new(host, NullDelay);
        let _info = driver
            .init()
            .expect("init succeeds even when HS200 tuning fails");

        // We *did* attempt HS200 — voltage switched, tuning called.
        assert_eq!(driver.host.last_voltage, Some(SignalVoltage::V180));
        assert_eq!(driver.host.last_tuning_cmd, Some(21));
        // But ended up at HighSpeed, not Hs200.
        assert_eq!(driver.host.last_clock, Some(ClockSpeed::HighSpeed));

        // Two CMD6 SWITCHes for HS_TIMING: first =2 (HS200, failed),
        // then =1 (HS @ 52 MHz, succeeded).
        let hs_timing_writes: Vec<u8> = driver
            .host
            .commands
            .iter()
            .filter(|c| c.cmd == 6 && ((c.arg >> 16) & 0xFF) as u8 == 185)
            .map(|c| ((c.arg >> 8) & 0xFF) as u8)
            .collect();
        assert_eq!(hs_timing_writes, std::vec![0x02, 0x01]);
    }

    #[test]
    fn set_bus_width_bit8_is_unsupported_via_acmd6() {
        let mut driver = SdioSdmmc::new(MockHost::new(std::vec![ok_r1()]), NullDelay);
        driver.rca = 0x1;
        assert_eq!(
            driver.set_bus_width_sd(BusWidth::Bit8),
            Err(Error::UnsupportedCommand)
        );
    }

    #[test]
    fn switch_to_high_speed_returns_true_when_status_confirms() {
        let mut host = MockHost::new(std::vec![ok_r1()]);
        // Stage the 64-byte status block where group 1 reports HS active.
        let mut status = std::vec![0u8; 64];
        status[16] = 0x01;
        host.next_read_payload = Some(status);

        let mut driver = SdioSdmmc::new(host, NullDelay);
        let active = driver.switch_to_high_speed().unwrap();
        assert!(active);
        let cmd6 = driver
            .host
            .commands
            .iter()
            .find(|c| c.cmd == 6)
            .expect("CMD6 issued");
        assert_eq!(cmd6.arg, 0x80FF_FFF1);
    }

    #[test]
    fn switch_to_high_speed_returns_false_when_card_keeps_default() {
        let mut host = MockHost::new(std::vec![ok_r1()]);
        host.next_read_payload = Some(std::vec![0u8; 64]); // group 1 = 0
        let mut driver = SdioSdmmc::new(host, NullDelay);
        let active = driver.switch_to_high_speed().unwrap();
        assert!(!active);
    }

    #[test]
    fn read_blocks_into_uses_one_multi_block_transfer_for_contiguous_buffer() {
        let mut host = MockHost::new(std::vec![ok_r1(), ok_r1()]);
        let expected: Vec<u8> = (0..1024).map(|i| (i % 251) as u8).collect();
        host.next_read_payload = Some(expected.clone());

        let mut driver = SdioSdmmc::new(host, NullDelay);
        driver.high_capacity = true;
        let mut buf = [0u8; 1024];

        driver.read_blocks_into(7, &mut buf).unwrap();

        assert_eq!(&buf[..], &expected[..]);
        assert_eq!(
            driver.host.prepared_transfers,
            std::vec![(DataDirection::Read, 512, 2)]
        );
        assert_eq!(
            driver
                .host
                .commands
                .iter()
                .map(|c| c.cmd)
                .collect::<Vec<_>>(),
            std::vec![18, 12]
        );
        assert_eq!(driver.host.commands[0].arg, 7);
    }

    #[test]
    fn write_blocks_from_uses_one_multi_block_transfer_for_contiguous_buffer() {
        let host = MockHost::new(std::vec![ok_r1(), ok_r1()]);
        let mut driver = SdioSdmmc::new(host, NullDelay);
        driver.high_capacity = true;
        let buf = [0x5au8; 1024];

        driver.write_blocks_from(11, &buf).unwrap();

        assert_eq!(
            driver.host.prepared_transfers,
            std::vec![(DataDirection::Write, 512, 2)]
        );
        assert_eq!(
            driver
                .host
                .commands
                .iter()
                .map(|c| c.cmd)
                .collect::<Vec<_>>(),
            std::vec![25, 12]
        );
        assert_eq!(driver.host.commands[0].arg, 11);
        assert_eq!(driver.host.writes, std::vec![buf.to_vec()]);
    }

    #[test]
    fn contiguous_multi_block_io_rejects_misaligned_buffers() {
        let host = MockHost::new(std::vec![]);
        let mut driver = SdioSdmmc::new(host, NullDelay);
        let mut read_buf = [0u8; 513];
        let write_buf = [0u8; 513];

        assert_eq!(
            driver.read_blocks_into(0, &mut read_buf),
            Err(Error::Misaligned)
        );
        assert_eq!(
            driver.write_blocks_from(0, &write_buf),
            Err(Error::Misaligned)
        );
        assert!(driver.host.commands.is_empty());
    }
}
