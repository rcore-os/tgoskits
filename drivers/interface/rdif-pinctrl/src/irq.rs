use crate::{GpioBankId, GpioIrqSourceId, GpioLineId};

pub const MAX_GPIO_IRQ_EVENTS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpioIrqTrigger {
    EdgeRising,
    EdgeFalling,
    EdgeBoth,
    LevelHigh,
    LevelLow,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpioIrqError {
    Overflow,
    Spurious,
    Controller,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpioLineEvent {
    pub line: GpioLineId,
    pub trigger: GpioIrqTrigger,
}

impl GpioLineEvent {
    pub const fn new(line: GpioLineId, trigger: GpioIrqTrigger) -> Self {
        Self { line, trigger }
    }

    const fn empty() -> Self {
        Self {
            line: GpioLineId::new(GpioBankId::new(0), 0),
            trigger: GpioIrqTrigger::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpioIrqEvent {
    source: Option<GpioIrqSourceId>,
    lines: [GpioLineEvent; MAX_GPIO_IRQ_EVENTS],
    len: usize,
    error: Option<GpioIrqError>,
}

impl GpioIrqEvent {
    pub const fn none() -> Self {
        Self {
            source: None,
            lines: [GpioLineEvent::empty(); MAX_GPIO_IRQ_EVENTS],
            len: 0,
            error: None,
        }
    }

    pub fn from_line(line: GpioLineEvent) -> Self {
        let mut event = Self::none();
        let _ = event.push_line(line);
        event
    }

    pub const fn with_error(error: GpioIrqError) -> Self {
        Self {
            source: None,
            lines: [GpioLineEvent::empty(); MAX_GPIO_IRQ_EVENTS],
            len: 0,
            error: Some(error),
        }
    }

    pub const fn source(&self) -> Option<GpioIrqSourceId> {
        self.source
    }

    pub fn set_source(&mut self, source: GpioIrqSourceId) {
        self.source = Some(source);
    }

    pub fn push_line(&mut self, line: GpioLineEvent) -> bool {
        if self.len == self.lines.len() {
            self.error = Some(GpioIrqError::Overflow);
            return false;
        }
        self.lines[self.len] = line;
        self.len += 1;
        true
    }

    pub fn lines(&self) -> &[GpioLineEvent] {
        &self.lines[..self.len]
    }

    pub const fn error(&self) -> Option<GpioIrqError> {
        self.error
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0 && self.error.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpioIrqSourceInfo {
    pub id: GpioIrqSourceId,
    pub lines: alloc::vec::Vec<GpioLineId>,
}

impl GpioIrqSourceInfo {
    pub fn new(id: GpioIrqSourceId, lines: alloc::vec::Vec<GpioLineId>) -> Self {
        Self { id, lines }
    }
}

pub trait GpioIrqHandler: Send + 'static {
    fn handle_irq(&mut self) -> GpioIrqEvent;
}
