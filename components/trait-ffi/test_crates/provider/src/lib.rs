//! Provider with renamed dependencies and no direct macro dependency.
#![no_std]

struct Provider;
renamed_interface::platform::clock::impl_trait! {
    impl Clock for Provider { fn ticks() -> u64 { 123 } }
}
renamed_interface::other::clock::impl_trait! {
    impl Clock for Provider { fn ticks() -> u64 { 456 } }
}

use renamed_interface::WeakClock as ClockAlias;

#[renamed_interface::impl_extern_trait]
impl ClockAlias for Provider {
    fn base() -> u64 {
        90
    }
}
