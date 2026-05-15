//! SDIO (Secure Digital Input Output) mode transport layer
//!
//! SDIO mode uses a dedicated host controller with 1-bit or 4-bit data bus.
//! Implement [`SdioHost`] for your platform's SDIO peripheral; the host
//! implementation controls command/data progress.

use core::task::Waker;

use log::{debug, info, warn};

pub use crate::cmd::DataDirection;
use crate::{
    block::{BlockRequestId, CommandResponsePoll, DataCommandPoll, OperationPoll},
    cmd::Command,
    common::block_addr_of,
    error::{Error, ErrorContext, Phase},
    response::{
        CardState, CidResponse, CsdResponse, OcrResponse, Response, ResponseType, SwitchStatus,
    },
};

/// Host IRQ event category returned by portable controller cores.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HostEventKind {
    /// No runtime action is required.
    #[default]
    None,
    /// A command response is ready.
    CommandComplete,
    /// A data transfer has completed.
    TransferComplete,
    /// Receive-side FIFO or buffer data is ready.
    ReceiveReady,
    /// Transmit-side FIFO or buffer space is ready.
    TransmitReady,
    /// Hardware reported an error condition.
    Error,
    /// Status is pending but has no stable protocol-level category.
    Other,
}

/// Hardware engine affected by a host IRQ event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HostEventSource {
    /// Whole controller or unknown source.
    #[default]
    Controller,
    /// Command engine.
    Command,
    /// Data engine or block queue.
    Data,
}

/// Stable event summary extracted by a host controller IRQ handler.
pub trait HostEvent {
    fn kind(&self) -> HostEventKind;

    fn source(&self) -> HostEventSource {
        HostEventSource::Controller
    }

    fn queue_id(&self) -> Option<BlockRequestId> {
        None
    }
}

impl HostEvent for () {
    fn kind(&self) -> HostEventKind {
        HostEventKind::None
    }
}

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
    /// Host-controller IRQ event type.
    ///
    /// Portable host crates can expose their native event enum here. The
    /// protocol layer does not interpret it; OS glue maps it to runtime wakeups.
    type Event: HostEvent + Default;

    /// Submit a command without waiting for its response.
    fn submit_command(&mut self, cmd: &Command) -> Result<(), Error>;

    /// Advance a submitted command and harvest the response when complete.
    fn poll_command_response(&mut self) -> Result<CommandResponsePoll, Error>;

    type DataRequest<'a>
    where
        Self: 'a;

    /// Submit a read-data command without waiting for its data phase.
    fn submit_read_data<'a>(
        &mut self,
        cmd: &Command,
        buf: &'a mut [u8],
        block_size: u32,
        block_count: u32,
    ) -> Result<Self::DataRequest<'a>, Error>;

    /// Submit a write-data command without waiting for its data phase.
    fn submit_write_data<'a>(
        &mut self,
        cmd: &Command,
        buf: &'a [u8],
        block_size: u32,
        block_count: u32,
    ) -> Result<Self::DataRequest<'a>, Error>;

    /// Advance a previously submitted data command without blocking.
    fn poll_data_request<'a>(
        &mut self,
        request: &mut Self::DataRequest<'a>,
    ) -> Result<DataCommandPoll, Error>;

    /// Set the bus width
    fn set_bus_width(&mut self, width: BusWidth) -> Result<(), Error>;

    /// Set the clock speed
    fn set_clock(&mut self, speed: ClockSpeed) -> Result<(), Error>;

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

    /// Route command/data completion and error status to the host IRQ line.
    ///
    /// Default is a no-op so polling-only hosts do not have to implement IRQ
    /// support.
    fn enable_completion_irq(&mut self) -> Result<(), Error> {
        Ok(())
    }

    /// Mask host IRQ delivery while keeping the controller usable for polling.
    ///
    /// Default is a no-op for polling-only hosts.
    fn disable_completion_irq(&mut self) -> Result<(), Error> {
        Ok(())
    }

    /// Acknowledge pending host IRQ status and return a hardware event summary.
    ///
    /// This must not block or perform OS wakeups.
    fn handle_irq(&mut self) -> Self::Event {
        Self::Event::default()
    }

    /// Register the task that should be woken when command or data progress is
    /// possible. Polling-only hosts may keep the default no-op implementation.
    fn register_waker(&mut self, _waker: &Waker) {}
}

struct SdioInitTiming;

impl SdioInitTiming {
    const TIMEOUT_MS: u32 = 1_000;
    const POLL_MS: u32 = 10;
}

struct MmcSwitchTiming;

impl MmcSwitchTiming {
    const TIMEOUT_MS: u32 = 250;
    const POLL_MS: u32 = SdioInitTiming::POLL_MS;
}

/// SDIO mode SD/MMC driver
pub struct SdioSdmmc<H: SdioHost> {
    host: H,
    rca: u16,
    high_capacity: bool,
    bus_width: BusWidth,
    kind: CardKind,
    sd_speed_selection_enabled: bool,
}

pub struct SdioDataRequest<'a, H: SdioHost + 'a> {
    inner: H::DataRequest<'a>,
}

/// Submitted SDIO command transaction.
pub struct SdioCommandRequest;

/// Submitted `CMD13 SEND_STATUS` transaction.
pub struct SdioStatusRequest {
    inner: SdioCommandRequest,
}

/// Submitted MMC `CMD8 SEND_EXT_CSD` data transaction.
pub struct ExtCsdRequest<'a, H: SdioHost + 'a> {
    inner: SdioDataRequest<'a, H>,
}

/// Submitted SD `CMD6 SWITCH_FUNC` data transaction.
pub struct SwitchFunctionRequest<'a, H: SdioHost + 'a> {
    inner: SdioDataRequest<'a, H>,
}

/// Submitted MMC `CMD6 SWITCH` transaction.
pub struct MmcSwitchRequest {
    rca: u16,
    index: u8,
    value: u8,
    elapsed_ms: u32,
    state: MmcSwitchRequestState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MmcSwitchRequestState {
    PollSwitch,
    PollStatus,
}

/// Card initialization probe order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardInitPreference {
    /// Probe SD first, then fall back to MMC.
    SdFirst,
    /// Probe MMC first. Use this for controller instances wired to eMMC.
    MmcFirst,
}

/// Caller-owned scratch buffers for SD/MMC initialization data commands.
pub struct SdioInitScratch {
    ext_csd: [u8; 512],
    switch_status: [u8; 64],
}

impl SdioInitScratch {
    pub const fn new() -> Self {
        Self {
            ext_csd: [0; 512],
            switch_status: [0; 64],
        }
    }
}

impl Default for SdioInitScratch {
    fn default() -> Self {
        Self::new()
    }
}

/// Submitted SDIO initialization transaction.
pub struct SdioInitRequest<'a, H: SdioHost + 'a> {
    state: SdioInitState,
    preference: CardInitPreference,
    sd_v2: bool,
    kind: Option<CardKind>,
    ocr: Option<OcrResponse>,
    cid: Option<CidResponse>,
    capacity_blocks: Option<u64>,
    parsed_ext_csd: Option<crate::ext_csd::ExtCsd>,
    acmd41_elapsed_ms: u32,
    mmc_elapsed_ms: u32,
    mmc_ocr_arg: u32,
    needs_pace: bool,
    ext_csd_buf: core::ptr::NonNull<[u8; 512]>,
    switch_status_buf: core::ptr::NonNull<[u8; 64]>,
    ext_csd_request: Option<ExtCsdRequest<'a, H>>,
    switch_function_request: Option<SwitchFunctionRequest<'a, H>>,
    mmc_switch_request: Option<MmcSwitchRequest>,
    status_request: Option<SdioStatusRequest>,
    command_request: Option<SdioCommandRequest>,
    current_bus_width: BusWidth,
    current_access_mode: Option<SdAccessMode>,
    sd_access_index: usize,
    mmc_hs200_attempted: bool,
    _scratch: core::marker::PhantomData<&'a mut SdioInitScratch>,
}

