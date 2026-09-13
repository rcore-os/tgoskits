//! Core filesystem state, mount, allocation, and mkfs helpers.

use ::alloc::{collections::VecDeque, vec, vec::Vec};

use crate::{
    bitmap::{InodeBitmap, bitmap_utils::count_set_bits_in_bitmap},
    blockdev::*,
    blockgroup_description::*,
    bmalloc::*,
    cache::{bitmap::CacheKey, *},
    checksum::*,
    config::*,
    crc32c::ext4_superblock_has_metadata_csum,
    dir::*,
    disknode::*,
    endian::*,
    error::*,
    jbd2::{jbd2::*, jbdstruct::*},
    loopfile::*,
    superblock::*,
    tool::*,
};

mod alloc;
mod fs;
mod geometry;
mod lookup;
mod mkfs;
mod mmp;
mod mount;
mod orphan;
mod owned;
mod sync;
mod system_zone;

pub(crate) use fs::GroupCounters;
pub use fs::{Ext4FileSystem, FileSystemStats};
pub(crate) use geometry::BlockMapContext;
pub use mkfs::{
    BlockGroupLayout, FsLayoutInfo, MkfsOptions, compute_fs_layout, mkfs, mkfs_with_options,
};
pub use mount::MountOptions;
pub use owned::{
    CompletedDirectoryLookup, CompletedInodeRead, CompletedInodeTableRead, CompletedInodeWrite,
    CompletedLiveInodeRead, DirectoryBlockRequest, DirectoryCursor, DirectoryEntry,
    DirectoryEntryType, DirectoryLookupOutcome, DirectoryLookupPreparation, DirectoryReadCache,
    DirectoryReader, Ext4, FilePermissions, InodeBlockRequest, InodeDataRequest, InodeFlags,
    InodeInfo, InodeMetadataReader, InodeMetadataUpdate, InodeReadCache, InodeReadPreparation,
    LiveInodeRead, MutationContext, PreparedDirectoryLookup, PreparedInodeRead,
    PreparedInodeTableRead, PreparedInodeWrite, PreparedLiveInodeRead, PreparedUnmount,
    SpecialInodeKind, UnmountReceipt, ValidatedInodeRead, format,
};
pub use sync::umount;
pub(crate) use system_zone::SystemZoneMap;
