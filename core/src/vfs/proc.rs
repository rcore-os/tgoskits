use alloc::{borrow::ToOwned, sync::Arc};
use axfs_ng_vfs::Filesystem;
use axsync::RawMutex;

use super::dynamic::{DynamicDir, DynamicFs};

pub fn new_procfs() -> Filesystem<RawMutex> {
    let mut root = DynamicDir::new();
    root.add_file(
        "mounts",
        Arc::new(|| "proc /proc proc rw,nosuid,nodev,noexec,relatime 0 0\n"),
    );
    DynamicFs::new("proc".to_owned(), 0x9fa0, Arc::new(root))
}