impl<'a, H: SdioHost + 'a> SdioInitRequest<'a, H> {
    fn new(preference: CardInitPreference, scratch: &'a mut SdioInitScratch) -> Self {
        Self {
            state: SdioInitState::PollCmd0,
            preference,
            sd_v2: false,
            kind: None,
            ocr: None,
            cid: None,
            capacity_blocks: None,
            parsed_ext_csd: None,
            acmd41_elapsed_ms: 0,
            mmc_elapsed_ms: 0,
            mmc_ocr_arg: 0,
            needs_pace: false,
            ext_csd_buf: core::ptr::NonNull::from(&mut scratch.ext_csd),
            switch_status_buf: core::ptr::NonNull::from(&mut scratch.switch_status),
            ext_csd_request: None,
            switch_function_request: None,
            mmc_switch_request: None,
            status_request: None,
            command_request: None,
            current_bus_width: BusWidth::Bit1,
            current_access_mode: None,
            sd_access_index: 0,
            mmc_hs200_attempted: false,
            _scratch: core::marker::PhantomData,
        }
    }

    /// Consume the pending power-up pacing hint for blocking runtimes.
    ///
    /// `poll_init_request` sets this when the card answered ACMD41/CMD1 but
    /// has not completed power-up yet. Runtime glue can translate it into a
    /// sleep, yield, timer wait, or busy wait. Ordinary command/data pending
    /// states do not set this hint.
    pub fn take_needs_pace(&mut self) -> bool {
        let needs_pace = self.needs_pace;
        self.needs_pace = false;
        needs_pace
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SdioInitState {
    PollCmd0,
    PollCmd8,
    PollAcmd41Cmd55,
    PollAcmd41,
    PollMmcInitial,
    PollMmcReady,
    PollCmd2,
    PollCmd3,
    PollCmd9,
    PollCmd7,
    PollSdBusWidthCmd55,
    PollSdBusWidthAcmd6,
    FinishCardSetup,
    PollMmcExtCsd,
    PollMmcBusWidth,
    PrepareMmcSpeed,
    PollMmcHs200Switch,
    PollMmcHs200Status,
    PollMmcHs52Switch,
    PrepareSdSpeed,
    PollSdSwitchFunctionCheck,
    PollSdVoltageSwitch,
    PollSdSetAccessMode,
    PollSdStatus,
    Complete,
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

    const fn name(self) -> &'static str {
        match self {
            Self::HighSpeed => "HighSpeed",
            Self::Sdr50 => "SDR50",
            Self::Sdr104 => "SDR104",
            Self::Ddr50 => "DDR50",
        }
    }
}

impl<H: SdioHost> SdioSdmmc<H> {
    pub fn new(host: H) -> Self {
        Self {
            host,
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

    pub fn submit_read_blocks_into<'a>(
        &mut self,
        addr: u32,
        buf: &'a mut [u8],
    ) -> Result<SdioDataRequest<'a, H>, Error>
    where
        H: 'a,
    {
        let count = block_count_from_len(buf.len())?;
        let block_addr = block_addr_of(addr, self.high_capacity);
        let cmd = if count == 1 {
            crate::cmd::cmd17(block_addr)
        } else {
            crate::cmd::cmd18(block_addr)
        };
        let inner = self.host.submit_read_data(&cmd, buf, 512, count)?;
        Ok(SdioDataRequest { inner })
    }

    pub fn submit_write_blocks_from<'a>(
        &mut self,
        addr: u32,
        buf: &'a [u8],
    ) -> Result<SdioDataRequest<'a, H>, Error>
    where
        H: 'a,
    {
        let count = block_count_from_len(buf.len())?;
        let block_addr = block_addr_of(addr, self.high_capacity);
        let cmd = if count == 1 {
            crate::cmd::cmd24(block_addr)
        } else {
            crate::cmd::cmd25(block_addr)
        };
        let inner = self.host.submit_write_data(&cmd, buf, 512, count)?;
        Ok(SdioDataRequest { inner })
    }

    pub fn poll_data_request<'a>(
        &mut self,
        request: &mut SdioDataRequest<'a, H>,
    ) -> Result<DataCommandPoll, Error>
    where
        H: 'a,
    {
        self.host.poll_data_request(&mut request.inner)
    }

    pub fn submit_command_request(&mut self, cmd: &Command) -> Result<SdioCommandRequest, Error> {
        self.host.submit_command(cmd)?;
        Ok(SdioCommandRequest)
    }

    pub fn poll_command_request(
        &mut self,
        _request: &mut SdioCommandRequest,
    ) -> Result<CommandResponsePoll, Error> {
        self.host.poll_command_response()
    }

    pub fn submit_status(&mut self) -> Result<SdioStatusRequest, Error> {
        let cmd = crate::cmd::cmd13(self.rca);
        let inner = self.submit_command_request(&cmd)?;
        Ok(SdioStatusRequest { inner })
    }

    pub fn poll_status_request(
        &mut self,
        request: &mut SdioStatusRequest,
    ) -> Result<OperationPoll<CardState>, Error> {
        match self.poll_command_request(&mut request.inner)? {
            CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
            CommandResponsePoll::Complete(Response::R1(r1)) => {
                Ok(OperationPoll::Complete(r1.current_state()))
            }
            CommandResponsePoll::Complete(_) => Err(Error::BadResponse(ErrorContext::for_cmd(
                Phase::ResponseWait,
                13,
            ))),
        }
    }

    pub fn submit_read_data_command<'a>(
        &mut self,
        cmd: &Command,
        buf: &'a mut [u8],
        block_size: u32,
        block_count: u32,
    ) -> Result<SdioDataRequest<'a, H>, Error>
    where
        H: 'a,
    {
        let inner = self
            .host
            .submit_read_data(cmd, buf, block_size, block_count)?;
        Ok(SdioDataRequest { inner })
    }

    pub fn submit_read_ext_csd<'a>(
        &mut self,
        buf: &'a mut [u8; 512],
    ) -> Result<ExtCsdRequest<'a, H>, Error>
    where
        H: 'a,
    {
        let inner = self.submit_read_data_command(&crate::cmd::CMD8_MMC, buf, 512, 1)?;
        Ok(ExtCsdRequest { inner })
    }

    pub fn poll_ext_csd_request<'a>(
        &mut self,
        request: &mut ExtCsdRequest<'a, H>,
    ) -> Result<OperationPoll<()>, Error>
    where
        H: 'a,
    {
        match self.poll_data_request(&mut request.inner)? {
            DataCommandPoll::Pending => Ok(OperationPoll::Pending),
            DataCommandPoll::Complete(_) => Ok(OperationPoll::Complete(())),
        }
    }

    pub fn submit_switch_function<'a>(
        &mut self,
        cmd: &Command,
        buf: &'a mut [u8; 64],
    ) -> Result<SwitchFunctionRequest<'a, H>, Error>
    where
        H: 'a,
    {
        let inner = self.submit_read_data_command(cmd, buf, 64, 1)?;
        Ok(SwitchFunctionRequest { inner })
    }

    pub fn poll_switch_function_request<'a>(
        &mut self,
        request: &mut SwitchFunctionRequest<'a, H>,
    ) -> Result<OperationPoll<()>, Error>
    where
        H: 'a,
    {
        match self.poll_data_request(&mut request.inner)? {
            DataCommandPoll::Pending => Ok(OperationPoll::Pending),
            DataCommandPoll::Complete(_) => Ok(OperationPoll::Complete(())),
        }
    }

    pub fn submit_mmc_switch(
        &mut self,
        access: u8,
        index: u8,
        value: u8,
    ) -> Result<MmcSwitchRequest, Error> {
        let cmd = crate::cmd::cmd6_mmc_switch(access, index, value);
        self.host.submit_command(&cmd)?;
        Ok(MmcSwitchRequest {
            rca: self.rca,
            index,
            value,
            elapsed_ms: 0,
            state: MmcSwitchRequestState::PollSwitch,
        })
    }

    pub fn poll_mmc_switch_request(
        &mut self,
        request: &mut MmcSwitchRequest,
    ) -> Result<OperationPoll<()>, Error> {
        match request.state {
            MmcSwitchRequestState::PollSwitch => match self.host.poll_command_response()? {
                CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                CommandResponsePoll::Complete(_) => {
                    let cmd = crate::cmd::cmd13(request.rca);
                    self.host.submit_command(&cmd)?;
                    request.state = MmcSwitchRequestState::PollStatus;
                    Ok(OperationPoll::Pending)
                }
            },
            MmcSwitchRequestState::PollStatus => match self.host.poll_command_response()? {
                CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                CommandResponsePoll::Complete(Response::R1(r1)) => {
                    if r1.switch_error() {
                        warn!(
                            "sdio: SWITCH_ERROR after CMD6 idx={} val={}",
                            request.index, request.value
                        );
                        return Err(Error::CardError(crate::error::CardError::IllegalCommand));
                    }
                    if r1.ready_for_data() && matches!(r1.current_state(), CardState::Transfer) {
                        return Ok(OperationPoll::Complete(()));
                    }
                    if request.elapsed_ms >= MmcSwitchTiming::TIMEOUT_MS {
                        return Err(Error::Timeout(ErrorContext::for_cmd(Phase::Init, 6)));
                    }
                    request.elapsed_ms =
                        request.elapsed_ms.saturating_add(MmcSwitchTiming::POLL_MS);
                    let cmd = crate::cmd::cmd13(request.rca);
                    self.host.submit_command(&cmd)?;
                    Ok(OperationPoll::Pending)
                }
                CommandResponsePoll::Complete(_) => {
                    if request.elapsed_ms >= MmcSwitchTiming::TIMEOUT_MS {
                        return Err(Error::Timeout(ErrorContext::for_cmd(Phase::Init, 6)));
                    }
                    request.elapsed_ms =
                        request.elapsed_ms.saturating_add(MmcSwitchTiming::POLL_MS);
                    let cmd = crate::cmd::cmd13(request.rca);
                    self.host.submit_command(&cmd)?;
                    Ok(OperationPoll::Pending)
                }
            },
        }
    }
}

