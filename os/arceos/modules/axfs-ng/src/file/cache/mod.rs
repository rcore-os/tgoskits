//! Generation-aware file caching and shared page-cache lifecycle.

mod handle;
mod shared;

pub use handle::CachedFile;
#[cfg(feature = "ext4")]
pub(crate) use shared::forget_cached_file_key;
#[cfg(feature = "vfs")]
pub use shared::{page_cache_reclaim, sync_all_cached_files};
