#![no_std]

#[macro_use]
mod _macro;

pub mod irq;

/// Kernel error
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KError {
    Io,
    NoMem,
    Again,
    Busy,
    BadAddr(usize),
    InvalidArg { name: &'static str },
    Unknown(&'static str),
}

impl core::fmt::Display for KError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io => write!(f, "IO error"),
            Self::NoMem => write!(f, "No memory"),
            Self::Again => write!(f, "Try Again"),
            Self::Busy => write!(f, "Busy"),
            Self::BadAddr(addr) => write!(f, "Bad Address: {addr:#x}"),
            Self::InvalidArg { name } => write!(f, "Invalid Argument `{name}`"),
            Self::Unknown(err) => write!(f, "Unknown: {err}"),
        }
    }
}

impl core::error::Error for KError {}

custom_type!(
    #[doc="CPU hardware ID"],
    CpuId, usize, "{:#x}");
