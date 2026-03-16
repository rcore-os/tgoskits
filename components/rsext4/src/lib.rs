#![no_std]
#![deny(unused)]
#![deny(dead_code)]
#![deny(warnings)]

extern crate alloc;
pub mod ext4_backend;
pub use ext4_backend::{api::*, blockdev::*, config::*, dir::*, error::*, ext4::*, file::*};
