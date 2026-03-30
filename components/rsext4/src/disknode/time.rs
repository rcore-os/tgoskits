/// Ext4 timestamp with second and nanosecond components.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Ext4Timestamp {
    pub sec: i64,
    pub nsec: u32,
}

impl Ext4Timestamp {
    pub const UNIX_EPOCH: Self = Self { sec: 0, nsec: 0 };
    pub const MAX_NSEC: u32 = 999_999_999;

    pub fn new(sec: i64, nsec: u32) -> Self {
        Self {
            sec,
            nsec: core::cmp::min(nsec, Self::MAX_NSEC),
        }
    }
}

/// POSIX-like timestamp selector for utimens style APIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Ext4TimeSpec {
    Set(Ext4Timestamp),
    Now,
    #[default]
    Omit,
}
