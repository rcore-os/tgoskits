//! Extent tree node parsing and update helpers.

use alloc::{vec, vec::*};

use log::{debug, error};

use crate::{
    blockdev::*, bmalloc::AbsoluteBN, config::*, disknode::*, endian::*, error::*, ext4::*,
};

mod insert;
mod node;
mod parse;
mod remove;
mod root;
mod split;

pub use node::ExtentNode;
pub use root::ExtentTree;
