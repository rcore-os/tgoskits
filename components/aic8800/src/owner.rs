//! Single-owner AIC8800 controller lifecycle.
//!
//! The portable core never owns a task or waker. The OS moves this value into
//! one CPU-pinned maintenance thread, registers the transferred IRQ endpoint
//! there, and feeds one acknowledged event into each bounded state-machine
//! activation.

use alloc::{boxed::Box, collections::VecDeque, vec::Vec};

use dma_api::DeviceDma;
use rdif_eth::{
    ActiveQueueSet, BIrqEndpoint, DmaBuffer, EthernetIrqFault, Event, InitIrqSources, IrqCapture,
    IrqEndpoint, IrqSourceControl, MaskedSource, NetDeviceOwner, NetError, OwnerInitInput,
    OwnerInitPoll, OwnerInitSchedule, QueueConfig, QueueMemoryMode, RxQueueOwner, TxQueueOwner,
    WifiCommand, WifiCommandProgress, WifiCommandResult, WifiCommandSchedule,
    WifiCommandStartError, WifiLinkPolicy,
};
use sdmmc_protocol::sdio::{HostEvent, host2::SdioHost2Timed};

use crate::{
    common::{
        ChipVariant, SDIOWIFI_BLOCK_CNT_REG, SDIOWIFI_BYTEMODE_ENABLE_REG,
        SDIOWIFI_INTR_CONFIG_REG, SDIOWIFI_RD_FIFO_ADDR, SDIOWIFI_REGISTER_BLOCK,
        SDIOWIFI_WR_FIFO_ADDR,
    },
    data::{DataWireError, build_tx_frame, decode_rx_aggregate},
    firmware::{self, FirmwareMachine, FirmwarePlan, FirmwarePoll},
    softap::{
        LmacRequest, SoftApPolicy, add_ap_interface_request, beacon_request,
        channel_config_request, filter_request, get_mac_request, me_config_request, reset_request,
        rf_calibration_request, stack_start_request, start_ap_request, start_mac_request,
    },
    transport::{SdioOperation, SdioTransactionEngine, TransactionError, TransactionPoll, r5_data},
    wire::{WireError, build_lmac_frame, parse_confirmation},
};

const DEVICE_NAME: &str = "aic8800-wifi";
const SDIO_COMPLETION_SOURCE: u64 = 1 << 0;
const SDIO_CARD_SOURCE: u64 = 1 << 1;
const AIC_INIT_IRQ_SOURCES: InitIrqSources =
    InitIrqSources::from_bits(SDIO_COMPLETION_SOURCE | SDIO_CARD_SOURCE);
const REQUEST_TIMEOUT_NS: u64 = 1_000_000_000;
const COMMAND_TIMEOUT_NS: u64 = 6_000_000_000;
const OCR_RETRY_NS: u64 = 10_000_000;
const FIRMWARE_SETTLE_NS: u64 = 200_000_000;
const TX_QUEUE_ID: usize = 0;
const RX_QUEUE_ID: usize = 0;
const DATA_QUEUE_CAPACITY: usize = 15;
const OWNER_EVENT_CAPACITY: usize = 64;
const AIC_CARD_FUNCTION_INTERRUPT_STATUS: u64 = 1 << 63;

/// Pure discovery policy supplied by board glue.
#[derive(Clone, Copy, Debug)]
pub struct AicDiscoveryConfig {
    mac_address: [u8; 6],
    link_policy: Option<WifiLinkPolicy>,
    chip: ChipVariant,
    soft_ap: Option<SoftApPolicy>,
}

impl AicDiscoveryConfig {
    /// Creates policy without touching the controller or card.
    pub const fn new(mac_address: [u8; 6], link_policy: Option<WifiLinkPolicy>) -> Self {
        Self {
            mac_address,
            link_policy,
            chip: ChipVariant::Aic8800DC,
            soft_ap: None,
        }
    }

    pub const fn with_chip(mut self, chip: ChipVariant) -> Self {
        self.chip = chip;
        self
    }

    pub const fn with_soft_ap(mut self, soft_ap: SoftApPolicy) -> Self {
        self.soft_ap = Some(soft_ap);
        self
    }
}

/// Public lifecycle phase for diagnostics and deterministic model tests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AicOwnerPhase {
    Discovered,
    ControllerInit,
    FirmwareLoad,
    FirmwareBoot,
    Configure,
    StartLink,
    Ready,
    Failed,
}

