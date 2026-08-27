//! Shared block cache between the block runtime and filesystem consumers,
//! modeled on Linux's block-device page cache and its `buffer_head` layer
//! (`fs/buffer.c`, `block/bdev.c`).
//!
//! # Linux design mapping
//!
//! | Linux | Here |
//! |---|---|
//! | bdev inode `address_space`, keyed by page index | [`BlockAddressSpace`], keyed by folio frame index, one per device |
//! | `folio` with attached buffers | [`CacheFolio`], 4 KiB or one device block, whichever is larger |
//! | `buffer_head` `BH_Uptodate`/`BH_Dirty` bits | [`BufferHead`] slot state (`BH_Mapped` is implicit: the cache is an identity mapping) |
//! | `PAGECACHE_TAG_DIRTY` tree mark | ordered `dirty_frames` index |
//! | `getblk` / `bread` | [`BlockAddressSpace`] folio lookup / [`BufferedBlockDevice`] buffered reads |
//! | `mark_buffer_dirty` | deferred one-folio writes (data reaches the device only at writeback) |
//! | `sync_dirty_buffers` | [`BlockAddressSpace::writeback_dirty`], submitting merged dirty runs |
//! | `invalidate_bdev` | [`BlockAddressSpace::invalidate_range`] after an indeterminate direct-write failure |
//! | partitions sharing one bdev cache | [`registry`], keyed by runtime-handle identity |
//!
//! [`registry`]: super::cache::registry
//!
//! # Deviations from Linux (recorded deliberately)
//!
//! * Writeback is synchronous and happens at `flush()`, eviction, and the
//!   last filesystem consumer's drop;
//!   Linux has per-BDI flusher threads. The current `FsBlockDevice` model
//!   is fully synchronous, so a WRITEBACK mark and a background flusher
//!   would have no observable effect.
//! * One sleepable lock serializes each device tree instead of per-folio
//!   locks. All current callers already serialize filesystem IO per
//!   instance; the shared tree only adds serialization between partitions
//!   of the same physical device.
//! * The metadata/data split is expressed at folio granularity: requests
//!   inside one folio take the buffered path, multi-folio requests go
//!   device-direct. Linux declares the same split at the filesystem layer
//!   (metadata via `__bread` into the bdev cache, file data via the inode
//!   page cache); the `FsBlockDevice` boundary only sees request shapes,
//!   and block-granular access is exactly the metadata-style pattern.
//!
//! # Crash-consistency contract
//!
//! `flush()` writes back every dirty slot and only then issues the device
//! barrier. rsext4's JBD2 commit already flushes the device before writing
//! the commit record, so deferring block writes into this layer preserves
//! journal ordering without any change to the commit sequence.

mod address_space;
mod buffer_head;
mod device;
mod folio;
mod folio_cache;
mod registry;

#[cfg(test)]
mod tests;

pub(crate) use device::BufferedBlockDevice;
#[cfg(feature = "vfs")]
pub(crate) use registry::reclaim_clean_folios;
pub use registry::sync_all_block_caches;
#[cfg(test)]
pub(super) use registry::{fail_registry_reserve_for_key_for_test, registry_contains_key_for_test};
