#![no_std]
// Hardware driver conventions: keep register/helper routines and protocol
// scratch fields even when currently unused, index fixed channel tables
// directly, and allow the wide argument lists the firmware command protocol
// requires.
#![allow(dead_code)]
#![allow(clippy::needless_range_loop)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::redundant_pattern_matching)]

extern crate alloc;

pub mod common;
pub mod fdrv;
pub mod fw;
pub mod runtime;
pub mod wireless;

pub use runtime::set_runtime;
pub use wifi_host;
pub use wireless::{Aic8800Wifi, probe};
