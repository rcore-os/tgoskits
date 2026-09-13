use core::{num::NonZeroU16, time::Duration};

use sdmmc_host::ProgressCause;

use super::{
    CisInfo, FunctionNumber, IoAddress, SdioCard, SdioCardInfo, SdioFunctionInfo,
    function::{fbr_base, read_common},
    response::{bad_response, check_r5, expect_r1},
};
use crate::{
    block::{CommandResponseProgress, OperationProgress},
    cmd::{self, Command},
    error::{Error, ErrorContext, Phase},
    response::{Response, SdioOcrResponse},
    sdio::{
        host::{BusWidth, ClockSpeed, SdMmcBusOp, SdMmcIrqHost},
        transport::ProtocolBusRequest,
    },
};

const OCR_3V2_3V4: u32 = 0x0030_0000;
const MAX_CMD5_POLLS: u16 = 100;
const MAX_CIS_TUPLES: u16 = 256;
const POWER_STABILIZATION_DELAY: Duration = Duration::from_millis(10);

const CCCR_SDIO_REVISION: u32 = 0x00;
const CCCR_SD_REVISION: u32 = 0x01;
const CCCR_BUS_INTERFACE: u32 = 0x07;
const CCCR_CARD_CAPABILITY: u32 = 0x08;
const CCCR_CIS_POINTER: u32 = 0x09;
const CCCR_BUS_SPEED_SELECT: u32 = 0x13;
const FBR_CIS_POINTER: u32 = 0x09;
const FBR_BLOCK_SIZE: u32 = 0x10;
const CISTPL_NULL: u8 = 0x00;
const CISTPL_MANFID: u8 = 0x20;
const CISTPL_END: u8 = 0xff;
const BUS_WIDTH_4BIT: u8 = 0x02;
const BUS_WIDTH_MASK: u8 = 0x03;
const CARD_CAP_LOW_SPEED: u8 = 1 << 6;
const CARD_CAP_LOW_SPEED_4BIT: u8 = 1 << 7;
const BUS_SPEED_SUPPORT_HIGH_SPEED: u8 = 1;
const BUS_SPEED_ENABLE_HIGH_SPEED: u8 = 1 << 1;

/// Non-blocking IO-card initialization request.
pub struct SdioInitRequest<H: SdMmcIrqHost + 'static> {
    state: InitState,
    active: InitActive<H>,
    ocr: Option<SdioOcrResponse>,
    rca: u16,
    io_functions: u8,
    cmd5_polls: u16,
    cccr_revision: u8,
    sd_revision: u8,
    card_capability: u8,
    bus_interface: u8,
    bus_speed: u8,
    common_cis: CisInfo,
    functions: [Option<SdioFunctionInfo>; 7],
    pointer_bytes: [u8; 3],
    block_size_bytes: [u8; 2],
    cis_target: u8,
    cis_cursor: u32,
    cis_tuple_count: u16,
    cis_tuple_code: u8,
    cis_tuple_link: u8,
    cis_tuple_data: [u8; 4],
    cis_tuple_data_index: u8,
}

enum InitActive<H: SdMmcIrqHost + 'static> {
    None,
    Command,
    Bus(ProtocolBusRequest<H>),
    Delay(Duration),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InitState {
    ResetHost,
    PowerOn,
    PostPowerOnDelay,
    SetOneBitBus,
    SetIdentificationClock,
    PostIdentificationClockDelay,
    QueryOcr,
    WaitForOcr,
    AssignRca,
    SelectCard,
    ReadCccrRevision,
    ReadSdRevision,
    ReadCardCapability,
    ReadBusInterface,
    ReadBusSpeed,
    ReadCommonCisPointer(u8),
    ReadFunctionInterface(u8),
    ReadFunctionCisPointer { function: u8, byte: u8 },
    ReadFunctionBlockSize { function: u8, byte: u8 },
    ReadCisTupleCode,
    ReadCisTupleLink,
    ReadCisManufacturerByte,
    WriteFourBitBus,
    SetFourBitHost,
    WriteHighSpeed,
    SetHighSpeedHost,
    Complete,
}

