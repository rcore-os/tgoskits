#![no_std]

#[macro_use]
extern crate axplat;

mod console;
mod init;
mod irq;
mod mem;
mod power;
mod time;

mod config {
    axconfig_macros::include_configs!(path_env = "AX_CONFIG_PATH", fallback = "axconfig.toml");
}

#[unsafe(no_mangle)]
unsafe extern "C" fn _start() -> ! {
    // TODO: Implement actual bootstrap logic
    axplat::call_main(0, 0);
}
