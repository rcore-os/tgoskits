mod access;
mod cache;
mod handle;
mod open;
mod page;

pub use access::{ExecutableFile, WriteAccess};
#[cfg(all(test, feature = "vfs"))]
pub(crate) use cache::BusyDirtyCachedFile;
#[cfg(feature = "ext4")]
pub(crate) use cache::forget_cached_file_key;
#[cfg(feature = "fat")]
pub(crate) use cache::prune_cached_files;
#[cfg(feature = "ext4")]
pub(crate) use cache::prune_cached_files_retiring;
pub(crate) use cache::start_background_readahead;
#[cfg(feature = "vfs")]
pub(crate) use cache::start_background_writeback;
#[cfg(feature = "fat")]
pub(crate) use cache::sync_fat_cached_files;
pub use cache::{
    CacheMappingEndpoint, CacheMappingEvent, CacheMappingResult, CachePageIdentity,
    CachePageoutDeferred, CachePageoutResult, CachedFile, CachedFileIdentity, CachedFrameIdentity,
    CachedPagePin,
};
#[cfg(feature = "ext4")]
pub(crate) use cache::{
    observe_filesystem_persistence, retire_filesystem_cache, writeback_filesystem_pages,
    writeback_filesystem_pages_periodic,
};
#[cfg(feature = "vfs")]
pub use cache::{page_cache_reclaim, sync_all_cached_files, sync_filesystem_cached_files};
pub use handle::{File, FileBackend};
pub use open::{FileFlags, OpenOptions, OpenResult};
pub use page::{CachedPageBacking, CachedPageBytes, PageCache};
