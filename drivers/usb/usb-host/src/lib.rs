#![cfg_attr(not(feature = "umod"), no_std)]
extern crate alloc;
#[macro_use]
extern crate log;
#[macro_use]
extern crate anyhow;

use core::ptr::NonNull;

pub use usb_if;

#[macro_use]
mod _macros;

pub(crate) mod backend;
pub mod device;
pub mod err;
mod host;

pub use host::*;

#[allow(unused_imports)]
#[cfg(kmod)]
pub use crate::backend::kmod::*;
pub use crate::backend::ty::{Event, ep::Endpoint};
#[cfg(kmod)]
pub use crate::backend::ty::{UsbIrqEvent, UsbIrqFault};

define_int_type!(BusAddr, u64);

pub type Mmio = NonNull<u8>;
