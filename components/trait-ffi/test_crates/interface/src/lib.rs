//! Interfaces compiled independently of the provider and caller.
#![no_std]
#![feature(linkage)]

pub use renamed_ffi::impl_extern_trait;

pub type Sample = u16;

pub mod platform {
    pub type Local = u8;
    pub const WIDTH: usize = 2;
    // The method exists only without `extra`; with `extra`, its type is also
    // absent so incorrectly re-evaluating cfg_attr cannot compile silently.
    #[cfg(not(feature = "extra"))]
    pub struct MissingType;
    use core::time::Duration;

    #[renamed_ffi::def_extern_trait(mod_path = "platform")]
    pub trait Clock {
        #[cfg_attr(feature = "extra", cfg_attr(all(), cfg(any())))]
        fn disabled_by_cfg_attr(_: MissingType);
        #[cfg_attr(not(feature = "extra"), cfg(any()))]
        fn conditional_default() -> u64 {
            77
        }
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

#[renamed_ffi::def_extern_trait(weak_default)]
pub trait WeakClock {
    fn base() -> u64 {
        1
    }
    fn derived() -> u64 {
        Self::base() + 1
    }
}
