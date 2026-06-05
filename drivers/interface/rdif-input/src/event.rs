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

    pub const fn bits_count(self) -> usize {
        match self {
            EventType::Synchronization => 0x10,
            EventType::Key => 0x300,
            EventType::Relative => 0x10,
            EventType::Absolute => 0x40,
            EventType::Misc => 0x08,
            EventType::Switch => 0x12,
            EventType::Led => 0x10,
            EventType::Sound => 0x08,
            EventType::ForceFeedback => 0x80,
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct InputEvent {
    pub event_type: u16,
    pub code: u16,
    pub value: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct AbsInfo {
    pub min: i32,
    pub max: i32,
    pub fuzz: i32,
    pub flat: i32,
    pub res: i32,
}
