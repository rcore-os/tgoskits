#![no_std]

#[cfg(all(axtest, feature = "axtest"))]
extern crate alloc;

#[macro_use]
mod _macro;

pub mod irq;

#[cfg(all(axtest, feature = "axtest"))]
pub mod axtest;

/// Kernel error
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum KError {
    #[error("IO error")]
    Io,
    #[error("No memory")]
    NoMem,
    #[error("Try Again")]
    Again,
    #[error("Busy")]
    Busy,
    #[error("Bad Address: {0:#x}")]
    BadAddr(usize),
    #[error("Invalid Argument `{name}`")]
    InvalidArg { name: &'static str },
    #[error("Unknown: {0}")]
    Unknown(&'static str),
}

custom_type!(
    #[doc="CPU hardware ID"],
    CpuId, usize, "{:#x}");
