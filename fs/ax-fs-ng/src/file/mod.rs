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
#[cfg(feature = "ext4")]
pub(crate) use cache::retire_filesystem_cache;
#[cfg(feature = "vfs")]
pub(crate) use cache::start_background_writeback;
pub use cache::{
    CacheMappingEndpoint, CacheMappingEvent, CacheMappingResult, CachePageIdentity,
    CachePageoutDeferred, CachePageoutResult, CachedFile, CachedFileIdentity, CachedFrameIdentity,
    CachedPagePin,
};
#[cfg(feature = "vfs")]
pub use cache::{page_cache_reclaim, sync_all_cached_files, sync_filesystem_cached_files};
pub use handle::{File, FileBackend};
pub use open::{FileFlags, OpenOptions, OpenResult};
pub use page::PageCache;
