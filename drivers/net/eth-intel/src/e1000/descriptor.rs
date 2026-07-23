#[repr(C)]
#[derive(bytemuck::Pod, bytemuck::Zeroable, Clone, Copy, Default)]
pub struct TxDesc {
    pub addr: u64,
    pub length: u16,
    pub cso: u8,
    pub cmd: u8,
    pub status: u8,
    pub css: u8,
    pub special: u16,
}

impl TxDesc {
    pub const CMD_EOP: u8 = 1 << 0;
    pub const CMD_IFCS: u8 = 1 << 1;
    pub const CMD_RS: u8 = 1 << 3;

    pub const STATUS_DD: u8 = 1 << 0;

    pub fn new(addr: u64, length: u16) -> Self {
        Self {
            addr,
            length,
            cso: 0,
            cmd: Self::CMD_EOP | Self::CMD_IFCS | Self::CMD_RS,
            status: 0,
            css: 0,
            special: 0,
        }
    }

    pub fn is_done(&self) -> bool {
        self.status & Self::STATUS_DD != 0
    }
}

#[repr(C)]
#[derive(bytemuck::Pod, bytemuck::Zeroable, Clone, Copy, Default)]
pub struct RxDesc {
    pub addr: u64,
    pub length: u16,
    pub checksum: u16,
    pub status: u8,
    pub errors: u8,
    pub special: u16,
}

impl RxDesc {
    pub const STATUS_DD: u8 = 1 << 0;

    pub fn new(addr: u64) -> Self {
        Self {
            addr,
            length: 0,
            checksum: 0,
            status: 0,
            errors: 0,
            special: 0,
        }
    }

    pub fn is_done(&self) -> bool {
        self.status & Self::STATUS_DD != 0
    }
}
