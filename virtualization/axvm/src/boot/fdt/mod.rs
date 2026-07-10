//! Shared FDT source root and the selected architecture's guest boot facade.

#[path = "../../arch/aarch64/boot/fdt/mod.rs"]
mod platform;

pub use platform::*;
