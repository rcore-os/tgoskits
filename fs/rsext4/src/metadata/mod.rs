//! Inode metadata update helpers and path-based metadata APIs.

mod api;
mod apply;
mod inode;
mod time;
mod types;

pub use api::{chmod, chown, set_flags, set_project, utimens};
pub(crate) use types::{
    Ext4DtimeUpdate, Ext4InodeMetadataUpdate, Ext4MetadataReason, Ext4ModeUpdate,
};
