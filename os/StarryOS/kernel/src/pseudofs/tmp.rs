use alloc::{borrow::ToOwned, string::String, sync::Arc, vec::Vec};
use core::{
    any::Any,
    borrow::Borrow,
    cmp::Ordering,
    sync::atomic::{AtomicU64, Ordering as AtomicOrdering},
    task::Context,
    time::Duration,
};

use ax_kspin::SpinNoIrq;
use ax_sync::Mutex;
use axfs_ng_vfs::{
    DeviceId, DirEntry, DirEntrySink, DirNode, DirNodeOps, FileNode, FileNodeOps, Filesystem,
    FilesystemOps, FsIoEvents, FsPollable, Metadata, MetadataUpdate, NodeFlags, NodeOps,
    NodePermission, NodeType, Reference, StatFs, VfsError, VfsResult, WeakDirEntry,
};
use axpoll::{IoEvents, Pollable};
use hashbrown::HashMap;
use slab::Slab;

/// StatFs total block count reported for tmpfs (~4 GiB at 4096-byte blocks).
const TMPFS_REPORTED_BLOCKS: u64 = 1 << 20;
/// StatFs free inode count reported for tmpfs.
const TMPFS_REPORTED_FREE_INODES: u64 = 1 << 16;

const TMPFS_NESTED_DIR_ENTRIES_SUBCLASS: u32 = 1;

fn fs_events_to_io(events: FsIoEvents) -> IoEvents {
    IoEvents::from_bits_truncate(events.bits())
}

fn io_events_to_fs(events: IoEvents) -> FsIoEvents {
    FsIoEvents::from_bits_truncate(events.bits())
}

#[derive(PartialEq, Eq, Hash, Clone)]
struct FileName(Arc<str>);

impl PartialOrd for FileName {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FileName {
    fn cmp(&self, other: &Self) -> Ordering {
        fn index(s: &str) -> u8 {
            match s {
                "." => 0,
                ".." => 1,
                _ => 2,
            }
        }
        (index(self.0.as_ref()), self.0.as_ref()).cmp(&(index(other.0.as_ref()), other.0.as_ref()))
    }
}

impl<T> From<T> for FileName
where
    T: Into<String>,
{
    fn from(name: T) -> Self {
        Self(Arc::from(name.into().into_boxed_str()))
    }
}

impl Borrow<str> for FileName {
    fn borrow(&self) -> &str {
        self.0.as_ref()
    }
}

/// A simple in-memory filesystem that supports basic file operations.
pub struct MemoryFs {
    // Inodes may be released from atomic cleanup paths, so the slab and
    // metadata locks must not sleep.
    inodes: SpinNoIrq<Slab<Arc<Inode>>>,
    // root_dir() is used while mounting pseudofs during early startup, before
    // Starry has reached a sleepable task context.
    root: SpinNoIrq<Option<DirEntry>>,
}

impl MemoryFs {
    /// Creates a new empty memory filesystem.
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> Filesystem {
        let (fs, handle) = Self::new_with_handle();
        drop(handle);
        fs
    }

    /// Creates a new empty memory filesystem and returns a handle to the
    /// underlying `MemoryFs` so callers can create anonymous (unlinked) nodes.
    pub fn new_with_handle() -> (Filesystem, Arc<Self>) {
        let handle = Arc::new(Self {
            inodes: SpinNoIrq::new(Slab::new()),
            root: SpinNoIrq::new(None),
        });
        let root_ino = Inode::new(
            &handle,
            None,
            NodeType::Directory,
            NodePermission::from_bits_truncate(0o755),
            0,
            0,
            0,
        );
        *handle.root.lock() = Some(DirEntry::new_dir(
            |this| DirNode::new(MemoryNode::new(handle.clone(), root_ino, Some(this))),
            Reference::root(),
        ));
        (Filesystem::new(handle.clone()), handle)
    }

    fn get(&self, ino: u64) -> Arc<Inode> {
        self.inodes.lock()[ino as usize - 1].clone()
    }

