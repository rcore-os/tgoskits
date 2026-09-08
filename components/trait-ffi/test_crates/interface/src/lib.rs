//! Interfaces compiled independently of the provider and caller.
#![no_std]

pub type Sample = u16;

pub mod platform {
    pub type Local = u8;
    pub const WIDTH: usize = 2;
    use core::time::Duration;

    #[renamed_ffi::def_extern_trait(mod_path = "platform")]
    pub trait Clock {
        fn ticks() -> u64;
        fn relative(value: super::Sample) -> self::Local {
            value as self::Local
        }
        fn array(value: [u8; self::WIDTH]) -> [u8; self::WIDTH] {
            value
        }
        fn elapsed() -> Duration {
            Duration::from_nanos(Self::ticks())
        }
        #[cfg(feature = "extra")]
        fn extra() -> u64 {
            99
        }
    }
}

pub mod other {
    #[renamed_ffi::def_extern_trait(mod_path = "other")]
    pub trait Clock {
        fn ticks() -> u64;
    }
}
