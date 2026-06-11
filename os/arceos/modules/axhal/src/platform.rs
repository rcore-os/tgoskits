#[cfg(all(not(test), not(feature = "host-test"), not(feature = "myplat")))]
include!(concat!(env!("OUT_DIR"), "/selected_platform.rs"));

#[cfg(any(test, feature = "host-test"))]
#[path = "dummy.rs"]
mod dummy;

pub mod selected {}
