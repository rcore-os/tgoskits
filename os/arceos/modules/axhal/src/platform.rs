#[cfg(all(not(test), not(feature = "myplat")))]
include!(concat!(env!("OUT_DIR"), "/selected_platform.rs"));

#[cfg(test)]
#[path = "dummy.rs"]
mod dummy;

pub mod selected {}
