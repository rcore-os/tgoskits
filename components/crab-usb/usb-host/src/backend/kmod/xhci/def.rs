use xhci::ring::trb::transfer;

use usb_if::transfer::Direction;

define_int_type!(SlotId, u8);

impl SlotId {
    pub fn as_u8(&self) -> u8 {
        self.0
    }

    pub fn as_usize(&self) -> usize {
        self.0 as usize
    }
}

define_int_type!(Dci, u8);

impl Dci {
    pub const CTRL: Self = Self(1);

    pub fn as_u8(&self) -> u8 {
        self.0
    }

    pub fn as_usize(&self) -> usize {
        self.0 as usize
    }
}

pub(crate) trait DirectionExt {
    fn to_xhci_direction(&self) -> transfer::Direction;
    fn to_xhci_transfer_type(&self) -> transfer::TransferType;
}

impl DirectionExt for Direction {
    fn to_xhci_direction(&self) -> transfer::Direction {
        match self {
            Direction::Out => transfer::Direction::Out,
            Direction::In => transfer::Direction::In,
        }
    }

    fn to_xhci_transfer_type(&self) -> transfer::TransferType {
        match self {
            Direction::Out => transfer::TransferType::Out,
            Direction::In => transfer::TransferType::In,
        }
    }
}
