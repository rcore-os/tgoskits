use crate::{GpioBankId, GpioLineId, OwnerId, PinctrlError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Input,
    Output { initial: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpioRange {
    pub bank: GpioBankId,
    pub pin_base: u32,
    pub line_base: u32,
    pub count: u32,
}

impl GpioRange {
    pub const fn new(bank: GpioBankId, pin_base: u32, line_base: u32, count: u32) -> Self {
        Self {
            bank,
            pin_base,
            line_base,
            count,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpioLineHandle {
    line: GpioLineId,
    owner: OwnerId,
}

impl GpioLineHandle {
    pub const fn new(line: GpioLineId, owner: OwnerId) -> Self {
        Self { line, owner }
    }

    pub const fn line(&self) -> GpioLineId {
        self.line
    }

    pub const fn owner(&self) -> OwnerId {
        self.owner
    }
}

pub trait GpioBank: Send + 'static {
    fn bank_id(&self) -> GpioBankId;

    fn line_count(&self) -> u32;

    fn request_line(
        &mut self,
        line: GpioLineId,
        owner: &str,
    ) -> Result<GpioLineHandle, PinctrlError>;

    fn release_line(&mut self, handle: GpioLineHandle) -> Result<(), PinctrlError>;

    fn set_direction(
        &mut self,
        handle: &GpioLineHandle,
        direction: Direction,
    ) -> Result<(), PinctrlError>;

    fn read(&self, handle: &GpioLineHandle) -> Result<bool, PinctrlError>;

    fn write(&mut self, handle: &GpioLineHandle, value: bool) -> Result<(), PinctrlError>;
}
