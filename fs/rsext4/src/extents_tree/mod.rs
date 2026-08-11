//! Extent tree node parsing and update helpers.

use alloc::{vec, vec::*};

use crate::{blockdev::*, bmalloc::AbsoluteBN, disknode::*, endian::*, error::*, ext4::*};

mod convert;
mod insert;
mod node;
mod parse;
mod remove;
mod root;
mod split;

pub use node::ExtentNode;
pub use parse::{ExtentBlockMapping, ExtentRun};
pub use root::ExtentTree;
