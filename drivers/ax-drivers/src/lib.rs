//! rdrive + rdif host driver registration collection.

#![no_std]

extern crate alloc;

pub mod bindings;
pub mod error;
mod init;
mod registers;
mod source;

#[cfg(feature = "block")]
pub mod block;
#[cfg(feature = "display")]
pub mod display {}
#[cfg(feature = "input")]
pub mod input {}
#[cfg(feature = "net")]
pub mod net;
#[cfg(feature = "vsock")]
pub mod vsock {}

#[cfg(feature = "pci")]
pub mod pci;
#[cfg(virtio_dev)]
pub mod virtio;

#[macro_export]
macro_rules! register_driver {
    (
        $($i:ident : $t:expr),+,
    ) => {
        rdrive::__mod_maker!{
            pub mod some {
                use super::*;
                use rdrive::register::*;

                #[unsafe(link_section = ".driver.register")]
                #[unsafe(no_mangle)]
                #[used]
                pub static DRIVER: DriverRegister = DriverRegister {
                    $($i : $t),+
                };
            }
        }
    };
}

pub use error::{Error, Result};
pub use init::init_static_drivers;
pub use source::STATIC_DEVICES as static_devices;
