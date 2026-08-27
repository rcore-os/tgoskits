use crate::data::DataDirection;

/// SD/SDIO/MMC command packet submitted on the CMD line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command {
    pub index: u8,
    pub argument: u32,
    pub response: ResponseType,
}

impl Command {
    pub const fn new(index: u8, argument: u32, response: ResponseType) -> Self {
        Self {
            index,
            argument,
            response,
        }
    }

    pub const fn index(self) -> u8 {
        self.index
    }

    pub const fn argument(self) -> u32 {
        self.argument
    }

    pub const fn with_response(self, response: ResponseType) -> Self {
        Self { response, ..self }
    }

    /// Direction of the data phase that follows this command when it is
    /// unambiguous from the command index alone.
    ///
    /// SDIO CMD53 carries its direction in the argument; CMD6 is also
    /// overloaded between ACMD6 and SWITCH_FUNC, so both return `None`.
    pub const fn data_direction(&self) -> Option<DataDirection> {
        match self.index {
            17 | 18 => Some(DataDirection::Read),
            24 | 25 => Some(DataDirection::Write),
            _ => None,
        }
    }

    /// Size in bytes of the data block when fixed by the command index.
    pub const fn data_block_size(&self) -> Option<u32> {
        match self.index {
            17 | 18 | 24 | 25 => Some(512),
            _ => None,
        }
    }

    /// Compute the SD SPI-mode CRC7 for this command packet.
    pub fn crc7(&self) -> u8 {
        let mut crc: u8 = 0;
        let token: u8 = 0x40 | (self.index & 0x3F);
        crc = crc7_update(crc, token);
        for byte in self.argument.to_be_bytes() {
            crc = crc7_update(crc, byte);
        }
        (crc << 1) | 1
    }

    /// Build the 6-byte SD SPI command packet.
    pub fn to_spi_bytes(&self) -> [u8; 6] {
        let crc = self.crc7();
        let token = 0x40 | (self.index & 0x3F);
        let arg = self.argument.to_be_bytes();
        [token, arg[0], arg[1], arg[2], arg[3], crc]
    }
}

fn crc7_update(crc: u8, byte: u8) -> u8 {
    let mut crc = crc;
    let mut data = byte;
    for _ in 0..8 {
        crc <<= 1;
        if (crc ^ data) & 0x80 != 0 {
            crc ^= 0x89;
        }
        data <<= 1;
    }
    crc
}

/// Command response shape expected from the card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResponseType {
    None,
    R1,
    R1b,
    R2,
    R3,
    R4,
    R5,
    R6,
    R7,
}

/// Raw response words harvested by a host controller.
///
/// ABI:
///
/// - 48-bit responses store their response payload in `words[0]`.
/// - R2/CID/CSD responses store four 32-bit words in most-significant-word
///   first order.
/// - Each word is the big-endian value of the corresponding response bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawResponse {
    pub ty: ResponseType,
    pub words: [u32; 4],
}

impl RawResponse {
    pub const fn new(ty: ResponseType, words: [u32; 4]) -> Self {
        Self { ty, words }
    }

    pub const fn empty() -> Self {
        Self {
            ty: ResponseType::None,
            words: [0; 4],
        }
    }
}
