//! Generate user space configuration.

#[rustfmt::skip]
core::include!(concat!(env!("OUT_DIR"), "/uspace_config.rs"));
