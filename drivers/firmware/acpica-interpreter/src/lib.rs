//! ACPICA 20260930, compiled freestanding from unchanged upstream C.
#![no_std]
extern crate alloc;
#[cfg(any(test, feature = "host-test"))]
extern crate std;
pub mod backend;
mod engine;
#[cfg(feature = "host-test")]
pub mod host;
mod osl;
pub use engine::{Engine, Mode, Node, Value};
pub use osl::aml_error_count;
/// An unmodified 32-bit ACPICA status code.
pub type Status = u32;
pub const OK: Status = 0;
pub const ERROR: Status = 1;
pub const NO_MEMORY: Status = 4;
pub const ALREADY_EXISTS: Status = 7;
pub const SUPPORT: Status = 15;
pub const LIMIT: Status = 16;
pub const TIME: Status = 17;
pub const BAD_PARAMETER: Status = 0x1001;
pub const TYPE: Status = 0x0008;
#[cfg(test)]
unsafe extern "C" {
    fn acpica_interpreter_abi_width() -> u32;
}
#[cfg(test)]
mod tests {
    #[test]
    fn c_size_matches_rust() {
        // SAFETY: constant, side-effect-free C ABI query.
        assert_eq!(
            unsafe { super::acpica_interpreter_abi_width() },
            usize::BITS
        );
    }
}

pub mod resources;

pub mod ec;

pub mod thermal;

pub mod routing;

pub mod gpe;