    /// Creates an anonymous (unlinked) regular file inode within this tmpfs.
    ///
    /// The returned entry is not inserted into any directory, so it has no
    /// path-based lookup and is kept alive solely by the returned handle(s).
    pub fn create_anonymous_file(
        self: &Arc<Self>,
        name: &str,
        perm: NodePermission,
        uid: u32,
        gid: u32,
    ) -> DirEntry {
        let inode = Inode::new(self, None, NodeType::RegularFile, perm, uid, gid, 0);
        DirEntry::new_file(
            FileNode::new(MemoryNode::new(self.clone(), inode, None)),
            NodeType::RegularFile,
            Reference::new(None, name.to_owned()),
        )
    }
}

impl FilesystemOps for MemoryFs {
    fn name(&self) -> &str {
        "tmpfs"
    }

    fn root_dir(&self) -> DirEntry {
        self.root.lock().clone().unwrap()
    }

    fn stat(&self) -> VfsResult<StatFs> {
        // Override dummy_stat_fs (which reports 50 KiB total = 100 blocks * 512 B):
        // BookKeeper / RocksDB / many Java servers refuse to allocate ledger / SST
        // / WAL when File.getUsableSpace() < minUsableSizeForEntryLogCreation
        // (default 1 GiB), throwing NoWritableLedgerDirException from a critical
        // bookie thread that exits the JVM silently. Pulsar standalone died here.
        // Linux tmpfs reports total = total_RAM / 2 by default; we lack a proper
        // accounting layer, so advertise 4 GiB / 4 GiB free with realistic block
        // size, which is enough to unblock every Java server we've hit and remains
        // accurate when the guest VM has >= 2 GiB.
        Ok(StatFs {
            fs_type: 0x01021994,
            block_size: 4096,
            blocks: TMPFS_REPORTED_BLOCKS,
            blocks_free: TMPFS_REPORTED_BLOCKS,
            blocks_available: TMPFS_REPORTED_BLOCKS,
            file_count: 0,
            free_file_count: TMPFS_REPORTED_FREE_INODES,
            name_length: axfs_ng_vfs::path::MAX_NAME_LEN as _,
            fragment_size: 4096,
            mount_flags: 0,
        })
    }
}

fn release_inode(fs: &MemoryFs, inode: &Arc<Inode>, nlink: u64) {
    let mut inodes = fs.inodes.lock();
    let mut metadata = inode.metadata.lock();
    metadata.nlink -= nlink;
    if metadata.nlink == 0 && Arc::strong_count(inode) == 2 {
        inodes.remove(metadata.inode as usize - 1);
    }
}

#[derive(Default)]
struct FileContent {
    /// The length of the file content.
    ///
    /// We only need to store the length here because we delegate the actual
    /// content management to page cache.
    length: Mutex<u64>,
    symlink: Mutex<Option<String>>,
}

struct DirContent {
    // VFS dentry-cache operations call tmpfs directory ops while holding
    // SpinNoIrq guards, so this per-directory map must not use a blocking
    // mutex.
    entries: SpinNoIrq<HashMap<FileName, InodeRef>>,
    next_cookie: AtomicU64,
}

impl Default for DirContent {
    fn default() -> Self {
        Self {
            entries: SpinNoIrq::new(HashMap::new()),
            next_cookie: AtomicU64::new(3),
        }
    }
}

enum NodeContent {
    File(FileContent),
    Dir(DirContent),
}

struct Inode {
    ino: u64,
    metadata: SpinNoIrq<Metadata>,
    content: NodeContent,
}

impl Inode {
    pub fn new(
        fs: &Arc<MemoryFs>,
        parent: Option<u64>,
        node_type: NodeType,
        permission: NodePermission,
        uid: u32,
        gid: u32,
        dir_entries_subclass: u32,
    ) -> Arc<Inode> {
        let mut inodes = fs.inodes.lock();
        let entry = inodes.vacant_entry();
        let ino = entry.key() as u64 + 1;
        let metadata = Metadata {
            device: 0,
            inode: ino,
            nlink: 0,
            mode: permission,
            node_type,
            uid,
            gid,
            size: 0,
            // Linux's tmpfs reports PAGE_SIZE so userspace sees a nonzero
            // st_blksize; several libcs rely on this being > 0.
            block_size: 4096,
            blocks: 0,
            rdev: DeviceId::default(),
            atime: Duration::default(),
            mtime: Duration::default(),
            ctime: Duration::default(),
        };
        let content = match node_type {
            NodeType::Directory => NodeContent::Dir(DirContent::default()),
            _ => NodeContent::File(FileContent::default()),
        };
        let result = Arc::new(Self {
            ino,
            metadata: SpinNoIrq::new(metadata),
            content,
        });
        entry.insert(result.clone());
        drop(inodes);
        if let NodeContent::Dir(dir) = &result.content {
            let mut entries = dir.entries.lock_nested(dir_entries_subclass);
            entries.insert(
                ".".into(),
                InodeRef::new(fs.clone(), ino, NodeType::Directory, 1),
            );
            entries.insert(
                "..".into(),
                InodeRef::new(fs.clone(), parent.unwrap_or(ino), NodeType::Directory, 2),
            );
        }
        result
    }

