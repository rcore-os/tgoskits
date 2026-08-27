use ax_lazyinit::OnceLock;
use axfs_ng_vfs::{VfsError, VfsResult};

/// Boot-independent entropy supplied by the embedding OS or platform.
///
/// Implementations must provide cryptographically suitable bytes without
/// opening a filesystem path. Filesystem code can request entropy while the
/// root filesystem itself is still being mounted.
pub trait FsEntropyProvider: Send + Sync {
    fn fill_bytes(&self, output: &mut [u8]) -> VfsResult<()>;
}

static ENTROPY_PROVIDER: OnceLock<&'static dyn FsEntropyProvider> = OnceLock::new();

pub fn set_entropy_provider(provider: &'static dyn FsEntropyProvider) {
    ENTROPY_PROVIDER.call_once(|| provider);
}

pub fn fill_entropy(output: &mut [u8]) -> VfsResult<()> {
    ENTROPY_PROVIDER
        .get()
        .ok_or(VfsError::BadState)?
        .fill_bytes(output)
}

pub fn has_entropy_provider() -> bool {
    ENTROPY_PROVIDER.get().is_some()
}
