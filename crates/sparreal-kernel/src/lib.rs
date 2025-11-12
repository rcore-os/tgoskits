#![no_std]

#[macro_use]
extern crate alloc;
#[macro_use]
extern crate log;

pub mod hal;
mod lang;
pub mod os;
pub mod __export;

pub use sparreal_macros::entry;
