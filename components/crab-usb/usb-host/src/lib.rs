#![cfg_attr(target_os = "none", no_std)]
#[macro_use]
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

pub use crate::backend::ty::Event;
pub use crate::backend::ty::ep::Endpoint;
pub use host::*;

#[allow(unused_imports)]
#[cfg(kmod)]
pub use crate::backend::kmod::*;

define_int_type!(BusAddr, u64);

pub type Mmio = NonNull<u8>;
