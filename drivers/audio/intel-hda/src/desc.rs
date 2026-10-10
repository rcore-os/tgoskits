//! BDL and command wire formats, stereo 16-bit PCM at 48000 Hz.
pub const FORMAT: u16 = 0x11;
pub const PERIOD: usize = 4096;
pub const PERIODS: usize = 4;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct BufferDescriptor {
    pub address: u64,
    pub length: u32,
    pub flags: u32,
}
pub const fn verb(codec: u8, node: u8, operation: u16, payload: u16) -> u32 {
    ((codec as u32) << 28) | ((node as u32) << 20) | ((operation as u32) << 8) | payload as u32
}
