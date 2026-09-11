//! Typed interfaces for reclaim capabilities that are not implemented yet.

use super::{FrameLease, PageObject};

/// A stable token identifying a future swap-cache entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SwapToken(u64);

impl SwapToken {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapError {
    /// Starry currently has no swap device or swap cache implementation.
    Unsupported,
    Busy,
    Io,
}

/// Capability boundary for anonymous swap.
///
/// Keeping this interface typed lets `MADV_PAGEOUT` and future reclaim code
/// report an explicit unsupported result instead of pretending that a page was
/// evicted.  Implementations must transfer frame ownership through
/// `FrameLease`; callers never exchange a bare physical address.
pub trait SwapProvider {
    fn swap_out(&self, page: &PageObject) -> Result<SwapToken, SwapError>;
    fn swap_in(&self, token: SwapToken, frame: FrameLease) -> Result<(), SwapError>;
}

/// Default provider used until a swap device is wired into Starry.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnsupportedSwap;

impl SwapProvider for UnsupportedSwap {
    fn swap_out(&self, _page: &PageObject) -> Result<SwapToken, SwapError> {
        Err(SwapError::Unsupported)
    }

    fn swap_in(&self, _token: SwapToken, _frame: FrameLease) -> Result<(), SwapError> {
        Err(SwapError::Unsupported)
    }
}