impl<H: SdioHost> SdioSdmmc<H> {
    /// Submit SD/MMC card initialization without waiting for completion.
    pub fn submit_init<'a>(
        &mut self,
        scratch: &'a mut SdioInitScratch,
    ) -> Result<SdioInitRequest<'a, H>, Error>
    where
        H: 'a,
    {
        self.submit_init_with_preference(CardInitPreference::SdFirst, scratch)
    }

    /// Submit SD/MMC card initialization with a caller-selected probe order.
    pub fn submit_init_with_preference<'a>(
        &mut self,
        preference: CardInitPreference,
        scratch: &'a mut SdioInitScratch,
    ) -> Result<SdioInitRequest<'a, H>, Error>
    where
        H: 'a,
    {
        info!("sdio: init starting");
        self.host.set_bus_width(BusWidth::Bit1)?;
        self.host.set_clock(ClockSpeed::Identification)?;

        info!("sdio: CMD0 reset");
        self.host.submit_command(&crate::cmd::CMD0)?;
        Ok(SdioInitRequest::new(preference, scratch))
    }

    /// Advance a submitted initialization request without blocking.
    pub fn poll_init_request<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
    ) -> Result<OperationPoll<CardInfo>, Error> {
        const MMC_HCS: u32 = 1 << 30;
        const MMC_VOLTAGE_MASK: u32 = 0x00FF_8000;
        const MMC_ACCESS_MODE_MASK: u32 = 0x6000_0000;

        match request.state {
            SdioInitState::PollCmd0 => match self.host.poll_command_response()? {
                CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                CommandResponsePoll::Complete(_) => {
                    match request.preference {
                        CardInitPreference::SdFirst => {
                            let cmd = crate::cmd::cmd8(0x01, 0xAA);
                            self.host.submit_command(&cmd)?;
                            request.state = SdioInitState::PollCmd8;
                        }
                        CardInitPreference::MmcFirst => {
                            info!("sdio: MMC-first init, trying CMD1");
                            self.host.submit_command(&crate::cmd::cmd1(0))?;
                            request.state = SdioInitState::PollMmcInitial;
                        }
                    }
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PollCmd8 => match self.host.poll_command_response() {
                Ok(CommandResponsePoll::Pending) => Ok(OperationPoll::Pending),
                Ok(CommandResponsePoll::Complete(Response::R7(resp))) => {
                    request.sd_v2 = resp.verify(0x01, 0xAA);
                    info!("sdio: CMD8 sd_v2={}", request.sd_v2);
                    let cmd55 = crate::cmd::cmd55(0);
                    self.host.submit_command(&cmd55)?;
                    request.state = SdioInitState::PollAcmd41Cmd55;
                    Ok(OperationPoll::Pending)
                }
                Ok(CommandResponsePoll::Complete(_))
                | Err(Error::Timeout(_))
                | Err(Error::BadResponse(_))
                | Err(Error::Crc(_)) => {
                    request.sd_v2 = false;
                    info!("sdio: CMD8 sd_v2=false");
                    let cmd55 = crate::cmd::cmd55(0);
                    self.host.submit_command(&cmd55)?;
                    request.state = SdioInitState::PollAcmd41Cmd55;
                    Ok(OperationPoll::Pending)
                }
                Err(e) => Err(e),
            },
            SdioInitState::PollAcmd41Cmd55 => match self.host.poll_command_response() {
                Ok(CommandResponsePoll::Pending) => Ok(OperationPoll::Pending),
                Ok(CommandResponsePoll::Complete(_)) => {
                    let acmd41 = crate::cmd::cmd41_with_s18r(request.sd_v2, 0xFF8000, true);
                    self.host.submit_command(&acmd41)?;
                    request.state = SdioInitState::PollAcmd41;
                    Ok(OperationPoll::Pending)
                }
                Err(_sd_err) => {
                    info!(
                        "sdio: ACMD41 prologue failed ({:?}), trying MMC CMD1",
                        _sd_err
                    );
                    self.host.submit_command(&crate::cmd::cmd1(0))?;
                    request.state = SdioInitState::PollMmcInitial;
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PollAcmd41 => match self.host.poll_command_response() {
                Ok(CommandResponsePoll::Pending) => Ok(OperationPoll::Pending),
                Ok(CommandResponsePoll::Complete(Response::R3(ocr))) => {
                    if ocr.card_powered_up() {
                        request.kind = Some(CardKind::Sd);
                        request.ocr = Some(ocr);
                        self.kind = CardKind::Sd;
                        info!("sdio: detected {:?} ocr={:#010x}", CardKind::Sd, ocr.raw);
                        self.host.submit_command(&crate::cmd::CMD2)?;
                        request.state = SdioInitState::PollCmd2;
                    } else {
                        if request.acmd41_elapsed_ms >= SdioInitTiming::TIMEOUT_MS {
                            warn!(
                                "sdio: ACMD41 timed out after {}ms, trying MMC CMD1",
                                request.acmd41_elapsed_ms
                            );
                            self.host.submit_command(&crate::cmd::cmd1(0))?;
                            request.state = SdioInitState::PollMmcInitial;
                            return Ok(OperationPoll::Pending);
                        }
                        request.acmd41_elapsed_ms = request
                            .acmd41_elapsed_ms
                            .saturating_add(SdioInitTiming::POLL_MS);
                        let cmd55 = crate::cmd::cmd55(0);
                        self.host.submit_command(&cmd55)?;
                        request.state = SdioInitState::PollAcmd41Cmd55;
                        request.needs_pace = true;
                    }
                    Ok(OperationPoll::Pending)
                }
                Ok(CommandResponsePoll::Complete(_)) => {
                    info!("sdio: ACMD41 returned bad response, trying MMC CMD1");
                    self.host.submit_command(&crate::cmd::cmd1(0))?;
                    request.state = SdioInitState::PollMmcInitial;
                    Ok(OperationPoll::Pending)
                }
                Err(_sd_err) => {
                    info!("sdio: ACMD41 failed ({:?}), trying MMC CMD1", _sd_err);
                    self.host.submit_command(&crate::cmd::cmd1(0))?;
                    request.state = SdioInitState::PollMmcInitial;
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PollMmcInitial => match self.host.poll_command_response()? {
                CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                CommandResponsePoll::Complete(Response::R3(ocr)) => {
                    if ocr.card_powered_up() {
                        request.kind = Some(CardKind::Mmc);
                        request.ocr = Some(ocr);
                        self.kind = CardKind::Mmc;
                        info!("sdio: detected {:?} ocr={:#010x}", CardKind::Mmc, ocr.raw);
                        self.host.submit_command(&crate::cmd::CMD2)?;
                        request.state = SdioInitState::PollCmd2;
                    } else {
                        let voltage = ocr.raw & MMC_VOLTAGE_MASK;
                        let voltage = if voltage == 0 {
                            MMC_VOLTAGE_MASK
                        } else {
                            voltage
                        };
                        request.mmc_ocr_arg = MMC_HCS | voltage | (ocr.raw & MMC_ACCESS_MODE_MASK);
                        let cmd = crate::cmd::cmd1(request.mmc_ocr_arg);
                        self.host.submit_command(&cmd)?;
                        request.state = SdioInitState::PollMmcReady;
                    }
                    Ok(OperationPoll::Pending)
                }
                CommandResponsePoll::Complete(_) => {
                    Err(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 1)))
                }
            },
            SdioInitState::PollMmcReady => match self.host.poll_command_response()? {
                CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                CommandResponsePoll::Complete(Response::R3(ocr)) => {
                    if ocr.card_powered_up() {
                        request.kind = Some(CardKind::Mmc);
                        request.ocr = Some(ocr);
                        self.kind = CardKind::Mmc;
                        info!("sdio: detected {:?} ocr={:#010x}", CardKind::Mmc, ocr.raw);
                        self.host.submit_command(&crate::cmd::CMD2)?;
                        request.state = SdioInitState::PollCmd2;
                    } else {
                        if request.mmc_elapsed_ms >= SdioInitTiming::TIMEOUT_MS {
                            warn!("sdio: CMD1 timed out after {}ms", request.mmc_elapsed_ms);
                            return Err(Error::Timeout(ErrorContext::for_cmd(Phase::Init, 1)));
                        }
                        request.mmc_elapsed_ms = request
                            .mmc_elapsed_ms
                            .saturating_add(SdioInitTiming::POLL_MS);
                        let cmd = crate::cmd::cmd1(request.mmc_ocr_arg);
                        self.host.submit_command(&cmd)?;
                        request.needs_pace = true;
                    }
                    Ok(OperationPoll::Pending)
                }
                CommandResponsePoll::Complete(_) => {
                    Err(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 1)))
                }
            },
            SdioInitState::PollCmd2 => match self.host.poll_command_response()? {
                CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                CommandResponsePoll::Complete(response) => {
                    if let Response::R2(raw) = response {
                        request.cid = Some(CidResponse::from_raw(raw));
                    } else {
                        request.cid = None;
                    }
                    match request.kind.ok_or(Error::InvalidArgument)? {
                        CardKind::Sd => self.host.submit_command(&crate::cmd::CMD3_SD)?,
                        CardKind::Mmc => self.host.submit_command(&crate::cmd::cmd3_mmc(1))?,
                    }
                    request.state = SdioInitState::PollCmd3;
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PollCmd3 => match self.host.poll_command_response()? {
                CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                CommandResponsePoll::Complete(response) => {
                    self.rca = match (request.kind.ok_or(Error::InvalidArgument)?, response) {
                        (CardKind::Sd, Response::R6(resp)) => resp.rca(),
                        (CardKind::Mmc, Response::R1(_)) => 1,
                        _ => {
                            return Err(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 3)));
                        }
                    };
                    info!("sdio: CMD3 rca={:#x}", self.rca);
                    let cmd9 = crate::cmd::cmd9(self.rca);
                    self.host.submit_command(&cmd9)?;
                    request.state = SdioInitState::PollCmd9;
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PollCmd9 => match self.host.poll_command_response()? {
                CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                CommandResponsePoll::Complete(response) => {
                    request.capacity_blocks = match response {
                        Response::R2(raw) => CsdResponse::from_raw(raw).capacity_blocks(),
                        _ => None,
                    };
                    info!("sdio: CSD capacity_blocks={:?}", request.capacity_blocks);
                    let cmd7 = crate::cmd::cmd7(self.rca);
                    self.host.submit_command(&cmd7)?;
                    request.state = SdioInitState::PollCmd7;
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PollCmd7 => match self.host.poll_command_response()? {
                CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                CommandResponsePoll::Complete(_) => {
                    let ocr = request.ocr.ok_or(Error::InvalidArgument)?;
                    self.high_capacity = ocr.ccs();
                    match request.kind.ok_or(Error::InvalidArgument)? {
                        CardKind::Sd => {
                            info!("sdio: switch SD bus width to 4-bit");
                            let cmd55 = crate::cmd::cmd55(self.rca);
                            self.host.submit_command(&cmd55)?;
                            request.state = SdioInitState::PollSdBusWidthCmd55;
                        }
                        CardKind::Mmc => {
                            request.state = SdioInitState::FinishCardSetup;
                        }
                    }
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PollSdBusWidthCmd55 => match self.host.poll_command_response()? {
                CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                CommandResponsePoll::Complete(_) => {
                    let acmd6 = Command::new(6, sd_acmd6_arg(BusWidth::Bit4)?, ResponseType::R1);
                    self.host.submit_command(&acmd6)?;
                    request.state = SdioInitState::PollSdBusWidthAcmd6;
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PollSdBusWidthAcmd6 => match self.host.poll_command_response()? {
                CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                CommandResponsePoll::Complete(_) => {
                    self.host.set_bus_width(BusWidth::Bit4)?;
                    self.bus_width = BusWidth::Bit4;
                    request.state = SdioInitState::FinishCardSetup;
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::FinishCardSetup => {
                let kind = request.kind.ok_or(Error::InvalidArgument)?;
                match kind {
                    CardKind::Sd => {
                        if self.sd_speed_selection_enabled {
                            request.state = SdioInitState::PrepareSdSpeed;
                        } else {
                            self.host.set_clock(ClockSpeed::Default)?;
                            info!("sdio: SD speed selection disabled; staying at default speed");
                            request.state = SdioInitState::Complete;
                        }
                        Ok(OperationPoll::Pending)
                    }
                    CardKind::Mmc => {
                        info!("sdio: read MMC EXT_CSD");
                        let ext_csd = unsafe { request.ext_csd_buf.as_mut() };
                        request.ext_csd_request = Some(self.submit_read_ext_csd(ext_csd)?);
                        request.state = SdioInitState::PollMmcExtCsd;
                        Ok(OperationPoll::Pending)
                    }
                }
            }
            SdioInitState::PollMmcExtCsd => {
                let ext_request = request
                    .ext_csd_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                match self.poll_ext_csd_request(ext_request)? {
                    OperationPoll::Pending => Ok(OperationPoll::Pending),
                    OperationPoll::Complete(()) => {
                        request.ext_csd_request = None;
                        let csd = crate::ext_csd::ExtCsd::from_bytes(unsafe {
                            *request.ext_csd_buf.as_ref()
                        });
                        if let Some(sectors) = csd.sector_count() {
                            request.capacity_blocks = Some(sectors as u64);
                            info!("sdio: EXT_CSD sector_count={}", sectors);
                        }
                        request.parsed_ext_csd = Some(csd);
                        submit_mmc_bus_width_or_continue(self, request, BusWidth::Bit8)
                    }
                }
            }
            SdioInitState::PollMmcBusWidth => {
                let switch_request = request
                    .mmc_switch_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                match self.poll_mmc_switch_request(switch_request) {
                    Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                    Ok(OperationPoll::Complete(())) => {
                        request.mmc_switch_request = None;
                        match self.host.set_bus_width(request.current_bus_width) {
                            Ok(()) => {
                                self.bus_width = request.current_bus_width;
                                request.state = SdioInitState::PrepareMmcSpeed;
                                Ok(OperationPoll::Pending)
                            }
                            Err(err) if matches!(request.current_bus_width, BusWidth::Bit8) => {
                                info!("sdio: 8-bit refused ({:?}), trying 4-bit", err);
                                submit_mmc_bus_width_or_continue(self, request, BusWidth::Bit4)
                            }
                            Err(err) if matches!(request.current_bus_width, BusWidth::Bit4) => {
                                info!("sdio: 4-bit refused ({:?}), staying at 1-bit", err);
                                request.state = SdioInitState::PrepareMmcSpeed;
                                Ok(OperationPoll::Pending)
                            }
                            Err(err) => Err(err),
                        }
                    }
                    Err(err) if matches!(request.current_bus_width, BusWidth::Bit8) => {
                        request.mmc_switch_request = None;
                        info!("sdio: 8-bit refused ({:?}), trying 4-bit", err);
                        submit_mmc_bus_width_or_continue(self, request, BusWidth::Bit4)
                    }
                    Err(err) if matches!(request.current_bus_width, BusWidth::Bit4) => {
                        request.mmc_switch_request = None;
                        info!("sdio: 4-bit refused ({:?}), staying at 1-bit", err);
                        request.state = SdioInitState::PrepareMmcSpeed;
                        Ok(OperationPoll::Pending)
                    }
                    Err(err) => Err(err),
                }
            }
            SdioInitState::PrepareMmcSpeed => {
                let Some(csd) = request.parsed_ext_csd.as_ref() else {
                    return Err(Error::InvalidArgument);
                };
                let dt = csd.device_type();
                if !request.mmc_hs200_attempted
                    && !matches!(self.bus_width, BusWidth::Bit1)
                    && dt.supports_hs200()
                {
                    request.mmc_hs200_attempted = true;
                    match self.host.switch_voltage(SignalVoltage::V180) {
                        Ok(()) | Err(Error::UnsupportedCommand) => {
                            if self.host.set_clock(ClockSpeed::Hs200).is_ok() {
                                let switch_request = self.submit_mmc_switch(
                                    0b11,
                                    crate::cmd::ext_csd::HS_TIMING as u8,
                                    0x02,
                                )?;
                                request.mmc_switch_request = Some(switch_request);
                                request.state = SdioInitState::PollMmcHs200Switch;
                                return Ok(OperationPoll::Pending);
                            }
                        }
                        Err(err) => debug!("sdio: switch_voltage(V180) failed ({:?})", err),
                    }
                    self.rollback_to_hs_compat();
                }
                if dt.supports_hs_52() {
                    let switch_request =
                        self.submit_mmc_switch(0b11, crate::cmd::ext_csd::HS_TIMING as u8, 1)?;
                    request.mmc_switch_request = Some(switch_request);
                    request.state = SdioInitState::PollMmcHs52Switch;
                } else {
                    request.state = SdioInitState::Complete;
                }
                Ok(OperationPoll::Pending)
            }
            SdioInitState::PollMmcHs200Switch => {
                let switch_request = request
                    .mmc_switch_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                match self.poll_mmc_switch_request(switch_request) {
                    Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                    Ok(OperationPoll::Complete(())) => {
                        request.mmc_switch_request = None;
                        if self.host.execute_tuning(21).is_ok() {
                            let status_request = self.submit_status()?;
                            request.status_request = Some(status_request);
                            request.state = SdioInitState::PollMmcHs200Status;
                        } else {
                            self.rollback_to_hs_compat();
                            request.state = SdioInitState::PrepareMmcSpeed;
                        }
                        Ok(OperationPoll::Pending)
                    }
                    Err(err) => {
                        request.mmc_switch_request = None;
                        debug!("sdio: MMC HS200 switch refused ({:?})", err);
                        self.rollback_to_hs_compat();
                        request.state = SdioInitState::PrepareMmcSpeed;
                        Ok(OperationPoll::Pending)
                    }
                }
            }
            SdioInitState::PollMmcHs200Status => {
                let status_request = request
                    .status_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                match self.poll_status_request(status_request)? {
                    OperationPoll::Pending => Ok(OperationPoll::Pending),
                    OperationPoll::Complete(CardState::Transfer) => {
                        request.status_request = None;
                        info!("sdio: HS200 entry succeeded");
                        request.state = SdioInitState::Complete;
                        Ok(OperationPoll::Pending)
                    }
                    OperationPoll::Complete(_) => {
                        request.status_request = None;
                        self.rollback_to_hs_compat();
                        request.state = SdioInitState::PrepareMmcSpeed;
                        Ok(OperationPoll::Pending)
                    }
                }
            }
            SdioInitState::PollMmcHs52Switch => {
                let switch_request = request
                    .mmc_switch_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                match self.poll_mmc_switch_request(switch_request) {
                    Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                    Ok(OperationPoll::Complete(())) => {
                        request.mmc_switch_request = None;
                        if let Err(_e) = self.host.set_clock(ClockSpeed::HighSpeed) {
                            info!("sdio: host refused HighSpeed clock ({:?})", _e);
                        }
                        request.state = SdioInitState::Complete;
                        Ok(OperationPoll::Pending)
                    }
                    Err(_e) => {
                        request.mmc_switch_request = None;
                        info!("sdio: MMC HS_TIMING switch refused ({:?})", _e);
                        request.state = SdioInitState::Complete;
                        Ok(OperationPoll::Pending)
                    }
                }
            }
            SdioInitState::PrepareSdSpeed => {
                let buf = unsafe { request.switch_status_buf.as_mut() };
                let switch_request =
                    self.submit_switch_function(&crate::cmd::cmd6_sd_access_mode(false, 0), buf)?;
                request.switch_function_request = Some(switch_request);
                request.state = SdioInitState::PollSdSwitchFunctionCheck;
                Ok(OperationPoll::Pending)
            }
            SdioInitState::PollSdSwitchFunctionCheck => {
                let switch_request = request
                    .switch_function_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                match self.poll_switch_function_request(switch_request) {
                    Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                    Ok(OperationPoll::Complete(())) => {
                        request.switch_function_request = None;
                        let status =
                            SwitchStatus::from_raw(unsafe { *request.switch_status_buf.as_ref() });
                        info!(
                            "sdio: SD access mode support hs={} sdr50={} sdr104={} ddr50={} \
                             s18a={}",
                            status.access_mode_supported(SdAccessMode::HighSpeed.function()),
                            status.access_mode_supported(SdAccessMode::Sdr50.function()),
                            status.access_mode_supported(SdAccessMode::Sdr104.function()),
                            status.access_mode_supported(SdAccessMode::Ddr50.function()),
                            request.ocr.ok_or(Error::InvalidArgument)?.s18a()
                        );
                        request.sd_access_index = 0;
                        submit_next_sd_access_mode(self, request, status)
                    }
                    Err(err) => {
                        request.switch_function_request = None;
                        warn!("sdio: SD speed selection skipped ({:?})", err);
                        request.state = SdioInitState::Complete;
                        Ok(OperationPoll::Pending)
                    }
                }
            }
            SdioInitState::PollSdVoltageSwitch => {
                let cmd = request
                    .command_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                let mode = request.current_access_mode.ok_or(Error::InvalidArgument)?;
                match self.poll_command_request(cmd) {
                    Ok(CommandResponsePoll::Pending) => Ok(OperationPoll::Pending),
                    Ok(CommandResponsePoll::Complete(_)) => {
                        request.command_request = None;
                        match self.host.switch_voltage(SignalVoltage::V180) {
                            Ok(()) => submit_sd_access_mode_switch(self, request, mode),
                            Err(err) => {
                                warn!("sdio: SD {} failed ({:?})", mode.name(), err);
                                let status = SwitchStatus::from_raw(unsafe {
                                    *request.switch_status_buf.as_ref()
                                });
                                submit_next_sd_access_mode(self, request, status)
                            }
                        }
                    }
                    Err(err) => {
                        request.command_request = None;
                        warn!("sdio: SD {} failed ({:?})", mode.name(), err);
                        let status =
                            SwitchStatus::from_raw(unsafe { *request.switch_status_buf.as_ref() });
                        submit_next_sd_access_mode(self, request, status)
                    }
                }
            }
            SdioInitState::PollSdSetAccessMode => {
                let mode = request.current_access_mode.ok_or(Error::InvalidArgument)?;
                let switch_request = request
                    .switch_function_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                match self.poll_switch_function_request(switch_request) {
                    Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                    Ok(OperationPoll::Complete(())) => {
                        request.switch_function_request = None;
                        let status =
                            SwitchStatus::from_raw(unsafe { *request.switch_status_buf.as_ref() });
                        if status.selected_function(1) != mode.function() {
                            warn!("sdio: SD {} failed (function mismatch)", mode.name());
                            submit_next_sd_access_mode(self, request, status)
                        } else {
                            self.host.set_clock(mode.clock())?;
                            if matches!(mode, SdAccessMode::Sdr50 | SdAccessMode::Sdr104) {
                                self.host.execute_tuning(19)?;
                            }
                            let status_request = self.submit_status()?;
                            request.status_request = Some(status_request);
                            request.state = SdioInitState::PollSdStatus;
                            Ok(OperationPoll::Pending)
                        }
                    }
                    Err(err) => {
                        request.switch_function_request = None;
                        warn!("sdio: SD {} failed ({:?})", mode.name(), err);
                        let status =
                            SwitchStatus::from_raw(unsafe { *request.switch_status_buf.as_ref() });
                        submit_next_sd_access_mode(self, request, status)
                    }
                }
            }
            SdioInitState::PollSdStatus => {
                let mode = request.current_access_mode.ok_or(Error::InvalidArgument)?;
                let status_request = request
                    .status_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                match self.poll_status_request(status_request)? {
                    OperationPoll::Pending => Ok(OperationPoll::Pending),
                    OperationPoll::Complete(CardState::Transfer) => {
                        request.status_request = None;
                        info!("sdio: SD speed selected {:?}", mode.clock());
                        request.state = SdioInitState::Complete;
                        Ok(OperationPoll::Pending)
                    }
                    OperationPoll::Complete(_) => {
                        request.status_request = None;
                        warn!("sdio: SD {} failed (bad status)", mode.name());
                        let status =
                            SwitchStatus::from_raw(unsafe { *request.switch_status_buf.as_ref() });
                        submit_next_sd_access_mode(self, request, status)
                    }
                }
            }
            SdioInitState::Complete => {
                let kind = request.kind.ok_or(Error::InvalidArgument)?;
                let ocr = request.ocr.ok_or(Error::InvalidArgument)?;
                info!(
                    "sdio: init done kind={:?} sd_v2={} high_capacity={} rca={:#x} ocr={:#x}",
                    kind, request.sd_v2, self.high_capacity, self.rca, ocr.raw
                );
                Ok(OperationPoll::Complete(CardInfo {
                    kind,
                    sd_v2: request.sd_v2,
                    high_capacity: self.high_capacity,
                    ocr: ocr.raw,
                    rca: self.rca,
                    capacity_blocks: request.capacity_blocks,
                    cid: request.cid,
                    ext_csd: request.parsed_ext_csd.take(),
                }))
            }
        }
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
}

fn submit_mmc_bus_width_or_continue<'a, H: SdioHost + 'a>(
    driver: &mut SdioSdmmc<H>,
    request: &mut SdioInitRequest<'a, H>,
    width: BusWidth,
) -> Result<OperationPoll<CardInfo>, Error> {
    let value: u8 = match width {
        BusWidth::Bit1 => 0,
        BusWidth::Bit4 => 1,
        BusWidth::Bit8 => 2,
    };
    request.current_bus_width = width;
    request.mmc_switch_request =
        Some(driver.submit_mmc_switch(0b11, crate::cmd::ext_csd::BUS_WIDTH as u8, value)?);
    request.state = SdioInitState::PollMmcBusWidth;
    Ok(OperationPoll::Pending)
}

fn submit_next_sd_access_mode<'a, H: SdioHost + 'a>(
    driver: &mut SdioSdmmc<H>,
    request: &mut SdioInitRequest<'a, H>,
    status: SwitchStatus,
) -> Result<OperationPoll<CardInfo>, Error> {
    let ocr = request.ocr.ok_or(Error::InvalidArgument)?;
    let candidates = if ocr.s18a() {
        &[
            SdAccessMode::Sdr104,
            SdAccessMode::Sdr50,
            SdAccessMode::Ddr50,
            SdAccessMode::HighSpeed,
        ][..]
    } else {
        &[SdAccessMode::HighSpeed][..]
    };

    while request.sd_access_index < candidates.len() {
        let mode = candidates[request.sd_access_index];
        request.sd_access_index += 1;
        if !status.access_mode_supported(mode.function()) {
            continue;
        }
        if matches!(mode, SdAccessMode::HighSpeed) {
            info!("sdio: trying SD HighSpeed");
        } else {
            info!("sdio: trying SD {}", mode.name());
        }
        return submit_sd_access_mode(driver, request, mode);
    }

    info!("sdio: SD card stayed at default speed");
    request.state = SdioInitState::Complete;
    Ok(OperationPoll::Pending)
}

fn submit_sd_access_mode<'a, H: SdioHost + 'a>(
    driver: &mut SdioSdmmc<H>,
    request: &mut SdioInitRequest<'a, H>,
    mode: SdAccessMode,
) -> Result<OperationPoll<CardInfo>, Error> {
    request.current_access_mode = Some(mode);
    if !matches!(mode, SdAccessMode::HighSpeed) && request.ocr.ok_or(Error::InvalidArgument)?.s18a()
    {
        let cmd = crate::cmd::CMD11;
        request.command_request = Some(driver.submit_command_request(&cmd)?);
        request.state = SdioInitState::PollSdVoltageSwitch;
        return Ok(OperationPoll::Pending);
    }

    submit_sd_access_mode_switch(driver, request, mode)
}

fn submit_sd_access_mode_switch<'a, H: SdioHost + 'a>(
    driver: &mut SdioSdmmc<H>,
    request: &mut SdioInitRequest<'a, H>,
    mode: SdAccessMode,
) -> Result<OperationPoll<CardInfo>, Error> {
    let buf = unsafe { request.switch_status_buf.as_mut() };
    request.switch_function_request = Some(
        driver
            .submit_switch_function(&crate::cmd::cmd6_sd_access_mode(true, mode.function()), buf)?,
    );
    request.state = SdioInitState::PollSdSetAccessMode;
    Ok(OperationPoll::Pending)
}

fn block_count_from_len(len: usize) -> Result<u32, Error> {
    if len == 0 || !len.is_multiple_of(512) {
        return Err(Error::Misaligned);
    }
    u32::try_from(len / 512).map_err(|_| Error::InvalidArgument)
}

fn sd_acmd6_arg(width: BusWidth) -> Result<u32, Error> {
    match width {
        BusWidth::Bit1 => Ok(0),
        BusWidth::Bit4 => Ok(2),
        BusWidth::Bit8 => Err(Error::UnsupportedCommand),
    }
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

    /// Mock host that replays canned responses in order. Used to verify the
    /// init sequence and that the driver tracks RCA on its own.
    struct MockHost {
        replies: Vec<Result<Response, Error>>,
        commands: Vec<Command>,
        bus_width: Option<BusWidth>,
        data_requests: Vec<(DataDirection, u32, u32)>,
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
        pending_polls: usize,
    }

    struct MockDataRequest<'a> {
        response: Option<Response>,
        _marker: core::marker::PhantomData<&'a ()>,
    }

    impl MockHost {
        fn new(replies: Vec<Response>) -> Self {
            Self {
                replies: replies.into_iter().map(Ok).collect(),
                commands: Vec::new(),
                bus_width: None,
                data_requests: Vec::new(),
                next_read_payload: None,
                read_payloads: Vec::new(),
                writes: Vec::new(),
                reject_bit8: false,
                last_clock: None,
                last_voltage: None,
                voltage_switch_result: None,
                tuning_result: None,
                last_tuning_cmd: None,
                pending_polls: 0,
            }
        }

        /// Build a host where any response slot can be a synthesized
        /// error (e.g. a CMD8 timeout to simulate an eMMC card).
        fn with_results(replies: Vec<Result<Response, Error>>) -> Self {
            Self {
                replies,
                commands: Vec::new(),
                bus_width: None,
                data_requests: Vec::new(),
                next_read_payload: None,
                read_payloads: Vec::new(),
                writes: Vec::new(),
                reject_bit8: false,
                last_clock: None,
                last_voltage: None,
                voltage_switch_result: None,
                tuning_result: None,
                last_tuning_cmd: None,
                pending_polls: 0,
            }
        }
    }

    impl SdioHost for MockHost {
        type Event = ();
        type DataRequest<'a> = MockDataRequest<'a>;

        fn submit_command(&mut self, cmd: &Command) -> Result<(), Error> {
            self.commands.push(*cmd);
            Ok(())
        }

        fn poll_command_response(&mut self) -> Result<CommandResponsePoll, Error> {
            if self.pending_polls > 0 {
                self.pending_polls -= 1;
                return Ok(CommandResponsePoll::Pending);
            }
            if self.replies.is_empty() {
                return Err(Error::Timeout(ErrorContext::default()));
            }
            self.replies.remove(0).map(CommandResponsePoll::Complete)
        }

        fn submit_read_data<'a>(
            &mut self,
            cmd: &Command,
            buf: &'a mut [u8],
            block_size: u32,
            block_count: u32,
        ) -> Result<Self::DataRequest<'a>, Error> {
            self.data_requests
                .push((DataDirection::Read, block_size, block_count));
            self.submit_command(cmd)?;
            let CommandResponsePoll::Complete(response) = self.poll_command_response()? else {
                return Err(Error::Timeout(ErrorContext::default()));
            };
            let payload = if self.read_payloads.is_empty() {
                self.next_read_payload.take()
            } else {
                Some(self.read_payloads.remove(0))
            };
            match payload {
                Some(data) if data.len() == buf.len() => {
                    buf.copy_from_slice(&data);
                    Ok(MockDataRequest {
                        response: Some(response),
                        _marker: core::marker::PhantomData,
                    })
                }
                _ => Err(Error::UnsupportedCommand),
            }
        }

        fn submit_write_data<'a>(
            &mut self,
            cmd: &Command,
            buf: &'a [u8],
            block_size: u32,
            block_count: u32,
        ) -> Result<Self::DataRequest<'a>, Error> {
            self.data_requests
                .push((DataDirection::Write, block_size, block_count));
            self.submit_command(cmd)?;
            let CommandResponsePoll::Complete(response) = self.poll_command_response()? else {
                return Err(Error::Timeout(ErrorContext::default()));
            };
            self.writes.push(buf.to_vec());
            Ok(MockDataRequest {
                response: Some(response),
                _marker: core::marker::PhantomData,
            })
        }

        fn poll_data_request<'a>(
            &mut self,
            request: &mut Self::DataRequest<'a>,
        ) -> Result<DataCommandPoll, Error> {
            request
                .response
                .take()
                .map(DataCommandPoll::Complete)
                .ok_or(Error::InvalidArgument)
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

    #[test]
    fn sdio_host_irq_methods_default_to_noop() {
        let mut host = MockHost::new(Vec::new());

        assert_eq!(host.enable_completion_irq(), Ok(()));
        assert_eq!(host.disable_completion_irq(), Ok(()));
        assert_eq!(host.handle_irq(), ());
    }

    #[test]
    fn unit_irq_event_reports_no_runtime_action() {
        let event = ();

        assert_eq!(event.kind(), HostEventKind::None);
        assert_eq!(event.source(), HostEventSource::Controller);
        assert_eq!(event.queue_id(), None);
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

    fn disable_speed_selection(driver: &mut SdioSdmmc<MockHost>) {
        driver.set_sd_speed_selection_enabled(false);
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

    fn poll_init_to_completion<H: SdioHost>(driver: &mut SdioSdmmc<H>) -> Result<CardInfo, Error> {
        poll_init_to_completion_with_preference(driver, CardInitPreference::SdFirst)
    }

    fn poll_init_to_completion_with_preference<H: SdioHost>(
        driver: &mut SdioSdmmc<H>,
        preference: CardInitPreference,
    ) -> Result<CardInfo, Error> {
        let mut scratch = SdioInitScratch::new();
        let mut request = driver.submit_init_with_preference(preference, &mut scratch)?;
        loop {
            match driver.poll_init_request(&mut request)? {
                OperationPoll::Pending => {}
                OperationPoll::Complete(info) => return Ok(info),
            }
        }
    }

    #[test]
    fn init_records_rca_in_driver_state() {
        let replies = sd_init_replies();
        let host = MockHost::with_results(replies);
        let mut driver = SdioSdmmc::new(host);
        disable_speed_selection(&mut driver);
        let info = poll_init_to_completion(&mut driver).unwrap();

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
    fn submit_init_starts_request_without_spinning_past_pending_cmd0() {
        let mut host = MockHost::with_results(std::vec![Ok(ok_r1())]);
        host.pending_polls = 1;
        let mut driver = SdioSdmmc::new(host);
        let mut scratch = SdioInitScratch::new();
        let mut request = driver.submit_init(&mut scratch).unwrap();

        assert_eq!(
            driver
                .host
                .commands
                .iter()
                .map(|cmd| cmd.cmd)
                .collect::<Vec<_>>(),
            std::vec![0]
        );
        assert!(matches!(
            driver.poll_init_request(&mut request).unwrap(),
            OperationPoll::Pending
        ));
        assert_eq!(
            driver
                .host
                .commands
                .iter()
                .map(|cmd| cmd.cmd)
                .collect::<Vec<_>>(),
            std::vec![0]
        );
    }

    #[test]
    fn poll_init_request_returns_after_submitting_next_command() {
        let mut driver = SdioSdmmc::new(MockHost::with_results(std::vec![
            Ok(ok_r1()),                                             // CMD0
            Ok(Response::R7(IfCondResponse::from_raw(0x0000_01AA))), // CMD8
        ]));
        let mut scratch = SdioInitScratch::new();
        let mut request = driver.submit_init(&mut scratch).unwrap();

        assert!(matches!(
            driver.poll_init_request(&mut request).unwrap(),
            OperationPoll::Pending
        ));
        assert_eq!(
            driver
                .host
                .commands
                .iter()
                .map(|cmd| cmd.cmd)
                .collect::<Vec<_>>(),
            std::vec![0, 8]
        );

        assert!(matches!(
            driver.poll_init_request(&mut request).unwrap(),
            OperationPoll::Pending
        ));
        assert_eq!(
            driver
                .host
                .commands
                .iter()
                .map(|cmd| cmd.cmd)
                .collect::<Vec<_>>(),
            std::vec![0, 8, 55]
        );
    }

    #[test]
    fn poll_init_request_falls_back_to_cmd1_after_acmd41_not_ready_timeout() {
        let mut driver = SdioSdmmc::new(MockHost::with_results(std::vec![
            Ok(Response::R3(OcrResponse::from_raw(0x00FF_8000))),
            Ok(ok_r1()),
        ]));
        let mut scratch = SdioInitScratch::new();
        let mut request = SdioInitRequest::new(CardInitPreference::SdFirst, &mut scratch);
        request.state = SdioInitState::PollAcmd41;
        request.sd_v2 = false;
        request.acmd41_elapsed_ms = SdioInitTiming::TIMEOUT_MS;

        assert!(matches!(
            driver.poll_init_request(&mut request).unwrap(),
            OperationPoll::Pending
        ));
        assert_eq!(
            driver
                .host
                .commands
                .iter()
                .map(|cmd| cmd.cmd)
                .collect::<Vec<_>>(),
            std::vec![1]
        );
    }

    #[test]
    fn submit_init_with_mmc_preference_skips_sd_probe_after_cmd0() {
        let mut driver = SdioSdmmc::new(MockHost::with_results(std::vec![Ok(ok_r1())]));
        let mut scratch = SdioInitScratch::new();
        let mut request = driver
            .submit_init_with_preference(CardInitPreference::MmcFirst, &mut scratch)
            .unwrap();

        assert!(matches!(
            driver.poll_init_request(&mut request).unwrap(),
            OperationPoll::Pending
        ));
        assert_eq!(
            driver
                .host
                .commands
                .iter()
                .map(|cmd| cmd.cmd)
                .collect::<Vec<_>>(),
            std::vec![0, 1]
        );
    }

    #[test]
    fn submit_mmc_switch_returns_before_polling_status() {
        let mut driver = SdioSdmmc::new(MockHost::with_results(std::vec![
            Ok(ok_r1()),         // CMD6
            Ok(r1_tran_ready()), // CMD13
        ]));
        driver.rca = 1;

        let mut request = driver
            .submit_mmc_switch(0b11, crate::cmd::ext_csd::HS_TIMING as u8, 1)
            .unwrap();
        assert_eq!(
            driver
                .host
                .commands
                .iter()
                .map(|cmd| cmd.cmd)
                .collect::<Vec<_>>(),
            std::vec![6]
        );

        assert!(matches!(
            driver.poll_mmc_switch_request(&mut request).unwrap(),
            OperationPoll::Pending
        ));
        assert_eq!(
            driver
                .host
                .commands
                .iter()
                .map(|cmd| cmd.cmd)
                .collect::<Vec<_>>(),
            std::vec![6, 13]
        );

        assert!(matches!(
            driver.poll_mmc_switch_request(&mut request).unwrap(),
            OperationPoll::Complete(())
        ));
    }

    #[test]
    fn submit_status_returns_before_polling_cmd13_response() {
        let mut driver = SdioSdmmc::new(MockHost::with_results(std::vec![Ok(r1_tran_ready())]));
        driver.rca = 0x1234;

        let mut request = driver.submit_status().unwrap();
        assert_eq!(
            driver
                .host
                .commands
                .iter()
                .map(|cmd| cmd.cmd)
                .collect::<Vec<_>>(),
            std::vec![13]
        );
        assert_eq!(driver.host.commands[0].arg, 0x1234 << 16);

        assert!(matches!(
            driver.poll_status_request(&mut request).unwrap(),
            OperationPoll::Complete(CardState::Transfer)
        ));
    }

    #[test]
    fn submit_read_ext_csd_uses_caller_buffer_and_poll_completion() {
        let mut host = MockHost::new(std::vec![ok_r1()]);
        let payload = ext_csd_blob();
        host.next_read_payload = Some(payload.clone());
        let mut driver = SdioSdmmc::new(host);
        let mut buf = [0u8; 512];

        let mut request = driver.submit_read_ext_csd(&mut buf).unwrap();
        assert_eq!(
            driver
                .host
                .commands
                .iter()
                .map(|cmd| cmd.cmd)
                .collect::<Vec<_>>(),
            std::vec![8]
        );

        assert!(matches!(
            driver.poll_ext_csd_request(&mut request).unwrap(),
            OperationPoll::Complete(())
        ));
        drop(request);
        assert_eq!(&buf[..], payload.as_slice());
    }

    #[test]
    fn submit_switch_function_uses_caller_buffer_and_poll_completion() {
        let mut host = MockHost::new(std::vec![ok_r1()]);
        let payload = switch_status_payload(1, 1 << 1);
        host.next_read_payload = Some(payload.clone());
        let mut driver = SdioSdmmc::new(host);
        let mut buf = [0u8; 64];

        let mut request = driver
            .submit_switch_function(&crate::cmd::cmd6_high_speed(true), &mut buf)
            .unwrap();
        assert_eq!(
            driver
                .host
                .commands
                .iter()
                .map(|cmd| cmd.cmd)
                .collect::<Vec<_>>(),
            std::vec![6]
        );

        assert!(matches!(
            driver.poll_switch_function_request(&mut request).unwrap(),
            OperationPoll::Complete(())
        ));
        drop(request);
        assert_eq!(&buf[..], payload.as_slice());
    }

    #[test]
    fn poll_init_request_skips_pace_hint_on_ready_path() {
        let replies = sd_init_replies();
        let host = MockHost::with_results(replies);
        let mut driver = SdioSdmmc::new(host);
        disable_speed_selection(&mut driver);
        let mut scratch = SdioInitScratch::new();
        let mut request = driver.submit_init(&mut scratch).unwrap();
        let mut pace_hints = 0;
        let info = loop {
            match driver.poll_init_request(&mut request).unwrap() {
                OperationPoll::Pending => {
                    if request.take_needs_pace() {
                        pace_hints += 1;
                    }
                }
                OperationPoll::Complete(info) => break info,
            }
        };

        assert_eq!(info.rca, 0x1234);
        assert_eq!(pace_hints, 0);
    }

    #[test]
    fn poll_init_request_sets_pace_hint_for_power_up_retry() {
        let replies = std::vec![
            Ok(ok_r1()),                                             // CMD0
            Ok(Response::R7(IfCondResponse::from_raw(0x0000_01AA))), // CMD8
            Ok(ok_r1()),                                             // CMD55
            Ok(Response::R3(OcrResponse::from_raw(0x00FF_8000))),    // ACMD41 not ready
            Ok(ok_r1()),                                             // CMD55
            Ok(ocr_ready_sdhc()),                                    // ACMD41 ready
            Ok(cid_response()),                                      // CMD2
            Ok(rca_response(0x1234)),                                // CMD3
            Ok(csd_v2_response()),                                   // CMD9
            Ok(ok_r1()),                                             // CMD7
            Ok(ok_r1()),                                             // CMD55
            Ok(ok_r1()),                                             // ACMD6
        ];
        let host = MockHost::with_results(replies);
        let mut driver = SdioSdmmc::new(host);
        disable_speed_selection(&mut driver);
        let mut scratch = SdioInitScratch::new();
        let mut request = driver.submit_init(&mut scratch).unwrap();
        let mut power_up_retries = 0;
        let info = loop {
            match driver.poll_init_request(&mut request).unwrap() {
                OperationPoll::Pending => {
                    if request.take_needs_pace() {
                        power_up_retries += 1;
                    }
                }
                OperationPoll::Complete(info) => break info,
            }
        };

        assert_eq!(info.rca, 0x1234);
        assert_eq!(power_up_retries, 1);
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

        let mut driver = SdioSdmmc::new(host);
        poll_init_to_completion(&mut driver).expect("SD init succeeds with SDR104");

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

        let mut driver = SdioSdmmc::new(host);
        poll_init_to_completion(&mut driver)
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
        let mut driver = SdioSdmmc::new(host);
        driver.set_sd_speed_selection_enabled(false);

        poll_init_to_completion(&mut driver)
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
        let mut driver = SdioSdmmc::new(host);
        let info = poll_init_to_completion(&mut driver).expect("eMMC init succeeds");

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
        let mut driver = SdioSdmmc::new(host);
        let _info =
            poll_init_to_completion(&mut driver).expect("eMMC init succeeds with 4-bit fallback");

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
        let mut driver = SdioSdmmc::new(host);
        disable_speed_selection(&mut driver);
        let info = poll_init_to_completion(&mut driver).expect("SD v1 init succeeds");

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
        let mut driver = SdioSdmmc::new(host);
        let _info = poll_init_to_completion(&mut driver).expect("HS200 init succeeds");

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
        let mut driver = SdioSdmmc::new(host);
        let _info = poll_init_to_completion(&mut driver)
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
        assert_eq!(sd_acmd6_arg(BusWidth::Bit8), Err(Error::UnsupportedCommand));
    }

    #[test]
    fn submit_read_blocks_into_leaves_multi_block_stop_to_host_request() {
        let mut host = MockHost::new(std::vec![ok_r1()]);
        let expected: Vec<u8> = (0..1024).map(|i| (i % 251) as u8).collect();
        host.next_read_payload = Some(expected.clone());

        let mut driver = SdioSdmmc::new(host);
        driver.high_capacity = true;
        let mut buf = [0u8; 1024];

        let mut request = driver.submit_read_blocks_into(7, &mut buf).unwrap();
        assert!(matches!(
            driver.poll_data_request(&mut request).unwrap(),
            DataCommandPoll::Complete(_)
        ));

        assert_eq!(&buf[..], &expected[..]);
        assert_eq!(
            driver.host.data_requests,
            std::vec![(DataDirection::Read, 512, 2)]
        );
        assert_eq!(
            driver
                .host
                .commands
                .iter()
                .map(|c| c.cmd)
                .collect::<Vec<_>>(),
            std::vec![18]
        );
        assert_eq!(driver.host.commands[0].arg, 7);
    }

    #[test]
    fn submit_write_blocks_from_leaves_multi_block_stop_to_host_request() {
        let host = MockHost::new(std::vec![ok_r1()]);
        let mut driver = SdioSdmmc::new(host);
        driver.high_capacity = true;
        let buf = [0x5au8; 1024];

        let mut request = driver.submit_write_blocks_from(11, &buf).unwrap();
        assert!(matches!(
            driver.poll_data_request(&mut request).unwrap(),
            DataCommandPoll::Complete(_)
        ));

        assert_eq!(
            driver.host.data_requests,
            std::vec![(DataDirection::Write, 512, 2)]
        );
        assert_eq!(
            driver
                .host
                .commands
                .iter()
                .map(|c| c.cmd)
                .collect::<Vec<_>>(),
            std::vec![25]
        );
        assert_eq!(driver.host.commands[0].arg, 11);
        assert_eq!(driver.host.writes, std::vec![buf.to_vec()]);
    }

    #[test]
    fn submit_block_io_rejects_misaligned_buffers() {
        let host = MockHost::new(std::vec![]);
        let mut driver = SdioSdmmc::new(host);
        let mut read_buf = [0u8; 513];
        let write_buf = [0u8; 513];

        assert_eq!(
            driver.submit_read_blocks_into(0, &mut read_buf).map(|_| ()),
            Err(Error::Misaligned)
        );
        assert_eq!(
            driver.submit_write_blocks_from(0, &write_buf).map(|_| ()),
            Err(Error::Misaligned)
        );
        assert!(driver.host.commands.is_empty());
    }
}