impl<H: SdMmcIrqHost + 'static> SdioCard<H> {
    /// Begin IO-only card enumeration.
    pub fn submit_init(&mut self) -> Result<SdioInitRequest<H>, Error> {
        let mut request = SdioInitRequest::new();
        self.submit_init_state(&mut request)?;
        Ok(request)
    }

    /// Advance one completed controller or register step of initialization.
    pub fn advance_init_request(
        &mut self,
        request: &mut SdioInitRequest<H>,
        cause: ProgressCause,
    ) -> Result<OperationProgress<SdioCardInfo>, Error> {
        match &mut request.active {
            InitActive::None => return Err(Error::InvalidArgument),
            InitActive::Command => match self.host.advance_command_response(cause)? {
                CommandResponseProgress::Pending => return Ok(OperationProgress::Pending),
                CommandResponseProgress::Complete(response) => {
                    request.active = InitActive::None;
                    self.consume_init_response(request, response)?;
                }
            },
            InitActive::Bus(bus_request) => match self.host.advance_bus_op(bus_request, cause)? {
                OperationProgress::Pending => return Ok(OperationProgress::Pending),
                OperationProgress::Complete(()) => {
                    request.active = InitActive::None;
                    request.advance_after_bus()?;
                }
            },
            InitActive::Delay(_) => {
                if cause != ProgressCause::RegisterRetry {
                    return Ok(OperationProgress::Pending);
                }
                request.active = InitActive::None;
                request.advance_after_delay()?;
            }
        }

        if request.state == InitState::Complete {
            let info = request.card_info()?;
            self.info = Some(info);
            self.functions = request.functions;
            return Ok(OperationProgress::Complete(info));
        }
        self.submit_init_state(request)?;
        Ok(OperationProgress::Pending)
    }

    /// Abort an in-flight initialization request and return host ownership to
    /// the idle protocol state.
    pub fn abort_init_request(&mut self, request: &mut SdioInitRequest<H>) -> Result<(), Error> {
        match core::mem::replace(&mut request.active, InitActive::None) {
            InitActive::None => Ok(()),
            InitActive::Command => self.host.abort_command_request(),
            InitActive::Bus(mut bus_request) => self.host.abort_bus_request(&mut bus_request),
            InitActive::Delay(_) => Ok(()),
        }
    }

    fn submit_init_state(&mut self, request: &mut SdioInitRequest<H>) -> Result<(), Error> {
        if !matches!(request.active, InitActive::None) {
            return Err(Error::Busy);
        }
        let active = match request.state {
            InitState::ResetHost => self.submit_init_bus(SdMmcBusOp::ResetAll)?,
            InitState::PowerOn => self.submit_init_bus(SdMmcBusOp::PowerOn)?,
            InitState::SetOneBitBus => {
                self.submit_init_bus(SdMmcBusOp::SetBusWidth(BusWidth::Bit1))?
            }
            InitState::SetIdentificationClock => {
                self.submit_init_bus(SdMmcBusOp::SetClock(ClockSpeed::Identification))?
            }
            InitState::PostPowerOnDelay | InitState::PostIdentificationClockDelay => {
                InitActive::Delay(POWER_STABILIZATION_DELAY)
            }
            InitState::SetFourBitHost => {
                self.submit_init_bus(SdMmcBusOp::SetBusWidth(BusWidth::Bit4))?
            }
            InitState::SetHighSpeedHost => {
                self.submit_init_bus(SdMmcBusOp::SetClock(ClockSpeed::HighSpeed))?
            }
            InitState::Complete => return Ok(()),
            state => {
                let command = request.command_for_state(state)?;
                self.host.submit_command(&command)?;
                InitActive::Command
            }
        };
        request.active = active;
        Ok(())
    }

    fn submit_init_bus(&mut self, operation: SdMmcBusOp) -> Result<InitActive<H>, Error> {
        self.host.submit_bus_op(operation).map(InitActive::Bus)
    }

    fn consume_init_response(
        &mut self,
        request: &mut SdioInitRequest<H>,
        response: Response,
    ) -> Result<(), Error> {
        match request.state {
            InitState::QueryOcr | InitState::WaitForOcr => {
                let Response::R4(ocr) = response else {
                    return Err(bad_response(5));
                };
                request.consume_ocr(ocr)
            }
            InitState::AssignRca => {
                let Response::R6(response) = response else {
                    return Err(bad_response(3));
                };
                request.rca = response.rca();
                request.state = InitState::SelectCard;
                Ok(())
            }
            InitState::SelectCard => {
                expect_r1(response, 7)?;
                request.state = InitState::ReadCccrRevision;
                Ok(())
            }
            state => {
                let Response::R5(response) = response else {
                    return Err(bad_response(state.command_index()));
                };
                let value = check_r5(response, state.command_index())?;
                request.consume_register_value(state, value)
            }
        }
    }
}

