//! File and inode data operations.

use alloc::{
    string::{String, ToString},
    vec::Vec,
};

use log::{debug, error, info, warn};

use crate::{
    blockdev::*,
    bmalloc::{AbsoluteBN, InodeNumber},
    checksum::update_ext4_dirblock_csum32,
    config::*,
    dir::*,
    disknode::*,
    entries::*,
    error::*,
    ext4::*,
    extents_tree::*,
    loopfile::*,
    metadata::{Ext4DtimeUpdate, Ext4InodeMetadataUpdate},
    superblock::Ext4Superblock,
};

mod blocks;
mod create;
mod delete;
mod io;
mod link;
mod rename;

pub use blocks::build_file_block_mapping;
pub use create::{create_symbol_link, mkfile};
pub use delete::{delete_dir, delete_file, unlink};
pub use io::{read_file, truncate, write_file};
pub use link::link;
pub use rename::{mv, rename};