    fn as_file(&self) -> VfsResult<&FileContent> {
        match self.content {
            NodeContent::File(ref content) => Ok(content),
            _ => Err(VfsError::IsADirectory),
        }
    }

    fn as_dir(&self) -> VfsResult<&DirContent> {
        match self.content {
            NodeContent::Dir(ref content) => Ok(content),
            _ => Err(VfsError::NotADirectory),
        }
    }
}

struct InodeRef {
    fs: Arc<MemoryFs>,
    ino: u64,
    node_type: NodeType,
    cookie: u64,
}

impl InodeRef {
    pub fn new(fs: Arc<MemoryFs>, ino: u64, node_type: NodeType, cookie: u64) -> Self {
        fs.get(ino).metadata.lock().nlink += 1;
        Self {
            fs,
            ino,
            node_type,
            cookie,
        }
    }

    fn get(&self) -> Arc<Inode> {
        self.fs.get(self.ino)
    }

    fn metadata_for_readdir(&self) -> (u64, NodeType) {
        (self.ino, self.node_type)
    }
}

impl Drop for InodeRef {
    fn drop(&mut self) {
        release_inode(&self.fs, &self.get(), 1);
    }
}

struct MemoryNode {
    fs: Arc<MemoryFs>,
    inode: Arc<Inode>,
    this: Option<WeakDirEntry>,
}

impl MemoryNode {
    pub fn new(fs: Arc<MemoryFs>, inode: Arc<Inode>, this: Option<WeakDirEntry>) -> Arc<Self> {
        Arc::new(Self { fs, inode, this })
    }

    fn new_entry(&self, name: &str, node_type: NodeType, inode: Arc<Inode>) -> VfsResult<DirEntry> {
        let fs = self.fs.clone();
        let reference = Reference::new(
            self.this.as_ref().and_then(WeakDirEntry::upgrade),
            name.to_owned(),
        );
        Ok(if node_type == NodeType::Directory {
            DirEntry::new_dir(
                |this| DirNode::new(MemoryNode::new(fs, inode, Some(this))),
                reference,
            )
        } else {
            DirEntry::new_file(
                FileNode::new(MemoryNode::new(fs, inode, None)),
                node_type,
                reference,
            )
        })
    }

    fn clear_dir_entries(inode: &Inode) {
        // Do this from unlink/rename paths while still in normal syscall
        // context. MemoryNode::drop may run during task cleanup, where a
        // blocking directory-entry mutex would panic in might_sleep().
        if let NodeContent::Dir(dir) = &inode.content {
            dir.entries.lock().clear();
        }
    }
}

impl NodeOps for MemoryNode {
    fn inode(&self) -> u64 {
        self.inode.ino
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        let mut metadata = self.inode.metadata.lock().clone();
        match &self.inode.content {
            NodeContent::File(content) => {
                metadata.size = *content.length.lock();
            }
            NodeContent::Dir(dir) => {
                metadata.size = dir.entries.lock().len() as u64;
            }
        }
        Ok(metadata)
    }

    fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()> {
        let mut metadata = self.inode.metadata.lock();
        if let Some(mode) = update.mode {
            metadata.mode = mode;
        }
        if let Some((uid, gid)) = update.owner {
            metadata.uid = uid;
            metadata.gid = gid;
        }
        if let Some(rdev) = update.rdev {
            metadata.rdev = rdev;
        }
        if let Some(atime) = update.atime {
            metadata.atime = atime;
        }
        if let Some(mtime) = update.mtime {
            metadata.mtime = mtime;
        }
        Ok(())
    }

    fn filesystem(&self) -> &dyn FilesystemOps {
        self.fs.as_ref()
    }

    fn sync(&self, _data_only: bool) -> VfsResult<()> {
        Ok(())
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::ALWAYS_CACHE
    }
}

impl FileNodeOps for MemoryNode {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let file = self.inode.as_file()?;
        if let Some(symlink) = file.symlink.lock().as_ref() {
            assert_eq!(offset, 0);
            let len = buf.len().min(symlink.len());
            buf[..len].copy_from_slice(&symlink.as_bytes()[..len]);
            return Ok(len);
        }
        unreachable!("page cache should handle reading");
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        unreachable!("page cache should handle writing");
    }

    fn append(&self, _buf: &[u8]) -> VfsResult<(usize, u64)> {
        unreachable!("page cache should handle writing");
    }

    fn set_len(&self, len: u64) -> VfsResult<()> {
        *self.inode.as_file()?.length.lock() = len;
        Ok(())
    }

    fn set_symlink(&self, target: &str) -> VfsResult<()> {
        let file = self.inode.as_file()?;
        *file.length.lock() = target.len() as u64;
        *file.symlink.lock() = Some(target.to_owned());
        Ok(())
    }
}
impl FsPollable for MemoryNode {
    fn poll(&self) -> FsIoEvents {
        FsIoEvents::IN | FsIoEvents::OUT
    }

    fn register(&self, _context: &mut Context<'_>, _events: FsIoEvents) {}
}

impl Pollable for MemoryNode {
    fn poll(&self) -> IoEvents {
        fs_events_to_io(FsPollable::poll(self))
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        FsPollable::register(self, context, io_events_to_fs(events));
    }
}

impl DirNodeOps for MemoryNode {
    fn read_dir(&self, offset: u64, sink: &mut dyn DirEntrySink) -> VfsResult<usize> {
        let dir = self.inode.as_dir()?;
        let entries = loop {
            let entries = dir.entries.lock();
            let count = entries
                .values()
                .filter(|entry| offset == 0 || entry.cookie >= offset)
                .count();
            drop(entries);

            let mut snapshot = Vec::new();
            snapshot
                .try_reserve(count)
                .map_err(|_| VfsError::NoMemory)?;

            let entries = dir.entries.lock();
            let live_count = entries
                .values()
                .filter(|entry| offset == 0 || entry.cookie >= offset)
                .count();
            if live_count > snapshot.capacity() {
                continue;
            }
            for (name, entry) in entries.iter() {
                if offset != 0 && entry.cookie < offset {
                    continue;
                }
                let (ino, node_type) = entry.metadata_for_readdir();
                snapshot.push((entry.cookie, name.0.clone(), ino, node_type));
            }
            snapshot.sort_by_key(|(cookie, ..)| *cookie);
            break snapshot;
        };

        let mut count = 0;
        for (cookie, name, ino, node_type) in entries {
            if !sink.accept(name.as_ref(), ino, node_type, cookie + 1) {
                return Ok(count);
            }
            count += 1;
        }
        Ok(count)
    }

    fn lookup(&self, name: &str) -> VfsResult<DirEntry> {
        let dir = self.inode.as_dir()?;
        let entries = dir.entries.lock();

        let entry = entries.get(name).ok_or(VfsError::NotFound)?;
        let inode = entry.get();
        let node_type = inode.metadata.lock().node_type;
        self.new_entry(name, node_type, inode)
    }

