//! File and inode data operations.

use alloc::{
    string::{String, ToString},
    vec::Vec,
};

use crate::{
    blockdev::*,
    bmalloc::{AbsoluteBN, InodeNumber},
    checksum::update_ext4_dirblock_csum32,
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

pub use blocks::build_file_block_mapping_with_inode_num;
pub(crate) use create::{
    CreateInodePayload, create_inode_at, discard_unpublished_inode, error_after_cleanup,
};
pub use create::{create_symbol_link, create_symbol_link_with_owner, mkfile, mkfile_with_owner};
pub(crate) use delete::{
    DentryReplacement, ParentDirEntry, find_named_entry_in_parent, preflight_inode_free,
    remove_named_entry_at, replace_named_entry_at, unlink_inode_at,
};
pub use delete::{
    UnlinkOutcome, delete_dir, delete_file, free_inode, is_dir_empty, reap_unlinked_inode,
    remove_inodeentry_from_parentdir, unlink,
};
pub use io::{
    read_file, read_inode_data_into, truncate, truncate_inode, write_file, write_inode_data,
};
pub use link::link;
pub(crate) use link::link_inode_at;
pub(crate) use rename::{RenameEntryRequest, rename_inode_at};
pub use rename::{RenameOptions, RenameOutcome, mv, rename, rename_with_options};
