//! The `/dev/mqueue` pseudo filesystem (Linux `mqueuefs`).
//!
//! Each open POSIX message queue appears as a regular file whose name is the
//! queue name without the leading `/`. Reading the file yields the Linux
//! status line
//!
//! ```text
//! QSIZE:<bytes> NOTIFY:<n> SIGNO:<n> NOTIFY_PID:<pid>
//! ```
//!
//! The directory listing is built on demand from the live queue registry, so
//! it always reflects the current set of queues.

use alloc::{borrow::Cow, boxed::Box, format, sync::Arc};

use axfs_ng_vfs::{Filesystem, NodePermission, VfsError, VfsResult};

use crate::{
    ipc::mqueue::{lookup_by_short_name, registry_names},
    pseudofs::{NodeOpsMux, SimpleDir, SimpleDirOps, SimpleFile, SimpleFs},
};

/// mqueuefs magic (Linux `MQUEUE_MAGIC`, `0x19800202`).
const MQUEUE_MAGIC: u32 = 0x1980_0202;

/// Root directory of `/dev/mqueue`: one file per registered queue.
struct MqueueDir {
    fs: Arc<SimpleFs>,
}

impl SimpleDirOps for MqueueDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(registry_names().into_iter().map(Cow::Owned))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let queue = lookup_by_short_name(name).ok_or(VfsError::NotFound)?;
        let file = {
            let queue = queue.clone();
            SimpleFile::new_regular(self.fs.clone(), move || {
                let (qsize, notify, signo, notify_pid) = queue.report();
                Ok(format!(
                    "QSIZE:{qsize} NOTIFY:{notify} SIGNO:{signo} NOTIFY_PID:{notify_pid}\n"
                ))
            })
        };
        // Report the mqueue inode's real metadata (Linux `mqueue_get_inode`
        // stamps `i_mode`/`i_uid`/`i_gid` and the inode times), so `stat` on
        // `/dev/mqueue/<name>` and a directory listing show the owning queue's
        // permission mode, creator uid/gid and maintained timestamps rather
        // than the pseudofs defaults.
        let (atime, ctime, mtime) = queue.times();
        file.set_attrs(
            NodePermission::from_bits_truncate(queue.mode()),
            queue.uid(),
            queue.gid(),
            atime,
            mtime,
            ctime,
        );
        // Linux `mqueue_get_inode` fixes the mqueuefs file's `i_size` at
        // `FILENT_SIZE` (80), the documented width of the `QSIZE:...NOTIFY_PID:...`
        // status line, not the live length of the rendered line. Report that so
        // `stat("/dev/mqueue/<name>")` matches Linux (st_size == 80).
        file.set_fixed_size(queue.inode_size());
        Ok(NodeOpsMux::File(file))
    }

    fn is_cacheable(&self) -> bool {
        // The queue set and each file's contents change at runtime, so the VFS
        // must re-resolve names and re-read content rather than cache them.
        false
    }
}

/// Build the mqueuefs filesystem for mounting at `/dev/mqueue`.
pub fn new_mqueuefs() -> Filesystem {
    SimpleFs::new_with("mqueue".into(), MQUEUE_MAGIC, |fs| {
        SimpleDir::new_maker(fs.clone(), Arc::new(MqueueDir { fs }))
    })
}
