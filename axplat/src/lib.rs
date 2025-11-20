#![cfg_attr(not(test), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![doc = include_str!("../README.md")]

#[macro_use]
extern crate axplat_macros;

pub mod console;
pub mod init;
#[cfg(feature = "irq")]
pub mod irq;
pub mod mem;
pub mod power;
pub mod time;

pub use axplat_macros::main;
pub use crate_interface::impl_interface as impl_plat_interface;

#[cfg(feature = "smp")]
pub use axplat_macros::secondary_main;

#[doc(hidden)]
pub mod __priv {
    pub use const_str::equal as const_str_eq;
    pub use crate_interface::{call_interface, def_interface};
}

/// Checks that two strings are equal. If they are not equal, it will cause a compile-time
/// error. And the message will be printed if it is provided.
///
/// # Example
///
/// ```rust
/// extern crate axplat;
/// const A: &str = "hello";
/// const B: &str = "hello";
/// axplat::assert_str_eq!(A, B);
/// ```
///
/// ```compile_fail
/// extern crate axplat;
/// const A: &str = "hello";
/// const B: &str = "world";
/// axplat::assert_str_eq!(A, B, "A and B are not equal!");
/// ```
#[macro_export]
macro_rules! assert_str_eq {
    ($expect:expr, $actual:expr, $mes:literal) => {
        const _: () = assert!($crate::__priv::const_str_eq!($expect, $actual), $mes);
    };
    ($expect:expr, $actual:expr $(,)?) => {
        const _: () = assert!(
            $crate::__priv::const_str_eq!($expect, $actual),
            "assertion failed: expected != actual.",
        );
    };
}

/// Call the function decorated by [`axplat::main`][main] for the primary core.
///
/// This function should only be called by the platform implementer, not the kernel.
pub fn call_main(cpu_id: usize, arg: usize) -> ! {
    unsafe { __axplat_main(cpu_id, arg) }
}

/// Call the function decorated by [`axplat::secondary_main`][secondary_main] for secondary cores.
///
/// This function should only be called by the platform implementer, not the kernel.
#[cfg(feature = "smp")]
pub fn call_secondary_main(cpu_id: usize) -> ! {
    unsafe { __axplat_secondary_main(cpu_id) }
}

unsafe extern "Rust" {
    fn __axplat_main(cpu_id: usize, arg: usize) -> !;
    fn __axplat_secondary_main(cpu_id: usize) -> !;
}
