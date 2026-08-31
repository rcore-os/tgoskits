mod cache;
mod handle;
mod open;
mod page;

#[cfg(feature = "ext4")]
pub(crate) use cache::forget_cached_file_key;
pub use cache::{CachedFile, EvictListenerOwner};
#[cfg(feature = "vfs")]
pub use cache::{page_cache_reclaim, sync_all_cached_files};
pub use handle::{File, FileBackend};
pub use open::{FileFlags, OpenOptions, OpenResult};
pub use page::PageCache;
