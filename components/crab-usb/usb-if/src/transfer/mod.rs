use num_enum::{FromPrimitive, IntoPrimitive};

#[repr(u8)]
/// The direction of the data transfer.
#[derive(Copy, Clone, Debug, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub enum Direction {
    /// Out (Write Data)
    Out = 0,
    /// In (Read Data)
    In = 1,
}

impl Direction {
    pub fn from_address(addr: u8) -> Direction {
        match addr & Self::MASK {
            0 => Self::Out,
            _ => Self::In,
        }
    }
    const MASK: u8 = 0x80;

    pub fn from_raw(raw: u8) -> Direction {
        match raw {
            0 => Self::Out,
            _ => Self::In,
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone)]
pub struct BmRequestType {
    pub direction: Direction,
    pub request_type: RequestType,
    pub recipient: Recipient,
}

impl BmRequestType {
    pub const fn new(
        direction: Direction,
        transfer_type: RequestType,
        recipient: Recipient,
    ) -> BmRequestType {
        BmRequestType {
            direction,
            request_type: transfer_type,
            recipient,
        }
    }
}

impl From<BmRequestType> for u8 {
    fn from(value: BmRequestType) -> Self {
        ((value.direction as u8) << 7) | ((value.request_type as u8) << 5) | value.recipient as u8
    }
}

#[derive(Copy, Clone, Debug)]
#[repr(u8)]
pub enum RequestType {
    Standard = 0,
    Class = 1,
    Vendor = 2,
    Reserved = 3,
}

#[derive(Copy, Clone, Debug)]
#[repr(u8)]
pub enum Recipient {
    Device = 0,
    Interface = 1,
    Endpoint = 2,
    Other = 3,
}

#[derive(Debug, Clone, FromPrimitive, IntoPrimitive, Copy)]
#[repr(u8)]
pub enum Request {
    GetStatus = 0,
    ClearFeature = 1,
    SetFeature = 3,
    SetAddress = 5,
    GetDescriptor = 6,
    SetDescriptor = 7,
    GetConfiguration = 8,
    SetConfiguration = 9,
    GetInterface = 10,
    SetInterface = 11,
    SynchFrame = 12,
    SetEncryption = 13,
    GetEncryption = 14,
    SetHandshake = 15,
    GetHandshake = 16,
    SetConnection = 17,
    SetSecurityData = 18,
    GetSecurityData = 19,
    SetWusbData = 20,
    LoopbackDataWrite = 21,
    LoopbackDataRead = 22,
    SetInterfaceDs = 23,
    GetFwStatus = 26,
    SetFwStatus = 27,
    SetSel = 48,
    SetIsochDelay = 49,
    #[num_enum(catch_all)]
    Other(u8),
}
