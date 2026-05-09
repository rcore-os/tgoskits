#![cfg_attr(not(any(windows, unix)), no_std)]

#[macro_use]
extern crate alloc;

#[macro_use]
extern crate log;

#[macro_use]
mod _macros;

#[macro_use]
mod grf;

mod clock;

pub(crate) mod pinctrl;
mod rst;
mod syscon;
pub(crate) mod variants;

use core::ptr::NonNull;

pub use clock::{
    ClkId, ClockError, ClockResult, Cru, CruOp,
    pll::{PllClock, PllRateParams, PllRateTable, RockchipPllType},
};
pub use pinctrl::{GpioDirection, PinConfig, PinCtrl, PinCtrlOp, PinctrlResult, Pull, id::*};
pub use rst::{ResetRockchip, RstId};
pub use variants::*;

pub type Mmio = NonNull<u8>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocType {
    Rk3588,
}