/// Failure of the portable owner state machine.
#[derive(Debug, thiserror::Error)]
pub enum AicError {
    #[error("SDIO host did not transfer an IRQ endpoint")]
    MissingIrqSource,
    #[error("unsupported AIC8800 chip variant")]
    UnsupportedChip,
    #[error("no startup wireless policy was provided")]
    MissingStartupPolicy,
    #[error("SDIO transaction failed: {0}")]
    Transaction(#[from] TransactionError),
    #[error("AIC8800 wire protocol failed: {0}")]
    Wire(#[source] WireError),
    #[error("LMAC confirmation mismatch: expected {expected:#06x}, got {actual:#06x}")]
    ConfirmationMismatch { expected: u16, actual: u16 },
    #[error("AIC8800 owner state {phase:?} timed out")]
    Timeout { phase: AicOwnerPhase },
    #[error("AIC8800 firmware rejected {operation} with status {status}")]
    FirmwareRejected { operation: &'static str, status: u8 },
    #[error("AIC8800 firmware initialization failed: {0}")]
    Firmware(#[from] firmware::FirmwareError),
    #[error("AIC8800 confirmation for {0} is truncated")]
    InvalidConfirmation(&'static str),
    #[error("SDIO host IRQ routing failed: {0}")]
    IrqHost(#[source] sdmmc_protocol::Error),
    #[error("AIC8800 data queue support is unavailable")]
    QueueUnavailable,
    #[error("AIC8800 packet wire protocol failed: {0}")]
    DataWire(#[from] DataWireError),
}

impl From<WireError> for AicError {
    fn from(error: WireError) -> Self {
        match error {
            WireError::ConfirmationMismatch { expected, actual } => {
                Self::ConfirmationMismatch { expected, actual }
            }
            other => Self::Wire(other),
        }
    }
}

trait TransactionPort {
    fn submit(&mut self, operation: SdioOperation) -> Result<(), TransactionError>;
    fn poll(&mut self, now_ns: u64) -> Result<TransactionPoll, TransactionError>;
    fn is_active(&self) -> bool;
}

impl<H> TransactionPort for SdioTransactionEngine<H>
where
    H: SdioHost2Timed + 'static,
{
    fn submit(&mut self, operation: SdioOperation) -> Result<(), TransactionError> {
        SdioTransactionEngine::submit(self, operation)
    }

    fn poll(&mut self, now_ns: u64) -> Result<TransactionPoll, TransactionError> {
        SdioTransactionEngine::poll(self, now_ns)
    }

    fn is_active(&self) -> bool {
        SdioTransactionEngine::is_active(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CfmStage {
    Probe,
    CountActive,
    ReadReady(u8),
    ReadActive,
}

enum AicCommandState {
    Idle,
    Submitting {
        expected: u16,
        deadline_ns: u64,
    },
    WaitingCfm {
        expected: u16,
        deadline_ns: u64,
        stage: CfmStage,
    },
    Completing,
}

struct AicCommandEngine {
    chip: ChipVariant,
    state: AicCommandState,
}

#[derive(Debug)]
enum CommandProgress {
    Pending(OwnerInitSchedule),
    Ready(Vec<u8>),
}

impl AicCommandEngine {
    fn new(chip: ChipVariant) -> Self {
        Self {
            chip,
            state: AicCommandState::Idle,
        }
    }

    fn is_idle(&self) -> bool {
        matches!(self.state, AicCommandState::Idle)
    }

    fn command_function(&self) -> u8 {
        if matches!(self.chip, ChipVariant::Aic8800DC | ChipVariant::Aic8800DW) {
            2
        } else {
            1
        }
    }

    fn write_fifo(&self) -> u32 {
        if self.chip.is_v3() {
            0x10
        } else {
            SDIOWIFI_WR_FIFO_ADDR
        }
    }

    fn read_fifo(&self) -> u32 {
        if self.chip.is_v3() {
            0x0f
        } else {
            SDIOWIFI_RD_FIFO_ADDR
        }
    }

    fn block_count(&self) -> u32 {
        if self.chip.is_v3() {
            0x04
        } else {
            SDIOWIFI_BLOCK_CNT_REG
        }
    }

    fn start<P: TransactionPort>(
        &mut self,
        port: &mut P,
        request: LmacRequest,
        now_ns: u64,
    ) -> Result<OwnerInitSchedule, AicError> {
        if !self.is_idle() || port.is_active() {
            return Err(TransactionError::Busy.into());
        }
        let frame = build_lmac_frame(
            self.chip,
            request.message_id,
            request.destination,
            &request.payload,
        )?;
        port.submit(SdioOperation::WriteBlocks {
            function: self.command_function(),
            address: self.write_fifo(),
            increment: false,
            bytes: frame,
        })?;
        let deadline_ns = now_ns.saturating_add(COMMAND_TIMEOUT_NS);
        self.state = AicCommandState::Submitting {
            expected: request.message_id.wrapping_add(1),
            deadline_ns,
        };
        Ok(OwnerInitSchedule::wait_for_irq_until(
            AIC_INIT_IRQ_SOURCES,
            deadline_ns,
        ))
    }

    fn poll<P: TransactionPort>(
        &mut self,
        port: &mut P,
        input: OwnerInitInput,
    ) -> Result<CommandProgress, AicError> {
        let state = core::mem::replace(&mut self.state, AicCommandState::Completing);
        match state {
            AicCommandState::Idle | AicCommandState::Completing => {
                self.state = AicCommandState::Idle;
                Err(TransactionError::InvalidOperation.into())
            }
            AicCommandState::Submitting {
                expected,
                deadline_ns,
            } => {
                if input.event.is_none() {
                    self.state = AicCommandState::Submitting {
                        expected,
                        deadline_ns,
                    };
                    return self.wait_or_timeout(input.now_ns, deadline_ns);
                }
                match port.poll(input.now_ns)? {
                    TransactionPoll::Pending { wake_at_ns } => {
                        self.state = AicCommandState::Submitting {
                            expected,
                            deadline_ns,
                        };
                        Ok(CommandProgress::Pending(wait_schedule(
                            wake_at_ns,
                            deadline_ns,
                        )))
                    }
                    TransactionPoll::Ready(_) => {
                        self.state = AicCommandState::WaitingCfm {
                            expected,
                            deadline_ns,
                            stage: CfmStage::Probe,
                        };
                        Ok(CommandProgress::Pending(OwnerInitSchedule::run_again()))
                    }
                }
            }
            AicCommandState::WaitingCfm {
                expected,
                deadline_ns,
                stage,
            } => {
                if input.now_ns >= deadline_ns {
                    self.state = AicCommandState::Idle;
                    return Err(AicError::Timeout {
                        phase: AicOwnerPhase::Configure,
                    });
                }
                match stage {
                    CfmStage::Probe => {
                        port.submit(SdioOperation::ReadByte {
                            function: self.command_function(),
                            address: self.block_count(),
                        })?;
                        self.state = AicCommandState::WaitingCfm {
                            expected,
                            deadline_ns,
                            stage: CfmStage::CountActive,
                        };
                        Ok(CommandProgress::Pending(
                            OwnerInitSchedule::wait_for_irq_until(
                                AIC_INIT_IRQ_SOURCES,
                                deadline_ns,
                            ),
                        ))
                    }
                    CfmStage::CountActive => {
                        if input.event.is_none() {
                            self.state = AicCommandState::WaitingCfm {
                                expected,
                                deadline_ns,
                                stage,
                            };
                            return self.wait_or_timeout(input.now_ns, deadline_ns);
                        }
                        match port.poll(input.now_ns)? {
                            TransactionPoll::Pending { wake_at_ns } => {
                                self.state = AicCommandState::WaitingCfm {
                                    expected,
                                    deadline_ns,
                                    stage,
                                };
                                Ok(CommandProgress::Pending(wait_schedule(
                                    wake_at_ns,
                                    deadline_ns,
                                )))
                            }
                            TransactionPoll::Ready(completion) => {
                                let blocks = r5_data(completion.response)? & 0x7f;
                                if blocks == 0 {
                                    self.state = AicCommandState::WaitingCfm {
                                        expected,
                                        deadline_ns,
                                        stage: CfmStage::Probe,
                                    };
                                    return Ok(CommandProgress::Pending(
                                        OwnerInitSchedule::wait_for_irq_until(
                                            AIC_INIT_IRQ_SOURCES,
                                            deadline_ns,
                                        ),
                                    ));
                                }
                                self.state = AicCommandState::WaitingCfm {
                                    expected,
                                    deadline_ns,
                                    stage: CfmStage::ReadReady(blocks),
                                };
                                Ok(CommandProgress::Pending(OwnerInitSchedule::run_again()))
                            }
                        }
                    }
                    CfmStage::ReadReady(blocks) => {
                        port.submit(SdioOperation::ReadBlocks {
                            function: self.command_function(),
                            address: self.read_fifo(),
                            increment: false,
                            blocks: u16::from(blocks),
                        })?;
                        self.state = AicCommandState::WaitingCfm {
                            expected,
                            deadline_ns,
                            stage: CfmStage::ReadActive,
                        };
                        Ok(CommandProgress::Pending(
                            OwnerInitSchedule::wait_for_irq_until(
                                AIC_INIT_IRQ_SOURCES,
                                deadline_ns,
                            ),
                        ))
                    }
                    CfmStage::ReadActive => {
                        if input.event.is_none() {
                            self.state = AicCommandState::WaitingCfm {
                                expected,
                                deadline_ns,
                                stage,
                            };
                            return self.wait_or_timeout(input.now_ns, deadline_ns);
                        }
                        match port.poll(input.now_ns)? {
                            TransactionPoll::Pending { wake_at_ns } => {
                                self.state = AicCommandState::WaitingCfm {
                                    expected,
                                    deadline_ns,
                                    stage,
                                };
                                Ok(CommandProgress::Pending(wait_schedule(
                                    wake_at_ns,
                                    deadline_ns,
                                )))
                            }
                            TransactionPoll::Ready(completion) => {
                                let payload =
                                    parse_confirmation(&completion.bytes, expected)?.to_vec();
                                self.state = AicCommandState::Idle;
                                Ok(CommandProgress::Ready(payload))
                            }
                        }
                    }
                }
            }
        }
    }

    fn wait_or_timeout(&self, now_ns: u64, deadline_ns: u64) -> Result<CommandProgress, AicError> {
        if now_ns >= deadline_ns {
            Err(AicError::Timeout {
                phase: AicOwnerPhase::Configure,
            })
        } else {
            Ok(CommandProgress::Pending(
                OwnerInitSchedule::wait_for_irq_until(AIC_INIT_IRQ_SOURCES, deadline_ns),
            ))
        }
    }
}

fn wait_schedule(wake_at_ns: Option<u64>, watchdog_ns: u64) -> OwnerInitSchedule {
    OwnerInitSchedule::wait_for_irq_until(
        AIC_INIT_IRQ_SOURCES,
        wake_at_ns.map_or(watchdog_ns, |wake| wake.min(watchdog_ns)),
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ControllerStep {
    Reset,
    Power,
    IdentificationClock,
    GoIdle,
    ProbeOcr,
    AssignRca,
    SelectCard,
    EnableFunctions,
    Function1BlockLow,
    Function1BlockHigh,
    Function2BlockLow,
    Function2BlockHigh,
    CardBusWidth,
    HostBusWidth,
    DefaultClock,
    Function2BlockMode,
    Function2ByteMode,
    Function2Irq,
    Function1BlockMode,
    Function1ByteMode,
    Function1Irq,
    Done,
}

impl ControllerStep {
    fn next(self) -> Self {
        use ControllerStep::*;
        match self {
            Reset => Power,
            Power => IdentificationClock,
            IdentificationClock => GoIdle,
            GoIdle => ProbeOcr,
            ProbeOcr => AssignRca,
            AssignRca => SelectCard,
            SelectCard => EnableFunctions,
            EnableFunctions => Function1BlockLow,
            Function1BlockLow => Function1BlockHigh,
            Function1BlockHigh => Function2BlockLow,
            Function2BlockLow => Function2BlockHigh,
            Function2BlockHigh => CardBusWidth,
            CardBusWidth => HostBusWidth,
            HostBusWidth => DefaultClock,
            DefaultClock => Function2BlockMode,
            Function2BlockMode => Function2ByteMode,
            Function2ByteMode => Function2Irq,
            Function2Irq => Function1BlockMode,
            Function1BlockMode => Function1ByteMode,
            Function1ByteMode => Function1Irq,
            Function1Irq | Done => Done,
        }
    }
}

struct ControllerInit {
    step: ControllerStep,
    active: bool,
    rca: u16,
    deadline_ns: u64,
    retry_at_ns: Option<u64>,
}

enum StateProgress {
    Pending(OwnerInitSchedule),
    Ready,
}

impl ControllerInit {
    const fn new() -> Self {
        Self {
            step: ControllerStep::Reset,
            active: false,
            rca: 0,
            deadline_ns: 0,
            retry_at_ns: None,
        }
    }

    fn poll<P: TransactionPort>(
        &mut self,
        port: &mut P,
        input: OwnerInitInput,
    ) -> Result<StateProgress, AicError> {
        if self.step == ControllerStep::Done {
            return Ok(StateProgress::Ready);
        }
        if let Some(retry_at) = self.retry_at_ns {
            if input.now_ns < retry_at {
                return Ok(StateProgress::Pending(OwnerInitSchedule::wait_until(
                    retry_at,
                )));
            }
            self.retry_at_ns = None;
        }
        if !self.active {
            port.submit(self.operation())?;
            self.active = true;
            self.deadline_ns = input.now_ns.saturating_add(REQUEST_TIMEOUT_NS);
            return Ok(StateProgress::Pending(
                OwnerInitSchedule::wait_for_irq_until(AIC_INIT_IRQ_SOURCES, self.deadline_ns),
            ));
        }

        if input.event.is_none()
            && !matches!(
                self.step,
                ControllerStep::Reset
                    | ControllerStep::Power
                    | ControllerStep::IdentificationClock
                    | ControllerStep::HostBusWidth
                    | ControllerStep::DefaultClock
            )
        {
            if input.now_ns >= self.deadline_ns {
                return Err(AicError::Timeout {
                    phase: AicOwnerPhase::ControllerInit,
                });
            }
            return Ok(StateProgress::Pending(
                OwnerInitSchedule::wait_for_irq_until(AIC_INIT_IRQ_SOURCES, self.deadline_ns),
            ));
        }

        let completion = match port.poll(input.now_ns)? {
            TransactionPoll::Pending { wake_at_ns } => {
                return Ok(StateProgress::Pending(wait_schedule(
                    wake_at_ns,
                    self.deadline_ns,
                )));
            }
            TransactionPoll::Ready(completion) => completion,
        };
        self.active = false;
        match self.step {
            ControllerStep::ProbeOcr if completion.response.words[0] & (1 << 31) == 0 => {
                self.retry_at_ns = Some(input.now_ns.saturating_add(OCR_RETRY_NS));
                return Ok(StateProgress::Pending(OwnerInitSchedule::wait_until(
                    self.retry_at_ns.unwrap(),
                )));
            }
            ControllerStep::AssignRca => {
                self.rca = (completion.response.words[0] >> 16) as u16;
                if self.rca == 0 {
                    return Err(AicError::InvalidConfirmation("CMD3 RCA"));
                }
            }
            ControllerStep::EnableFunctions
            | ControllerStep::Function1BlockLow
            | ControllerStep::Function1BlockHigh
            | ControllerStep::Function2BlockLow
            | ControllerStep::Function2BlockHigh
            | ControllerStep::CardBusWidth
            | ControllerStep::Function2BlockMode
            | ControllerStep::Function2ByteMode
            | ControllerStep::Function2Irq
            | ControllerStep::Function1BlockMode
            | ControllerStep::Function1ByteMode
            | ControllerStep::Function1Irq => {
                r5_data(completion.response)?;
            }
            _ => {}
        }
        self.step = self.step.next();
        if self.step == ControllerStep::Done {
            Ok(StateProgress::Ready)
        } else {
            Ok(StateProgress::Pending(OwnerInitSchedule::run_again()))
        }
    }

    fn operation(&self) -> SdioOperation {
        use ControllerStep::*;
        match self.step {
            Reset => SdioOperation::Bus(sdio_host2::BusOp::ResetAll),
            Power => SdioOperation::Bus(sdio_host2::BusOp::PowerOn),
            IdentificationClock => SdioOperation::Bus(sdio_host2::BusOp::SetClock(
                sdio_host2::ClockSpeed::Identification,
            )),
            GoIdle => SdioOperation::Command(sdio_host2::Command::new(
                0,
                0,
                sdio_host2::ResponseType::None,
            )),
            ProbeOcr => SdioOperation::Command(sdio_host2::Command::new(
                5,
                0x00ff_8000,
                sdio_host2::ResponseType::R4,
            )),
            AssignRca => {
                SdioOperation::Command(sdio_host2::Command::new(3, 0, sdio_host2::ResponseType::R6))
            }
            SelectCard => SdioOperation::Command(sdio_host2::Command::new(
                7,
                u32::from(self.rca) << 16,
                sdio_host2::ResponseType::R1b,
            )),
            EnableFunctions => write_byte(0, 0x02, 0x06),
            Function1BlockLow => write_byte(0, 0x110, 0),
            Function1BlockHigh => write_byte(0, 0x111, 2),
            Function2BlockLow => write_byte(0, 0x210, 0),
            Function2BlockHigh => write_byte(0, 0x211, 2),
            CardBusWidth => write_byte(0, 0x07, 2),
            HostBusWidth => {
                SdioOperation::Bus(sdio_host2::BusOp::SetBusWidth(sdio_host2::BusWidth::Bit4))
            }
            DefaultClock => {
                SdioOperation::Bus(sdio_host2::BusOp::SetClock(sdio_host2::ClockSpeed::Default))
            }
            Function2BlockMode => write_byte(2, SDIOWIFI_REGISTER_BLOCK, 1),
            Function2ByteMode => write_byte(2, SDIOWIFI_BYTEMODE_ENABLE_REG, 1),
            Function2Irq => write_byte(2, SDIOWIFI_INTR_CONFIG_REG, 7),
            Function1BlockMode => write_byte(1, SDIOWIFI_REGISTER_BLOCK, 1),
            Function1ByteMode => write_byte(1, SDIOWIFI_BYTEMODE_ENABLE_REG, 1),
            Function1Irq => write_byte(1, SDIOWIFI_INTR_CONFIG_REG, 7),
            Done => unreachable!("completed controller initialization has no operation"),
        }
    }
}

fn write_byte(function: u8, address: u32, value: u8) -> SdioOperation {
    SdioOperation::WriteByte {
        function,
        address,
        value,
    }
}

#[derive(Clone, Copy)]
struct RuntimePacketBuffer {
    virt: usize,
    bus_addr: u64,
    len: usize,
}

impl From<DmaBuffer> for RuntimePacketBuffer {
    fn from(buffer: DmaBuffer) -> Self {
        Self {
            virt: buffer.virt.as_ptr() as usize,
            bus_addr: buffer.bus_addr,
            len: buffer.len,
        }
    }
}

enum DataIoState {
    Idle,
    Transmitting { bus_addr: u64 },
    ReadingCount,
    ReadingFrames,
}

impl DataIoState {
    const fn is_idle(&self) -> bool {
        matches!(self, Self::Idle)
    }
}

enum OwnerState {
    Discovered,
    ControllerInit(ControllerInit),
    FirmwareLoad { machine: FirmwareMachine },
    FirmwareBoot { settle_until_ns: u64 },
    Configure { step: u8, mac: [u8; 6], vif: u8 },
    StartLink { step: u8, mac: [u8; 6], vif: u8 },
    Ready,
    Failed,
}

struct AicOwnerCore<P> {
    port: P,
    command: AicCommandEngine,
    state: OwnerState,
    config: AicDiscoveryConfig,
    firmware: FirmwarePlan,
    tx_taken: bool,
    rx_taken: bool,
    active_mac: [u8; 6],
    active_vif: u8,
    wifi_command_active: bool,
    wifi_schedule: Option<OwnerInitSchedule>,
    event_credits: VecDeque<Event>,
    data_io: DataIoState,
    card_event_pending: bool,
    tx_completions: VecDeque<u64>,
    rx_buffers: VecDeque<RuntimePacketBuffer>,
    rx_completions: VecDeque<(u64, usize)>,
    pending_rx_packets: VecDeque<Vec<u8>>,
}

impl<P: TransactionPort> AicOwnerCore<P> {
    fn new(port: P, config: AicDiscoveryConfig, firmware: FirmwarePlan) -> Self {
        Self {
            port,
            command: AicCommandEngine::new(config.chip),
            state: OwnerState::Discovered,
            config,
            firmware,
            tx_taken: false,
            rx_taken: false,
            active_mac: config.mac_address,
            active_vif: 0,
            wifi_command_active: false,
            wifi_schedule: None,
            event_credits: VecDeque::with_capacity(OWNER_EVENT_CAPACITY),
            data_io: DataIoState::Idle,
            card_event_pending: false,
            tx_completions: VecDeque::with_capacity(DATA_QUEUE_CAPACITY),
            rx_buffers: VecDeque::with_capacity(DATA_QUEUE_CAPACITY),
            rx_completions: VecDeque::with_capacity(DATA_QUEUE_CAPACITY),
            pending_rx_packets: VecDeque::with_capacity(DATA_QUEUE_CAPACITY),
        }
    }

    fn phase(&self) -> AicOwnerPhase {
        match self.state {
            OwnerState::Discovered => AicOwnerPhase::Discovered,
            OwnerState::ControllerInit(_) => AicOwnerPhase::ControllerInit,
            OwnerState::FirmwareLoad { .. } => AicOwnerPhase::FirmwareLoad,
            OwnerState::FirmwareBoot { .. } => AicOwnerPhase::FirmwareBoot,
            OwnerState::Configure { .. } => AicOwnerPhase::Configure,
            OwnerState::StartLink { .. } => AicOwnerPhase::StartLink,
            OwnerState::Ready => AicOwnerPhase::Ready,
            OwnerState::Failed => AicOwnerPhase::Failed,
        }
    }

    fn submit_tx(&mut self, buffer: DmaBuffer) -> Result<(), NetError> {
        if !matches!(self.state, OwnerState::Ready)
            || self.wifi_command_active
            || !self.data_io.is_idle()
            || self.port.is_active()
            || self.tx_completions.len() == DATA_QUEUE_CAPACITY
        {
            return Err(NetError::Retry);
        }
        // SAFETY: `ITxQueue::submit` guarantees that the buffer is valid for
        // `len` bytes for the duration of this call. OwnerCopy means the CPU
        // retains ownership; `build_tx_frame` copies every byte before the
        // runtime buffer can leave this function.
        let packet = unsafe { core::slice::from_raw_parts(buffer.virt.as_ptr(), buffer.len) };
        let frame = build_tx_frame(self.config.chip, packet, self.active_vif, 0)
            .map_err(|error| NetError::Other(Box::new(AicError::DataWire(error))))?;
        self.port
            .submit(SdioOperation::WriteBlocks {
                function: 1,
                address: self.data_write_fifo(),
                increment: false,
                bytes: frame,
            })
            .map_err(|error| NetError::Other(Box::new(AicError::Transaction(error))))?;
        self.data_io = DataIoState::Transmitting {
            bus_addr: buffer.bus_addr,
        };
        Ok(())
    }

    fn submit_rx_buffer(&mut self, buffer: DmaBuffer) -> Result<(), NetError> {
        if self.rx_buffers.len() + self.rx_completions.len() >= DATA_QUEUE_CAPACITY {
            return Err(NetError::Retry);
        }
        let buffer = RuntimePacketBuffer::from(buffer);
        if let Some(packet) = self.pending_rx_packets.pop_front() {
            self.complete_rx_packet(buffer, &packet);
        } else {
            self.rx_buffers.push_back(buffer);
        }
        Ok(())
    }

    fn reclaim_tx(&mut self) -> Option<u64> {
        self.tx_completions.pop_front()
    }

    fn reclaim_rx(&mut self) -> Option<(u64, usize)> {
        self.rx_completions.pop_front()
    }

    fn service_ready_event(&mut self, event: Event) -> Result<(), AicError> {
        if event.device_status & AIC_CARD_FUNCTION_INTERRUPT_STATUS != 0 {
            self.card_event_pending = true;
        }

        let state = core::mem::replace(&mut self.data_io, DataIoState::Idle);
        match state {
            DataIoState::Idle => {}
            state @ (DataIoState::Transmitting { .. }
            | DataIoState::ReadingCount
            | DataIoState::ReadingFrames) => match self.port.poll(0)? {
                TransactionPoll::Pending { .. } => self.data_io = state,
                TransactionPoll::Ready(completion) => match state {
                    DataIoState::Transmitting { bus_addr } => {
                        if self.tx_completions.len() == DATA_QUEUE_CAPACITY {
                            return Err(AicError::QueueUnavailable);
                        }
                        self.tx_completions.push_back(bus_addr);
                    }
                    DataIoState::ReadingCount => {
                        let blocks = self.rx_block_count(r5_data(completion.response)?);
                        if blocks != 0 {
                            self.port.submit(SdioOperation::ReadBlocks {
                                function: 1,
                                address: self.data_read_fifo(),
                                increment: false,
                                blocks: u16::from(blocks),
                            })?;
                            self.data_io = DataIoState::ReadingFrames;
                        }
                    }
                    DataIoState::ReadingFrames => {
                        for packet in decode_rx_aggregate(&completion.bytes)? {
                            self.accept_rx_packet(packet)?;
                        }
                    }
                    DataIoState::Idle => unreachable!("active data state was matched above"),
                },
            },
        }

        if self.data_io.is_idle() && self.card_event_pending && !self.port.is_active() {
            self.card_event_pending = false;
            self.port.submit(SdioOperation::ReadByte {
                function: 1,
                address: self.data_block_count(),
            })?;
            self.data_io = DataIoState::ReadingCount;
        }
        Ok(())
    }

    fn accept_rx_packet(&mut self, packet: Vec<u8>) -> Result<(), AicError> {
        if let Some(buffer) = self.rx_buffers.pop_front() {
            self.complete_rx_packet(buffer, &packet);
            return Ok(());
        }
        if self.pending_rx_packets.len() == DATA_QUEUE_CAPACITY {
            return Err(AicError::QueueUnavailable);
        }
        self.pending_rx_packets.push_back(packet);
        Ok(())
    }

    fn complete_rx_packet(&mut self, buffer: RuntimePacketBuffer, packet: &[u8]) {
        let len = packet.len().min(buffer.len);
        // SAFETY: `IRxQueue::submit` transfers a buffer that remains valid
        // until its bus address is reclaimed. OwnerCopy keeps it in CPU
        // ownership, and this owner is the sole writer before publication.
        unsafe {
            core::ptr::copy_nonoverlapping(packet.as_ptr(), buffer.virt as *mut u8, len);
        }
        self.rx_completions.push_back((buffer.bus_addr, len));
    }

    fn data_block_count(&self) -> u32 {
        if self.config.chip.is_v3() {
            0x04
        } else {
            SDIOWIFI_BLOCK_CNT_REG
        }
    }

    fn data_read_fifo(&self) -> u32 {
        if self.config.chip.is_v3() {
            0x0f
        } else {
            SDIOWIFI_RD_FIFO_ADDR
        }
    }

    fn data_write_fifo(&self) -> u32 {
        if self.config.chip.is_v3() {
            0x10
        } else {
            SDIOWIFI_WR_FIFO_ADDR
        }
    }

    fn rx_block_count(&self, status: u8) -> u8 {
        if !self.config.chip.is_v3() {
            return status & 0x7f;
        }
        let function_2_status = status | (1 << 3);
        if function_2_status > 120 {
            if function_2_status == 127 {
                1
            } else {
                status & 0x07
            }
        } else if status == 120 {
            1
        } else {
            status & 0x7f
        }
    }

    fn poll(&mut self, input: OwnerInitInput) -> Result<StateProgress, AicError> {
        let state = core::mem::replace(&mut self.state, OwnerState::Failed);
        match state {
            OwnerState::Discovered => {
                self.state = OwnerState::ControllerInit(ControllerInit::new());
                Ok(StateProgress::Pending(OwnerInitSchedule::run_again()))
            }
            OwnerState::ControllerInit(mut controller) => {
                match controller.poll(&mut self.port, input)? {
                    StateProgress::Pending(schedule) => {
                        self.state = OwnerState::ControllerInit(controller);
                        Ok(StateProgress::Pending(schedule))
                    }
                    StateProgress::Ready => {
                        self.state = OwnerState::FirmwareLoad {
                            machine: FirmwareMachine::new(self.firmware),
                        };
                        Ok(StateProgress::Pending(OwnerInitSchedule::run_again()))
                    }
                }
            }
            OwnerState::FirmwareLoad { mut machine } => {
                let mut confirmation = None;
                if !self.command.is_idle() {
                    match self.command.poll(&mut self.port, input)? {
                        CommandProgress::Pending(schedule) => {
                            self.state = OwnerState::FirmwareLoad { machine };
                            return Ok(StateProgress::Pending(schedule));
                        }
                        CommandProgress::Ready(payload) => {
                            confirmation = Some(payload);
                        }
                    }
                }
                match machine.poll(confirmation.as_deref())? {
                    FirmwarePoll::Request(request) => {
                        let schedule = self.command.start(&mut self.port, request, input.now_ns)?;
                        self.state = OwnerState::FirmwareLoad { machine };
                        Ok(StateProgress::Pending(schedule))
                    }
                    FirmwarePoll::Ready => {
                        let settle_until_ns = input.now_ns.saturating_add(FIRMWARE_SETTLE_NS);
                        self.state = OwnerState::FirmwareBoot { settle_until_ns };
                        Ok(StateProgress::Pending(OwnerInitSchedule::wait_until(
                            settle_until_ns,
                        )))
                    }
                }
            }
            OwnerState::FirmwareBoot { settle_until_ns } => {
                if input.now_ns < settle_until_ns {
                    self.state = OwnerState::FirmwareBoot { settle_until_ns };
                    return Ok(StateProgress::Pending(OwnerInitSchedule::wait_until(
                        settle_until_ns,
                    )));
                }
                self.state = OwnerState::Configure {
                    step: 0,
                    mac: self.config.mac_address,
                    vif: 0,
                };
                Ok(StateProgress::Pending(OwnerInitSchedule::run_again()))
            }
            OwnerState::Configure {
                mut step,
                mut mac,
                mut vif,
            } => {
                if !self.command.is_idle() {
                    match self.command.poll(&mut self.port, input)? {
                        CommandProgress::Pending(schedule) => {
                            self.state = OwnerState::Configure { step, mac, vif };
                            return Ok(StateProgress::Pending(schedule));
                        }
                        CommandProgress::Ready(payload) => {
                            if step == 2 {
                                if payload.len() < 6 {
                                    return Err(AicError::InvalidConfirmation("get MAC"));
                                }
                                mac.copy_from_slice(&payload[..6]);
                            } else if step == 6 {
                                if payload.len() < 2 {
                                    return Err(AicError::InvalidConfirmation("add AP interface"));
                                }
                                if payload[0] != 0 {
                                    return Err(AicError::FirmwareRejected {
                                        operation: "add AP interface",
                                        status: payload[0],
                                    });
                                }
                                vif = payload[1];
                            }
                            step += 1;
                        }
                    }
                }
                if step == 9 {
                    self.config.mac_address = mac;
                    self.state = OwnerState::StartLink { step: 0, mac, vif };
                    return Ok(StateProgress::Pending(OwnerInitSchedule::run_again()));
                }
                let request = configure_request(step, self.config.chip, mac);
                let schedule = self.command.start(&mut self.port, request, input.now_ns)?;
                self.state = OwnerState::Configure { step, mac, vif };
                Ok(StateProgress::Pending(schedule))
            }
            OwnerState::StartLink { mut step, mac, vif } => {
                let policy = self.config.soft_ap.ok_or(AicError::MissingStartupPolicy)?;
                if !self.command.is_idle() {
                    match self.command.poll(&mut self.port, input)? {
                        CommandProgress::Pending(schedule) => {
                            self.state = OwnerState::StartLink { step, mac, vif };
                            return Ok(StateProgress::Pending(schedule));
                        }
                        CommandProgress::Ready(payload) => {
                            if step == 1 {
                                let status = payload
                                    .first()
                                    .copied()
                                    .ok_or(AicError::InvalidConfirmation("AP start"))?;
                                if status != 0 {
                                    return Err(AicError::FirmwareRejected {
                                        operation: "AP start",
                                        status,
                                    });
                                }
                            }
                            step += 1;
                        }
                    }
                }
                if step == 2 {
                    self.active_mac = mac;
                    self.active_vif = vif;
                    self.state = OwnerState::Ready;
                    return Ok(StateProgress::Ready);
                }
                let request = if step == 0 {
                    beacon_request(policy, mac, vif)
                } else {
                    start_ap_request(policy, mac, vif)
                };
                let schedule = self.command.start(&mut self.port, request, input.now_ns)?;
                self.state = OwnerState::StartLink { step, mac, vif };
                Ok(StateProgress::Pending(schedule))
            }
            OwnerState::Ready => {
                self.state = OwnerState::Ready;
                Ok(StateProgress::Ready)
            }
            OwnerState::Failed => Err(AicError::InvalidConfirmation("failed owner state")),
        }
    }
}

fn configure_request(step: u8, _chip: ChipVariant, mac: [u8; 6]) -> LmacRequest {
    match step {
        0 => stack_start_request(),
        1 => rf_calibration_request(),
        2 => get_mac_request(),
        3 => reset_request(),
        4 => me_config_request(),
        5 => channel_config_request(),
        6 => add_ap_interface_request(mac),
        7 => start_mac_request(),
        8 => filter_request(),
        _ => unreachable!("configuration step is bounded"),
    }
}

fn map_owner_error(error: AicError) -> OwnerInitPoll {
    OwnerInitPoll::Failed(NetError::Other(Box::new(error)))
}

/// Discovered AIC8800 controller. All methods other than IRQ capture execute
/// on its final CPU-pinned maintenance owner.
pub struct AicWifiNetDev<H>
where
    H: SdioHost2Timed + 'static,
{
    core: AicOwnerCore<SdioTransactionEngine<H>>,
    irq_endpoint: Option<AicIrqEndpoint<H::IrqEndpoint>>,
    irq_control: H::IrqControl,
}

impl<H> AicWifiNetDev<H>
where
    H: SdioHost2Timed + 'static,
{
    /// Splits the host IRQ source without accessing device registers.
    pub fn discover(
        mut host: H,
        dma: DeviceDma,
        config: AicDiscoveryConfig,
    ) -> Result<Self, AicError> {
        let firmware = firmware::plan(config.chip).ok_or(AicError::UnsupportedChip)?;
        let source = host.take_irq_source().ok_or(AicError::MissingIrqSource)?;
        let (endpoint, control) = source.into_parts();
        Ok(Self {
            core: AicOwnerCore::new(SdioTransactionEngine::new(host, dma), config, firmware),
            irq_endpoint: Some(AicIrqEndpoint { inner: endpoint }),
            irq_control: control,
        })
    }

    pub fn owner_phase(&self) -> AicOwnerPhase {
        self.core.phase()
    }
}

impl<H> rdif_eth::DriverGeneric for AicWifiNetDev<H>
where
    H: SdioHost2Timed + Send + 'static,
    H::BusRequest: Send + 'static,
    H::TransactionRequest<'static>: Send + 'static,
{
    fn name(&self) -> &str {
        DEVICE_NAME
    }
}

impl<H> NetDeviceOwner for AicWifiNetDev<H>
where
    H: SdioHost2Timed + Send + 'static,
    H::BusRequest: Send + 'static,
    H::TransactionRequest<'static>: Send + 'static,
{
    fn poll_owner_init(&mut self, input: OwnerInitInput) -> OwnerInitPoll {
        match self.core.poll(input) {
            Ok(StateProgress::Pending(schedule)) => OwnerInitPoll::Pending(schedule),
            Ok(StateProgress::Ready) => OwnerInitPoll::Ready,
            Err(error) => {
                let _ = self.core.port.abort_active();
                self.core.state = OwnerState::Failed;
                map_owner_error(error)
            }
        }
    }

    fn mac_address(&self) -> [u8; 6] {
        self.core.config.mac_address
    }

    fn activate_queue_set(&mut self) -> Result<ActiveQueueSet, NetError> {
        if !matches!(self.core.state, OwnerState::Ready) || self.core.tx_taken || self.core.rx_taken
        {
            return Err(NetError::Retry);
        }
        let queues = ActiveQueueSet::single(packet_queue_config(), packet_queue_config())
            .map_err(|error| NetError::Other(Box::new(error)))?;
        self.core.tx_taken = true;
        self.core.rx_taken = true;
        Ok(queues)
    }

    fn submit_tx(&mut self, queue: &TxQueueOwner, buffer: DmaBuffer) -> Result<(), NetError> {
        self.validate_tx_owner(queue)?;
        self.core.submit_tx(buffer)
    }

    fn reclaim_tx(&mut self, queue: &TxQueueOwner) -> Result<Option<u64>, NetError> {
        self.validate_tx_owner(queue)?;
        Ok(self.core.reclaim_tx())
    }

    fn submit_rx(&mut self, queue: &RxQueueOwner, buffer: DmaBuffer) -> Result<(), NetError> {
        self.validate_rx_owner(queue)?;
        self.core.submit_rx_buffer(buffer)
    }

    fn reclaim_rx(&mut self, queue: &RxQueueOwner) -> Result<Option<(u64, usize)>, NetError> {
        self.validate_rx_owner(queue)?;
        Ok(self.core.reclaim_rx())
    }

    fn enable_irq(&mut self) -> Result<(), NetError> {
        self.core
            .port
            .host_mut()
            .enable_completion_irq()
            .map_err(|error| NetError::Other(Box::new(AicError::IrqHost(error))))
    }

    fn disable_irq(&mut self) -> Result<(), NetError> {
        self.core
            .port
            .host_mut()
            .disable_completion_irq()
            .map_err(|error| NetError::Other(Box::new(AicError::IrqHost(error))))
    }

    fn is_irq_enabled(&self) -> bool {
        self.core.port.host().completion_irq_enabled()
    }

    fn take_irq_endpoint(&mut self) -> Option<BIrqEndpoint> {
        self.irq_endpoint
            .take()
            .map(|endpoint| Box::new(endpoint) as BIrqEndpoint)
    }

    fn rearm_irq_source(&mut self, source: MaskedSource) -> Result<(), NetError> {
        self.irq_control
            .rearm(source)
            .map_err(|error| NetError::Other(Box::new(error)))
    }

    fn owner_link_policy(&self) -> Option<WifiLinkPolicy> {
        self.core.config.link_policy
    }

    fn service_irq_event(&mut self, event: Event) -> Result<(), NetError> {
        if self.core.wifi_command_active {
            if event.device_status & AIC_CARD_FUNCTION_INTERRUPT_STATUS != 0 {
                self.core.card_event_pending = true;
            }
            if self.core.event_credits.len() == OWNER_EVENT_CAPACITY {
                return Err(NetError::Retry);
            }
            self.core.event_credits.push_back(event);
            return Ok(());
        }
        match self.core.service_ready_event(event) {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = self.core.port.abort_active();
                self.core.state = OwnerState::Failed;
                Err(NetError::Other(Box::new(error)))
            }
        }
    }

    fn supports_wifi_control(&self) -> bool {
        true
    }

    fn start_wifi_command(
        &mut self,
        command: WifiCommand,
        now_ns: u64,
    ) -> Result<WifiCommandProgress, WifiCommandStartError> {
        if self.core.wifi_command_active
            || !matches!(self.core.state, OwnerState::Ready)
            || !self.core.data_io.is_idle()
            || self.core.port.is_active()
        {
            return Err(WifiCommandStartError::Busy(command));
        }
        let (ssid, channel) = match command {
            unsupported @ WifiCommand::JoinStation { .. } => {
                return Err(WifiCommandStartError::Unsupported(unsupported));
            }
            WifiCommand::StartAccessPoint { ssid, channel } => (ssid, channel),
        };
        let policy = match SoftApPolicy::try_new(&ssid, channel) {
            Ok(policy) => policy,
            Err(_) => {
                return Err(WifiCommandStartError::Unsupported(
                    WifiCommand::StartAccessPoint { ssid, channel },
                ));
            }
        };
        self.core.config.soft_ap = Some(policy);
        self.core.wifi_command_active = true;
        self.core.state = OwnerState::StartLink {
            step: 0,
            mac: self.core.active_mac,
            vif: self.core.active_vif,
        };
        match self.core.poll(OwnerInitInput::at(now_ns)) {
            Ok(StateProgress::Pending(schedule)) => {
                self.core.wifi_schedule = Some(schedule);
                Ok(WifiCommandProgress::Pending(wifi_schedule(schedule)))
            }
            Ok(StateProgress::Ready) => {
                self.core.wifi_command_active = false;
                self.core.wifi_schedule = None;
                Ok(WifiCommandProgress::Complete(
                    WifiCommandResult::AccessPointStarted,
                ))
            }
            Err(error) => {
                self.core.wifi_command_active = false;
                self.core.wifi_schedule = None;
                let _ = self.core.port.abort_active();
                self.core.state = OwnerState::Failed;
                Ok(WifiCommandProgress::Failed(NetError::Other(Box::new(
                    error,
                ))))
            }
        }
    }

    fn poll_wifi_command(&mut self, now_ns: u64) -> WifiCommandProgress {
        if !self.core.wifi_command_active {
            return WifiCommandProgress::Failed(NetError::NotSupported);
        }
        let schedule = self
            .core
            .wifi_schedule
            .take()
            .unwrap_or_else(OwnerInitSchedule::run_again);
        let input = if schedule.run_again {
            OwnerInitInput::at(now_ns)
        } else {
            match self.core.event_credits.pop_front() {
                Some(event) => OwnerInitInput::with_event(now_ns, event),
                None => OwnerInitInput::at(now_ns),
            }
        };
        match self.core.poll(input) {
            Ok(StateProgress::Pending(schedule)) => {
                self.core.wifi_schedule = Some(schedule);
                WifiCommandProgress::Pending(wifi_schedule(schedule))
            }
            Ok(StateProgress::Ready) => {
                self.core.wifi_command_active = false;
                self.core.wifi_schedule = None;
                while let Some(event) = self.core.event_credits.pop_front() {
                    if let Err(error) = self.core.service_ready_event(event) {
                        let _ = self.core.port.abort_active();
                        self.core.state = OwnerState::Failed;
                        return WifiCommandProgress::Failed(NetError::Other(Box::new(error)));
                    }
                }
                WifiCommandProgress::Complete(WifiCommandResult::AccessPointStarted)
            }
            Err(error) => {
                self.core.wifi_command_active = false;
                self.core.wifi_schedule = None;
                let _ = self.core.port.abort_active();
                self.core.state = OwnerState::Failed;
                WifiCommandProgress::Failed(NetError::Other(Box::new(error)))
            }
        }
    }
}

impl<H> AicWifiNetDev<H>
where
    H: SdioHost2Timed + Send + 'static,
    H::BusRequest: Send + 'static,
    H::TransactionRequest<'static>: Send + 'static,
{
    fn validate_tx_owner(&self, queue: &TxQueueOwner) -> Result<(), NetError> {
        if self.core.tx_taken
            && queue.id() == TX_QUEUE_ID
            && queue.config() == packet_queue_config()
        {
            Ok(())
        } else {
            Err(NetError::NotSupported)
        }
    }

    fn validate_rx_owner(&self, queue: &RxQueueOwner) -> Result<(), NetError> {
        if self.core.rx_taken
            && queue.id() == RX_QUEUE_ID
            && queue.config() == packet_queue_config()
        {
            Ok(())
        } else {
            Err(NetError::NotSupported)
        }
    }
}

const fn wifi_schedule(schedule: OwnerInitSchedule) -> WifiCommandSchedule {
    WifiCommandSchedule {
        run_again: schedule.run_again,
        irq_sources: schedule.irq_sources,
        wake_at_ns: schedule.wake_at_ns,
    }
}

const fn packet_queue_config() -> QueueConfig {
    QueueConfig {
        dma_mask: u32::MAX as u64,
        align: 4,
        buf_size: 2048,
        ring_size: 16,
        memory_mode: QueueMemoryMode::OwnerCopy,
    }
}

struct AicIrqEndpoint<E>
where
    E: IrqEndpoint,
{
    inner: E,
}

impl<E> IrqEndpoint for AicIrqEndpoint<E>
where
    E: IrqEndpoint,
    E::Event: HostEvent,
{
    type Event = Event;
    type Fault = EthernetIrqFault;

    fn capture(&mut self) -> IrqCapture<Self::Event, Self::Fault> {
        match self.inner.capture() {
            IrqCapture::Unhandled => IrqCapture::Unhandled,
            IrqCapture::Captured { event, masked } => IrqCapture::Captured {
                event: host_event_to_net_event(&event),
                masked,
            },
            IrqCapture::Fault { containment, .. } => IrqCapture::Fault {
                reason: EthernetIrqFault::Capture,
                containment,
            },
        }
    }

    fn contain(&mut self, cause: rdif_eth::ContainmentCause) -> Result<MaskedSource, Self::Fault> {
        self.inner
            .contain(cause)
            .map_err(|_| EthernetIrqFault::Containment)
    }
}

fn host_event_to_net_event(host_event: &impl HostEvent) -> Event {
    let summary = host_event.stable_summary();
    let mut tx_queue = rdif_eth::IdList::none();
    let mut rx_queue = rdif_eth::IdList::none();
    if summary.queue_service {
        tx_queue.insert(TX_QUEUE_ID);
        // AIC serializes both directions through one SDIO transaction slot, so
        // both runtime directions must observe its terminal host event.
        rx_queue.insert(RX_QUEUE_ID);
    }
    if summary.card_function_interrupt {
        rx_queue.insert(RX_QUEUE_ID);
    }
    Event {
        tx_queue,
        rx_queue,
        device_status: u64::from(summary.stable_status)
            | if summary.card_function_interrupt {
                AIC_CARD_FUNCTION_INTERRUPT_STATUS
            } else {
                0
            },
    }
}

#[cfg(test)]
mod tests {
    use alloc::{collections::VecDeque, vec};

    use super::*;
    use crate::transport::SdioCompletion;

    struct FakePort {
        active: bool,
        submitted: Vec<SdioOperation>,
        completions: VecDeque<SdioCompletion>,
    }

    impl FakePort {
        fn new(completions: Vec<SdioCompletion>) -> Self {
            Self {
                active: false,
                submitted: Vec::new(),
                completions: completions.into(),
            }
        }
    }

    impl TransactionPort for FakePort {
        fn submit(&mut self, operation: SdioOperation) -> Result<(), TransactionError> {
            if self.active {
                return Err(TransactionError::Busy);
            }
            self.active = true;
            self.submitted.push(operation);
            Ok(())
        }

        fn poll(&mut self, _now_ns: u64) -> Result<TransactionPoll, TransactionError> {
            if !self.active {
                return Err(TransactionError::InvalidOperation);
            }
            self.active = false;
            Ok(TransactionPoll::Ready(
                self.completions
                    .pop_front()
                    .unwrap_or_else(command_completion),
            ))
        }

        fn is_active(&self) -> bool {
            self.active
        }
    }

    fn command_completion() -> SdioCompletion {
        SdioCompletion {
            response: sdio_host2::RawResponse::new(sdio_host2::ResponseType::R5, [0, 0, 0, 0]),
            bytes: Vec::new(),
        }
    }

    fn cfm_completion(message_id: u16, payload: &[u8]) -> SdioCompletion {
        let mut bytes = vec![0; 512];
        bytes[4..6].copy_from_slice(&message_id.to_le_bytes());
        bytes[10..12].copy_from_slice(&(payload.len() as u16).to_le_bytes());
        bytes[16..16 + payload.len()].copy_from_slice(payload);
        SdioCompletion {
            response: sdio_host2::RawResponse::new(sdio_host2::ResponseType::R5, [0, 0, 0, 0]),
            bytes,
        }
    }

    fn drive_command(
        engine: &mut AicCommandEngine,
        port: &mut FakePort,
        now: &mut u64,
    ) -> Result<Vec<u8>, AicError> {
        loop {
            *now += 1;
            match engine.poll(port, OwnerInitInput::with_event(*now, Event::none()))? {
                CommandProgress::Pending(_) => {}
                CommandProgress::Ready(payload) => return Ok(payload),
            }
        }
    }

    struct ProtocolFakePort {
        active: Option<SdioOperation>,
        pending_cfm: Option<u16>,
        pending_debug_address: Option<u32>,
        debug_requests: Vec<(u16, u32)>,
        submitted: usize,
        data_writes: Vec<Vec<u8>>,
        rx_aggregates: VecDeque<Vec<u8>>,
    }

    impl ProtocolFakePort {
        const fn new() -> Self {
            Self {
                active: None,
                pending_cfm: None,
                pending_debug_address: None,
                debug_requests: Vec::new(),
                submitted: 0,
                data_writes: Vec::new(),
                rx_aggregates: VecDeque::new(),
            }
        }
    }

    impl TransactionPort for ProtocolFakePort {
        fn submit(&mut self, operation: SdioOperation) -> Result<(), TransactionError> {
            if self.active.is_some() {
                return Err(TransactionError::Busy);
            }
            if let SdioOperation::WriteBlocks { bytes, .. } = &operation {
                if bytes.get(2) == Some(&crate::common::SDIO_TYPE_CFG_CMD_RSP) && bytes.len() >= 10
                {
                    let request = u16::from_le_bytes([bytes[8], bytes[9]]);
                    self.pending_cfm = Some(request.wrapping_add(1));
                    self.pending_debug_address =
                        matches!(request, 0x0400 | 0x0402 | 0x040b | 0x040d | 0x0411)
                            .then(|| u32::from_le_bytes(bytes[16..20].try_into().unwrap()));
                    if let Some(address) = self.pending_debug_address {
                        self.debug_requests.push((request, address));
                    }
                } else if bytes.get(2) == Some(&crate::common::SDIO_TYPE_DATA) {
                    self.data_writes.push(bytes.clone());
                }
            }
            self.submitted += 1;
            self.active = Some(operation);
            Ok(())
        }

        fn poll(&mut self, _now_ns: u64) -> Result<TransactionPoll, TransactionError> {
            let operation = self
                .active
                .take()
                .ok_or(TransactionError::InvalidOperation)?;
            let completion = match operation {
                SdioOperation::Command(command) if command.index == 5 => SdioCompletion {
                    response: sdio_host2::RawResponse::new(
                        sdio_host2::ResponseType::R4,
                        [1 << 31, 0, 0, 0],
                    ),
                    bytes: Vec::new(),
                },
                SdioOperation::Command(command) if command.index == 3 => SdioCompletion {
                    response: sdio_host2::RawResponse::new(
                        sdio_host2::ResponseType::R6,
                        [1 << 16, 0, 0, 0],
                    ),
                    bytes: Vec::new(),
                },
                SdioOperation::ReadByte { .. } if self.pending_cfm.is_some() => SdioCompletion {
                    response: sdio_host2::RawResponse::new(
                        sdio_host2::ResponseType::R5,
                        [1, 0, 0, 0],
                    ),
                    bytes: Vec::new(),
                },
                SdioOperation::ReadByte { .. } => {
                    let blocks = self.rx_aggregates.front().map_or(0, |frame| {
                        frame.len().div_ceil(crate::wire::SDIO_BLOCK_SIZE)
                    });
                    SdioCompletion {
                        response: sdio_host2::RawResponse::new(
                            sdio_host2::ResponseType::R5,
                            [blocks as u32, 0, 0, 0],
                        ),
                        bytes: Vec::new(),
                    }
                }
                SdioOperation::ReadBlocks { .. } => {
                    if let Some(confirmation) = self.pending_cfm.take() {
                        let payload = match confirmation {
                            0x0401 => {
                                let address = self.pending_debug_address.take().unwrap();
                                let mut payload = address.to_le_bytes().to_vec();
                                payload
                                    .extend_from_slice(&fake_memory_value(address).to_le_bytes());
                                payload
                            }
                            0x0403 | 0x040c | 0x0412 => self
                                .pending_debug_address
                                .take()
                                .unwrap()
                                .to_le_bytes()
                                .to_vec(),
                            0x040e => vec![0, 0, 0, 0],
                            0x0074 => vec![2, 0, 0, 0, 0, 1],
                            0x0007 => vec![0, 1],
                            0x1c01 => vec![0],
                            _ => Vec::new(),
                        };
                        cfm_completion(confirmation, &payload)
                    } else {
                        SdioCompletion {
                            response: sdio_host2::RawResponse::new(
                                sdio_host2::ResponseType::R5,
                                [0, 0, 0, 0],
                            ),
                            bytes: self
                                .rx_aggregates
                                .pop_front()
                                .ok_or(TransactionError::InvalidOperation)?,
                        }
                    }
                }
                _ => command_completion(),
            };
            Ok(TransactionPoll::Ready(completion))
        }

        fn is_active(&self) -> bool {
            self.active.is_some()
        }
    }

    const fn fake_memory_value(address: u32) -> u32 {
        match address {
            0x4050_0000 => 0x0001_0000,
            0x0000_0020 => 1,
            0x4050_0148 => 0,
            0x0001_0164 => 0x0020_0000,
            0x0001_016c => 0x0021_0000,
            0x0001_0170 => 0x0022_0000,
            0x0001_0174 => 0x0023_0000,
            0x0001_0178 => 0x0024_0000,
            0x0020_0124 => 0x0300_0000,
            _ => 0,
        }
    }

    fn drive_owner_to_ready(core: &mut AicOwnerCore<ProtocolFakePort>) {
        let mut input = OwnerInitInput::at(0);
        for _ in 0..20_000 {
            match core.poll(input).unwrap() {
                StateProgress::Ready => return,
                StateProgress::Pending(schedule) => {
                    let now_ns = if schedule.run_again {
                        input.now_ns + 1
                    } else if !schedule.irq_sources.is_empty() {
                        input.now_ns + 1
                    } else {
                        schedule
                            .wake_at_ns
                            .expect("deadline-only state must name its deadline")
                    };
                    input = if schedule.run_again || schedule.irq_sources.is_empty() {
                        OwnerInitInput::at(now_ns)
                    } else {
                        OwnerInitInput::with_event(now_ns, Event::none())
                    };
                }
            }
        }
        panic!("AIC owner did not reach Ready within the deterministic transition budget");
    }

    fn ethernet_packet() -> Vec<u8> {
        vec![
            0x02, 1, 2, 3, 4, 5, 0x02, 6, 7, 8, 9, 10, 0x08, 0x00, 0xde, 0xad, 0xbe, 0xef,
        ]
    }

    fn rx_aggregate() -> Vec<u8> {
        let packet = ethernet_packet();
        let mut mpdu = vec![0; 24];
        mpdu[0] = 0x08;
        mpdu[1] = 0x02;
        mpdu[4..10].copy_from_slice(&packet[..6]);
        mpdu[16..22].copy_from_slice(&packet[6..12]);
        mpdu.extend_from_slice(&[0xaa, 0xaa, 0x03, 0, 0, 0]);
        mpdu.extend_from_slice(&packet[12..]);
        let mut aggregate = vec![0; 60 + mpdu.len()];
        aggregate[..2].copy_from_slice(&(mpdu.len() as u16).to_le_bytes());
        aggregate[2] = crate::common::SDIO_TYPE_DATA;
        aggregate[60..].copy_from_slice(&mpdu);
        aggregate.resize(
            aggregate
                .len()
                .next_multiple_of(crate::wire::SDIO_BLOCK_SIZE),
            0,
        );
        aggregate
    }

    const fn host_completion_event() -> Event {
        Event {
            tx_queue: rdif_eth::IdList::none(),
            rx_queue: rdif_eth::IdList::none(),
            device_status: 1,
        }
    }

    const fn card_interrupt_event() -> Event {
        Event {
            tx_queue: rdif_eth::IdList::none(),
            rx_queue: rdif_eth::IdList::none(),
            device_status: AIC_CARD_FUNCTION_INTERRUPT_STATUS,
        }
    }

    struct StableHostEvent(sdmmc_protocol::sdio::HostEventSummary);

    impl HostEvent for StableHostEvent {
        fn kind(&self) -> sdmmc_protocol::sdio::HostEventKind {
            sdmmc_protocol::sdio::HostEventKind::Other
        }

        fn stable_summary(&self) -> sdmmc_protocol::sdio::HostEventSummary {
            self.0
        }
    }

    #[test]
    fn stable_host_facts_map_to_owner_event_without_os_callback() {
        let event =
            host_event_to_net_event(&StableHostEvent(sdmmc_protocol::sdio::HostEventSummary {
                stable_status: 0x55aa_00ff,
                queue_service: true,
                card_function_interrupt: true,
            }));

        assert!(event.tx_queue.contains(TX_QUEUE_ID));
        assert!(event.rx_queue.contains(RX_QUEUE_ID));
        assert_eq!(
            event.device_status & !AIC_CARD_FUNCTION_INTERRUPT_STATUS,
            0x55aa_00ff
        );
        assert_ne!(event.device_status & AIC_CARD_FUNCTION_INTERRUPT_STATUS, 0);
    }

    #[test]
    fn command_engine_rejects_wrong_confirmation_id() {
        let mut port = FakePort::new(vec![
            command_completion(),
            SdioCompletion {
                response: sdio_host2::RawResponse::new(sdio_host2::ResponseType::R5, [1, 0, 0, 0]),
                bytes: Vec::new(),
            },
            cfm_completion(0x007d, &[]),
        ]);
        let mut engine = AicCommandEngine::new(ChipVariant::Aic8800DC);
        engine.start(&mut port, stack_start_request(), 0).unwrap();
        let error = drive_command(&mut engine, &mut port, &mut 0).unwrap_err();
        assert!(matches!(
            error,
            AicError::ConfirmationMismatch {
                expected: 0x007c,
                actual: 0x007d
            }
        ));
    }

    #[test]
    fn command_engine_times_out_without_an_irq_activation() {
        let mut port = FakePort::new(Vec::new());
        let mut engine = AicCommandEngine::new(ChipVariant::Aic8800DC);
        engine.start(&mut port, stack_start_request(), 10).unwrap();
        let error = engine
            .poll(&mut port, OwnerInitInput::at(10 + COMMAND_TIMEOUT_NS))
            .unwrap_err();
        assert!(matches!(error, AicError::Timeout { .. }));
    }

    #[test]
    fn fake_host_reaches_ready_only_after_firmware_and_softap_confirmations() {
        let config = AicDiscoveryConfig::new([0; 6], None)
            .with_soft_ap(SoftApPolicy::try_new(b"PicoClaw-Car", 6).unwrap());
        let mut core = AicOwnerCore::new(
            ProtocolFakePort::new(),
            config,
            firmware::plan(config.chip).unwrap(),
        );

        drive_owner_to_ready(&mut core);

        assert_eq!(core.phase(), AicOwnerPhase::Ready);
        assert_eq!(core.config.mac_address, [2, 0, 0, 0, 0, 1]);
        assert!(core.port.submitted > 200);
        assert_eq!(
            &core.port.debug_requests[..2],
            &[(0x0400, 0x4050_0000), (0x0400, 0x0000_0020)]
        );
        let patch_upload = core
            .port
            .debug_requests
            .iter()
            .position(|request| *request == (0x040b, firmware::ROM_FMAC_PATCH_ADDR))
            .expect("DC patch must be uploaded");
        let rf_upload = core
            .port
            .debug_requests
            .iter()
            .position(|request| *request == (0x040b, 0x0021_0000))
            .expect("LDPC configuration must be uploaded");
        let final_start = core
            .port
            .debug_requests
            .iter()
            .position(|request| *request == (0x040d, firmware::RAM_FMAC_FW_ADDR))
            .expect("DC firmware must be started from its ROM entry");
        assert!(patch_upload < rf_upload && rf_upload < final_start);
        let queue = packet_queue_config();
        assert_eq!(queue.buf_size, 2048);
        assert_eq!(queue.ring_size, 16);
        assert_eq!(queue.memory_mode, QueueMemoryMode::OwnerCopy);

        let mut outgoing = ethernet_packet();
        core.submit_tx(DmaBuffer {
            virt: core::ptr::NonNull::new(outgoing.as_mut_ptr()).unwrap(),
            bus_addr: 0x1000,
            len: outgoing.len(),
        })
        .unwrap();
        core.service_ready_event(host_completion_event()).unwrap();
        assert_eq!(core.reclaim_tx(), Some(0x1000));
        assert_eq!(core.port.data_writes.len(), 1);

        let mut incoming = vec![0; 2048];
        core.submit_rx_buffer(DmaBuffer {
            virt: core::ptr::NonNull::new(incoming.as_mut_ptr()).unwrap(),
            bus_addr: 0x2000,
            len: incoming.len(),
        })
        .unwrap();
        core.port.rx_aggregates.push_back(rx_aggregate());
        core.service_ready_event(card_interrupt_event()).unwrap();
        core.service_ready_event(host_completion_event()).unwrap();
        core.service_ready_event(host_completion_event()).unwrap();
        assert_eq!(core.reclaim_rx(), Some((0x2000, ethernet_packet().len())));
        assert_eq!(&incoming[..ethernet_packet().len()], &ethernet_packet());
    }
}
