pub const DWC3_SCRATCHBUF_SIZE: usize = 0x1000;

pub const DWC3_EVENT_BUFFERS_SIZE: usize = 4 * 64;

pub const DWC3_REVISION_190A: u32 = 0x5533190a;
pub const DWC3_REVISION_194A: u32 = 0x5533194a;
pub const DWC3_REVISION_210A: u32 = 0x5533210a;
pub const DWC3_REVISION_250A: u32 = 0x5533250a;

pub const fn genmask(high: u32, low: u32) -> u64 {
    assert!(high < 64 && low < 64);
    assert!(high >= low);
    (u64::MAX << low) & (u64::MAX >> (63 - high))
}
