#![no_std]
#![feature(trait_upcasting)]

extern crate alloc;

mod fs;
mod node;
mod path;
mod types;

pub use fs::*;
pub use node::*;
pub use path::*;
pub use types::*;

pub type VfsError = axerrno::AxError;
pub type VfsResult<T> = Result<T, VfsError>;
