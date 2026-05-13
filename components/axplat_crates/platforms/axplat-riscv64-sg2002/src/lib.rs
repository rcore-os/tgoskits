#![no_std]

#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
#[macro_use]
extern crate log;
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
#[macro_use]
extern crate ax_plat;

#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
mod boot;
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
mod console;
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
mod init;
#[cfg(all(feature = "irq", any(target_arch = "riscv32", target_arch = "riscv64")))]
mod irq;
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
mod mem;
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
mod power;
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
mod time;

pub mod config {
    //! Platform configuration module.
    //!
    //! If the `AX_CONFIG_PATH` environment variable is set, it will load the configuration from the specified path.
    //! Otherwise, it will fall back to the `axconfig.toml` file in the current directory and generate the default configuration.
    //!
    //! If the `PACKAGE` field in the configuration does not match the package name, it will panic with an error message.
    use ax_plat::assert_str_eq;

    ax_config_macros::include_configs!(path_env = "AX_CONFIG_PATH", fallback = "axconfig.toml");
    assert_str_eq!(
        PACKAGE,
        env!("CARGO_PKG_NAME"),
        "`PACKAGE` field in the configuration does not match the Package name. Please check your \
         configuration file."
    );
}
