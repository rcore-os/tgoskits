#[repr(u8)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum EventType {
    Synchronization = 0x00,
    Key             = 0x01,
    Relative        = 0x02,
    Absolute        = 0x03,
    Misc            = 0x04,
    Switch          = 0x05,
    Led             = 0x11,
    Sound           = 0x12,
    ForceFeedback   = 0x15,
}

impl EventType {
    pub const MAX: u8 = 0x1f;
    pub const COUNT: u8 = Self::MAX + 1;

    pub const fn from_repr(value: u8) -> Option<Self> {
        match value {
            0x00 => Some(Self::Synchronization),
            0x01 => Some(Self::Key),
            0x02 => Some(Self::Relative),
            0x03 => Some(Self::Absolute),
            0x04 => Some(Self::Misc),
            0x05 => Some(Self::Switch),
            0x11 => Some(Self::Led),
            0x12 => Some(Self::Sound),
            0x15 => Some(Self::ForceFeedback),
            _ => None,
        }
    }

    pub const fn bits_count(self) -> usize {
        match self {
            Self::Synchronization => 0x10,
            Self::Key => 0x300,
            Self::Relative => 0x10,
            Self::Absolute => 0x40,
            Self::Misc => 0x08,
            Self::Switch => 0x12,
            Self::Led => 0x10,
            Self::Sound => 0x08,
            Self::ForceFeedback => 0x80,
        }
    }
}

/// An input event, as defined by the Linux input subsystem.
#[repr(C)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Event {
    pub event_type: u16,
    pub code: u16,
    pub value: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct AbsInfo {
    /// The minimum value for the axis.
    pub min: i32,
    /// The maximum value for the axis.
    pub max: i32,
    /// The fuzz value used to filter noise from the event stream.
    pub fuzz: i32,
    /// The size of the dead zone; values less than this will be reported as 0.
    pub flat: i32,
    /// The resolution for values reported for the axis.
    pub res: i32,
}
