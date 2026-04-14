#![no_std]
#![doc = include_str!("../README.md")]

macro_rules! aarch64_only {
    ($($item:item)*) => {
        $(
            #[cfg(target_arch = "aarch64")]
            $item
        )*
    };
}

aarch64_only! {
    #[macro_use]
    extern crate log;

    pub mod generic_timer;

    #[cfg(feature = "irq")]
    pub mod gic;
    pub mod pl011;
    pub mod pl031;
    pub mod psci;
}