    fn create(
        &self,
        name: &str,
        node_type: NodeType,
        permission: NodePermission,
        uid: u32,
        gid: u32,
    ) -> VfsResult<DirEntry> {
        let dir = self.inode.as_dir()?;
        let mut entries = dir.entries.lock();

        if entries.contains_key(name) {
            return Err(VfsError::AlreadyExists);
        }
        let inode = Inode::new(
            &self.fs,
            Some(self.inode.ino),
            node_type,
            permission,
            uid,
            gid,
            TMPFS_NESTED_DIR_ENTRIES_SUBCLASS,
        );
        let cookie = dir.next_cookie.fetch_add(1, AtomicOrdering::Relaxed);
        entries.insert(
            name.into(),
            InodeRef::new(self.fs.clone(), inode.ino, node_type, cookie),
        );
        self.new_entry(name, node_type, inode)
    }

    fn link(&self, name: &str, target: &DirEntry) -> VfsResult<DirEntry> {
        let dir = self.inode.as_dir()?;
        let mut entries = dir.entries.lock();

        let target = target.downcast::<Self>()?;

        if entries.contains_key(name) {
            return Err(VfsError::AlreadyExists);
        }
        let inode = target.inode.clone();
        let node_type = inode.metadata.lock().node_type;
        let cookie = dir.next_cookie.fetch_add(1, AtomicOrdering::Relaxed);
        entries.insert(
            name.into(),
            InodeRef::new(self.fs.clone(), inode.ino, node_type, cookie),
        );
        self.new_entry(name, node_type, inode)
    }

    fn unlink(&self, name: &str, is_dir: bool) -> VfsResult<()> {
        let dir = self.inode.as_dir()?;

        let (entry, inode) = {
            let mut entries = dir.entries.lock();
            let Some(entry) = entries.get(name) else {
                return Err(VfsError::NotFound);
            };
            let inode = entry.get();
            match (&inode.content, is_dir) {
                (NodeContent::Dir(_), false) => return Err(VfsError::IsADirectory),
                (NodeContent::Dir(DirContent { entries, .. }), true)
                    if entries.lock_nested(TMPFS_NESTED_DIR_ENTRIES_SUBCLASS).len() > 2 =>
                {
                    return Err(VfsError::DirectoryNotEmpty);
                }
                (NodeContent::File(_), true) => return Err(VfsError::NotADirectory),
                _ => {}
            }
            let entry = entries.remove(name).ok_or(VfsError::NotFound)?;
            (entry, inode)
        };

        Self::clear_dir_entries(&inode);
        drop(entry);

        Ok(())
    }

    // TODO: atomicity
    fn rename(&self, src_name: &str, dst_dir: &DirNode, dst_name: &str) -> VfsResult<()> {
        let dst_node = dst_dir.downcast::<Self>()?;
        if let Ok(entry) = dst_dir.lookup(dst_name) {
            let src_entry = self.lookup(src_name)?;
            if entry.inode() == src_entry.inode() {
                return Ok(());
            }
        }

        let src_entry = {
            let mut entries = self.inode.as_dir()?.entries.lock();
            entries.remove(src_name).ok_or(VfsError::NotFound)?
        };
        let dst_dir = dst_node.inode.as_dir()?;
        let cookie = dst_dir.next_cookie.fetch_add(1, AtomicOrdering::Relaxed);
        let moved_entry = InodeRef::new(
            src_entry.fs.clone(),
            src_entry.ino,
            src_entry.node_type,
            cookie,
        );
        let overwritten = {
            let mut entries = dst_dir.entries.lock();
            entries.insert(dst_name.into(), moved_entry)
        };
        drop(src_entry);
        if let Some(entry) = overwritten {
            Self::clear_dir_entries(&entry.get());
            drop(entry);
        }
        Ok(())
    }
}

impl Drop for MemoryNode {
    fn drop(&mut self) {
        release_inode(&self.fs, &self.inode, 0);
    }
}
