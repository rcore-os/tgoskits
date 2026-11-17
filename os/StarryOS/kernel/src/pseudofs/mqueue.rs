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

use alloc::{borrow::Cow, boxed::Box, string::String, sync::Arc};

use ax_lazyinit::LazyLock;
use axfs_ng_vfs::{
    DirEntry, FileNode, Filesystem, Location, Mountpoint, NodePermission, NodeType, Reference,
    VfsError, VfsResult,
};

use crate::{
    ipc::mqueue::{MessageQueue, lookup_by_short_name, registry_names},
    pseudofs::{
        NodeOpsMux, SimpleDir, SimpleDirOps, SimpleFileBacking, SimpleFs, SimpleFsOwner,
        SimpleInode,
    },
};

const MQUEUE_MAGIC: u32 = 0x1980_0202;

struct MqueueFilesystem {
    owner: SimpleFsOwner,
    object_mount: Arc<Mountpoint>,
}

// The current IPC queue registry is global. Linux mqueue_get_tree likewise keys
// a shared superblock by the IPC namespace; another mount is a view of that owner.
static MQUEUE_FILESYSTEM: LazyLock<MqueueFilesystem> = LazyLock::new(|| {
    let owner = SimpleFs::new_with("mqueue".into(), MQUEUE_MAGIC, |context| {
        let root = SimpleDir::new_maker(context, Arc::new(MqueueDir))?;
        Ok(root)
    })
    .expect("initialize the permanent IPC message-queue filesystem");
    let object_mount = Mountpoint::new_root(&owner.as_filesystem());
    MqueueFilesystem {
        owner,
        object_mount,
    }
});

struct MqueueDir;

fn queue_file(queue: Arc<MessageQueue>) -> Arc<SimpleFileBacking> {
    SimpleFileBacking::from_inode(queue.inode_state(), move || Ok(queue.status_line()))
}

impl SimpleDirOps for MqueueDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(registry_names().into_iter().map(Cow::Owned))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let queue = lookup_by_short_name(name).ok_or(VfsError::NotFound)?;
        Ok(queue_file(queue).into())
    }

    fn is_cacheable(&self) -> bool {
        false
    }
}

pub(crate) fn mqueue_device() -> u64 {
    MQUEUE_FILESYSTEM.object_mount.device()
}

pub(crate) fn allocate_mqueue_inode(mode: NodePermission) -> VfsResult<Arc<SimpleInode>> {
    MQUEUE_FILESYSTEM
        .owner
        .context()
        .allocate_inode(NodeType::RegularFile, mode)
}

/// Selects the real queue inode for descriptor metadata, without a pathname lookup.
pub(crate) fn mqueue_location(queue: Arc<MessageQueue>) -> Location {
    let owner = &*MQUEUE_FILESYSTEM;
    let maker: super::FileMaker = queue_file(queue).into();
    let file = maker
        .prepare(owner.owner.clone())
        .expect("queue belongs to this IPC filesystem")
        .materialize();
    let entry = DirEntry::new_file(
        FileNode::new(file),
        NodeType::RegularFile,
        Reference::new(None, String::new()),
    );
    Location::new(owner.object_mount.clone(), entry)
}

/// Mounts another view of the current IPC namespace's filesystem instance.
pub fn new_mqueuefs() -> VfsResult<Filesystem> {
    Ok(MQUEUE_FILESYSTEM.owner.as_filesystem())
}
