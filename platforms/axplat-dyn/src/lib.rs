#![no_std]

extern crate alloc;
extern crate ax_driver as _;
extern crate somehal;

#[macro_use]
extern crate ax_plat;
#[allow(unused_imports)]
#[macro_use]
extern crate log;

mod boot;
mod console;
pub mod drivers;
mod generic_timer;
mod init;
#[cfg(feature = "irq")]
mod irq;
mod mem;
mod platform;
mod power;

pub use boot::boot_stack_bounds;
pub use generic_timer::try_init_epoch_offset;

#[cfg(feature = "irq")]
pub fn enable_timer_irq() {
    somehal::timer::irq_enable();
}
#[cfg(all(feature = "irq", target_arch = "riscv64", feature = "hv"))]
pub use irq::register_virtual_irq_injector;

// pub mod config {
//     //! Platform configuration module.
//     //!
//     //! If the `AX_CONFIG_PATH` environment variable is set, it will load the configuration from the specified path.
//     //! Otherwise, it will fall back to the `axconfig.toml` file in the current directory and generate the default configuration.
//     //!
//     //! If the `PACKAGE` field in the configuration does not match the package name, it will panic with an error message.
//     ax_config_macros::include_configs!(path_env = "AX_CONFIG_PATH", fallback = "axconfig.toml");
//     assert_str_eq!(
//         PACKAGE,
//         env!("CARGO_PKG_NAME"),
//         "`PACKAGE` field in the configuration does not match the Package name. Please check your configuration file."
//     );
// }
