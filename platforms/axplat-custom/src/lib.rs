#![no_std]

#[macro_use]
extern crate ax_plat;

mod config;
mod console;
mod init;
mod mem;
mod power;
mod time;

#[cfg(feature = "irq")]
mod irq;

pub use mem::boot_stack_bounds;
pub use time::{enable_timer_irq, try_init_epoch_offset};
