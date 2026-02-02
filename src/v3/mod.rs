/// GICv3 ITS (Interrupt Translation Service) implementation.
pub mod gits;
mod registers;
mod utils;
/// GICv3 distributor implementation.
pub mod vgicd;
/// GICv3 redistributor implementation.
pub mod vgicr;
