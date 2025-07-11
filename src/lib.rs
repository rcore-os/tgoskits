#![no_std]
#![feature(unbounded_shifts)]

mod devops_impl;

pub mod vgic;
pub use vgic::Vgic;

mod consts;
// mod vgicc;
mod interrupt;
mod list_register;
mod registers;
mod vgicd;

#[cfg(feature = "vgicv3")]
pub mod v3;