impl<H: SdMmcIrqHost + 'static> SdioInitRequest<H> {
    fn new() -> Self {
        Self {
            state: InitState::ResetHost,
            active: InitActive::None,
            ocr: None,
            rca: 0,
            io_functions: 0,
            cmd5_polls: 0,
            cccr_revision: 0,
            sd_revision: 0,
            card_capability: 0,
            bus_interface: 0,
            bus_speed: 0,
            common_cis: CisInfo::default(),
            functions: [None; 7],
            pointer_bytes: [0; 3],
            block_size_bytes: [0; 2],
            cis_target: 0,
            cis_cursor: 0,
            cis_tuple_count: 0,
            cis_tuple_code: 0,
            cis_tuple_link: 0,
            cis_tuple_data: [0; 4],
            cis_tuple_data_index: 0,
        }
    }

    fn advance_after_bus(&mut self) -> Result<(), Error> {
        self.state = match self.state {
            InitState::ResetHost => InitState::PowerOn,
            InitState::PowerOn => InitState::PostPowerOnDelay,
            InitState::SetOneBitBus => InitState::SetIdentificationClock,
            InitState::SetIdentificationClock => InitState::PostIdentificationClockDelay,
            InitState::SetFourBitHost => {
                if self.bus_speed & BUS_SPEED_SUPPORT_HIGH_SPEED != 0 {
                    InitState::WriteHighSpeed
                } else {
                    InitState::Complete
                }
            }
            InitState::SetHighSpeedHost => InitState::Complete,
            _ => return Err(Error::InvalidArgument),
        };
        Ok(())
    }

    fn advance_after_delay(&mut self) -> Result<(), Error> {
        self.state = match self.state {
            InitState::PostPowerOnDelay => InitState::SetOneBitBus,
            InitState::PostIdentificationClockDelay => InitState::QueryOcr,
            _ => return Err(Error::InvalidArgument),
        };
        Ok(())
    }

    /// Return the task-context delay required before the next init step.
    ///
    /// Linux applies the host power delay once after enabling card power and
    /// again after starting the identification clock. Keeping the wait in the
    /// request makes that ordering explicit without sleeping in the protocol
    /// owner or relying on logging latency.
    pub const fn register_retry_after(&self) -> Option<Duration> {
        match self.active {
            InitActive::Delay(delay) => Some(delay),
            _ => None,
        }
    }

    fn command_for_state(&self, state: InitState) -> Result<Command, Error> {
        let command = match state {
            InitState::QueryOcr => cmd::CMD5,
            InitState::WaitForOcr => {
                cmd::cmd5(self.ocr.ok_or(Error::InvalidArgument)?.voltage_window() & OCR_3V2_3V4)
            }
            InitState::AssignRca => cmd::CMD3_SD,
            InitState::SelectCard => cmd::cmd7(self.rca),
            InitState::ReadCccrRevision => read_common(CCCR_SDIO_REVISION),
            InitState::ReadSdRevision => read_common(CCCR_SD_REVISION),
            InitState::ReadCardCapability => read_common(CCCR_CARD_CAPABILITY),
            InitState::ReadBusInterface => read_common(CCCR_BUS_INTERFACE),
            InitState::ReadBusSpeed => read_common(CCCR_BUS_SPEED_SELECT),
            InitState::ReadCommonCisPointer(byte) => {
                read_common(CCCR_CIS_POINTER + u32::from(byte))
            }
            InitState::ReadFunctionInterface(function) => read_common(fbr_base(function)),
            InitState::ReadFunctionCisPointer { function, byte } => {
                read_common(fbr_base(function) + FBR_CIS_POINTER + u32::from(byte))
            }
            InitState::ReadFunctionBlockSize { function, byte } => {
                read_common(fbr_base(function) + FBR_BLOCK_SIZE + u32::from(byte))
            }
            InitState::ReadCisTupleCode => read_common(self.cis_cursor),
            InitState::ReadCisTupleLink => read_common(self.cis_cursor + 1),
            InitState::ReadCisManufacturerByte => {
                read_common(self.cis_cursor + 2 + u32::from(self.cis_tuple_data_index))
            }
            InitState::WriteFourBitBus => cmd::cmd52(
                true,
                0,
                true,
                CCCR_BUS_INTERFACE,
                (self.bus_interface & !BUS_WIDTH_MASK) | BUS_WIDTH_4BIT,
            ),
            InitState::WriteHighSpeed => cmd::cmd52(
                true,
                0,
                true,
                CCCR_BUS_SPEED_SELECT,
                self.bus_speed | BUS_SPEED_ENABLE_HIGH_SPEED,
            ),
            InitState::PostPowerOnDelay | InitState::PostIdentificationClockDelay => {
                return Err(Error::InvalidArgument);
            }
            _ => return Err(Error::InvalidArgument),
        };
        Ok(command)
    }

    fn consume_ocr(&mut self, ocr: SdioOcrResponse) -> Result<(), Error> {
        if ocr.io_functions() == 0 {
            return Err(Error::NoIoFunctions);
        }
        let voltage = ocr.voltage_window() & OCR_3V2_3V4;
        if voltage == 0 {
            return Err(Error::UnsupportedCommand);
        }
        self.ocr = Some(ocr);
        self.io_functions = ocr.io_functions();
        if !ocr.io_ready() {
            self.cmd5_polls = self.cmd5_polls.saturating_add(1);
            if self.cmd5_polls >= MAX_CMD5_POLLS {
                return Err(Error::Timeout(ErrorContext::for_cmd(Phase::Init, 5)));
            }
            self.state = InitState::WaitForOcr;
            return Ok(());
        }
        if ocr.memory_present() {
            return Err(Error::UnsupportedComboCard);
        }
        self.state = InitState::AssignRca;
        Ok(())
    }

    fn consume_register_value(&mut self, state: InitState, value: u8) -> Result<(), Error> {
        match state {
            InitState::ReadCccrRevision => {
                self.cccr_revision = value;
                self.state = InitState::ReadSdRevision;
            }
            InitState::ReadSdRevision => {
                self.sd_revision = value;
                self.state = InitState::ReadCardCapability;
            }
            InitState::ReadCardCapability => {
                self.card_capability = value;
                self.state = InitState::ReadBusInterface;
            }
            InitState::ReadBusInterface => {
                self.bus_interface = value;
                self.state = InitState::ReadBusSpeed;
            }
            InitState::ReadBusSpeed => {
                self.bus_speed = value;
                self.pointer_bytes = [0; 3];
                self.state = InitState::ReadCommonCisPointer(0);
            }
            InitState::ReadCommonCisPointer(byte) => {
                self.pointer_bytes[usize::from(byte)] = value;
                if byte < 2 {
                    self.state = InitState::ReadCommonCisPointer(byte + 1);
                } else {
                    let pointer = pointer_from_bytes(self.pointer_bytes)?;
                    self.begin_cis(0, pointer);
                }
            }
            InitState::ReadFunctionInterface(function) => {
                let number = FunctionNumber::new(function)?;
                self.functions[usize::from(function - 1)] = Some(SdioFunctionInfo {
                    number,
                    interface_code: value & 0x0f,
                    block_size: None,
                    enabled: false,
                    interrupt_enabled: false,
                    cis: CisInfo::default(),
                });
                self.pointer_bytes = [0; 3];
                self.state = InitState::ReadFunctionCisPointer { function, byte: 0 };
            }
            InitState::ReadFunctionCisPointer { function, byte } => {
                self.pointer_bytes[usize::from(byte)] = value;
                if byte < 2 {
                    self.state = InitState::ReadFunctionCisPointer {
                        function,
                        byte: byte + 1,
                    };
                } else {
                    self.block_size_bytes = [0; 2];
                    self.state = InitState::ReadFunctionBlockSize { function, byte: 0 };
                }
            }
            InitState::ReadFunctionBlockSize { function, byte } => {
                self.block_size_bytes[usize::from(byte)] = value;
                if byte == 0 {
                    self.state = InitState::ReadFunctionBlockSize { function, byte: 1 };
                } else {
                    let index = usize::from(function - 1);
                    let size = u16::from_le_bytes(self.block_size_bytes);
                    let function_info = self.functions[index]
                        .as_mut()
                        .ok_or(Error::InvalidArgument)?;
                    function_info.block_size = NonZeroU16::new(size);
                    let pointer = pointer_from_bytes(self.pointer_bytes)?;
                    self.begin_cis(function, pointer);
                }
            }
            InitState::ReadCisTupleCode => self.consume_tuple_code(value)?,
            InitState::ReadCisTupleLink => self.consume_tuple_link(value)?,
            InitState::ReadCisManufacturerByte => self.consume_manfid_byte(value)?,
            InitState::WriteFourBitBus => self.state = InitState::SetFourBitHost,
            InitState::WriteHighSpeed => self.state = InitState::SetHighSpeedHost,
            _ => return Err(Error::InvalidArgument),
        }
        Ok(())
    }

    fn begin_cis(&mut self, target: u8, pointer: u32) {
        self.cis_target = target;
        self.cis_cursor = pointer;
        self.cis_tuple_count = 0;
        self.cis_tuple_code = 0;
        self.cis_tuple_link = 0;
        self.cis_tuple_data = [0; 4];
        self.cis_tuple_data_index = 0;
        if target == 0 {
            self.common_cis = CisInfo {
                pointer,
                ..CisInfo::default()
            };
        } else if let Some(function) = self.functions[usize::from(target - 1)].as_mut() {
            function.cis.pointer = pointer;
        }
        self.state = InitState::ReadCisTupleCode;
    }

    fn consume_tuple_code(&mut self, code: u8) -> Result<(), Error> {
        self.cis_tuple_count = self.cis_tuple_count.saturating_add(1);
        if self.cis_tuple_count > MAX_CIS_TUPLES {
            return Err(Error::MalformedCis);
        }
        match code {
            CISTPL_NULL => {
                self.advance_cis_cursor(1)?;
                self.state = InitState::ReadCisTupleCode;
            }
            CISTPL_END => self.finish_cis()?,
            _ => {
                self.cis_tuple_code = code;
                self.state = InitState::ReadCisTupleLink;
            }
        }
        Ok(())
    }

    fn consume_tuple_link(&mut self, link: u8) -> Result<(), Error> {
        self.cis_tuple_link = link;
        if self.cis_tuple_code == CISTPL_MANFID && link >= 4 {
            self.cis_tuple_data = [0; 4];
            self.cis_tuple_data_index = 0;
            self.state = InitState::ReadCisManufacturerByte;
        } else {
            self.advance_cis_cursor(2 + u32::from(link))?;
            self.state = InitState::ReadCisTupleCode;
        }
        Ok(())
    }

    fn consume_manfid_byte(&mut self, value: u8) -> Result<(), Error> {
        self.cis_tuple_data[usize::from(self.cis_tuple_data_index)] = value;
        self.cis_tuple_data_index += 1;
        if self.cis_tuple_data_index < 4 {
            self.state = InitState::ReadCisManufacturerByte;
            return Ok(());
        }
        let manufacturer_id = u16::from_le_bytes([self.cis_tuple_data[0], self.cis_tuple_data[1]]);
        let product_id = u16::from_le_bytes([self.cis_tuple_data[2], self.cis_tuple_data[3]]);
        if self.cis_target == 0 {
            self.common_cis.manufacturer_id = Some(manufacturer_id);
            self.common_cis.product_id = Some(product_id);
        } else {
            let function = self.functions[usize::from(self.cis_target - 1)]
                .as_mut()
                .ok_or(Error::InvalidArgument)?;
            function.cis.manufacturer_id = Some(manufacturer_id);
            function.cis.product_id = Some(product_id);
        }
        self.finish_cis()
    }

    fn finish_cis(&mut self) -> Result<(), Error> {
        if self.cis_target == 0 {
            self.state = InitState::ReadFunctionInterface(1);
        } else if self.cis_target < self.io_functions {
            self.state = InitState::ReadFunctionInterface(self.cis_target + 1);
        } else if supports_four_bit(self.card_capability) {
            self.state = InitState::WriteFourBitBus;
        } else if self.bus_speed & BUS_SPEED_SUPPORT_HIGH_SPEED != 0 {
            self.state = InitState::WriteHighSpeed;
        } else {
            self.state = InitState::Complete;
        }
        Ok(())
    }

    fn advance_cis_cursor(&mut self, count: u32) -> Result<(), Error> {
        self.cis_cursor = self
            .cis_cursor
            .checked_add(count)
            .filter(|address| *address <= 0x1_ffff)
            .ok_or(Error::MalformedCis)?;
        Ok(())
    }

    fn card_info(&self) -> Result<SdioCardInfo, Error> {
        let ocr = self.ocr.ok_or(Error::InvalidArgument)?;
        Ok(SdioCardInfo {
            rca: self.rca,
            ocr: ocr.raw,
            io_functions: self.io_functions,
            cccr_revision: self.cccr_revision,
            sd_revision: self.sd_revision,
            common_cis: self.common_cis,
        })
    }
}

impl InitState {
    const fn command_index(self) -> u8 {
        match self {
            Self::QueryOcr | Self::WaitForOcr => 5,
            Self::AssignRca => 3,
            Self::SelectCard => 7,
            Self::ResetHost
            | Self::PowerOn
            | Self::SetOneBitBus
            | Self::SetIdentificationClock
            | Self::SetFourBitHost
            | Self::SetHighSpeedHost
            | Self::Complete => 0,
            _ => 52,
        }
    }
}

fn pointer_from_bytes(bytes: [u8; 3]) -> Result<u32, Error> {
    let pointer = u32::from(bytes[0]) | (u32::from(bytes[1]) << 8) | (u32::from(bytes[2]) << 16);
    IoAddress::new(pointer).map(IoAddress::get)
}

const fn supports_four_bit(capability: u8) -> bool {
    capability & CARD_CAP_LOW_SPEED == 0 || capability & CARD_CAP_LOW_SPEED_4BIT != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cis_pointer_is_little_endian_and_bounded() {
        assert_eq!(pointer_from_bytes([0x56, 0x34, 0x01]).unwrap(), 0x1_3456);
        assert_eq!(pointer_from_bytes([0, 0, 2]), Err(Error::InvalidArgument));
    }
}
