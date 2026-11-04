//! 64-bit x86_64 ALSA PCM userspace ABI.
//!
//! This crate defines the Linux `sound/asound.h` wire layouts needed by the
//! unchanged `alsa-lib` and `alsa-utils` 1.2.13 `hw` PCM and card-control
//! paths. It also provides operating-system independent parameter constraint
//! helpers. Backend state and per-open PCM state intentionally do not live
//! here.
//!
//! The layouts and command values are specific to 64-bit x86_64 Linux. They
//! are not a portable representation of every ALSA ABI.

#![no_std]

#[cfg(not(all(target_arch = "x86_64", target_pointer_width = "64")))]
compile_error!("alsa-pcm-uapi currently defines only the 64-bit x86_64 Linux ABI");

mod abi;
mod params;

pub use self::{
    abi::*,
    params::{
        ParamError, PcmConstraints, PcmFormat, SwParamsError, accept_hw_params, refine_hw_params,
    },
};
