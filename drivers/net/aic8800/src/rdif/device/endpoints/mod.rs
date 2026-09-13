//! Move-only construction and endpoint families exposed through RDIF.

mod control;
mod device;
mod irq;
mod startup;

pub use device::{AicRdifDevice, AicRdifOptions};
