use alloc::{
    borrow::ToOwned,
    collections::BTreeMap,
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    any::Any,
    borrow::Borrow,
    cmp::Ordering,
    sync::atomic::{AtomicU64, Ordering as AtomicOrdering},
    task::Context,
    time::Duration,
};

use axfs_ng_vfs::{
    DeviceId, DirEntry, DirEntrySink, DirNode, DirNodeOps, DirectoryCursor, FileNode, FileNodeOps,
    FileRangeOperation, Filesystem, FilesystemOps, FsIoEvents, FsPollable, Metadata,
    MetadataUpdate, NodeFlags, NodeOps, NodePermission, NodeType, PreallocationMode, Reference,
    RenameOptions, StatFs, VfsError, VfsResult, WeakDirEntry, XattrOps, XattrSetMode,
};
use axpoll::{IoEvents, Pollable};
use hashbrown::HashMap;
use slab::Slab;

use crate::sync::{FsMutex, IrqMutex, Mutex};

const TMPFS_MAGIC: u32 = 0x0102_1994;
const RAMFS_MAGIC: u32 = 0x8584_58f6;
const STATFS_BLOCK_SIZE: u64 = 4096;
const DEFAULT_TMPFS_SIZE: u64 = 4 * 1024 * 1024 * 1024;

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
    name: &'static str,
    fs_type: u32,
    size_limit: Option<u64>,
    used_bytes: AtomicU64,
    // Inodes may be released from atomic cleanup paths, so the slab and
    // metadata locks must not sleep.
    inodes: IrqMutex<Slab<Arc<Inode>>>,
    // root_dir() is used while mounting pseudofs during early startup, before
    // Starry has reached a sleepable task context.
    root: IrqMutex<Option<DirEntry>>,
}

impl MemoryFs {
    /// Creates a new empty memory filesystem.
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> Filesystem {
        let (fs, handle) = Self::new_with_handle_and_limit(None);
        drop(handle);
        fs
    }

    /// Creates an empty tmpfs instance with a logical size limit.
    pub fn new_with_size_limit(size_limit: u64) -> Filesystem {
        let (fs, handle) = Self::new_with_handle_and_limit(Some(size_limit));
        drop(handle);
        fs
    }

    /// Creates an empty ramfs instance with the Linux-visible ramfs identity.
    pub fn new_ramfs() -> Filesystem {
        let (fs, handle) = Self::new_named_with_handle("ramfs", RAMFS_MAGIC, None);
        drop(handle);
        fs
    }

    /// Creates a new empty memory filesystem and returns a handle to the
    /// underlying `MemoryFs` so callers can create anonymous (unlinked) nodes.
    pub fn new_with_handle() -> (Filesystem, Arc<Self>) {
        Self::new_with_handle_and_limit(None)
    }

    fn new_with_handle_and_limit(size_limit: Option<u64>) -> (Filesystem, Arc<Self>) {
        Self::new_named_with_handle("tmpfs", TMPFS_MAGIC, size_limit)
    }

    fn new_named_with_handle(
        name: &'static str,
        fs_type: u32,
        size_limit: Option<u64>,
    ) -> (Filesystem, Arc<Self>) {
        let handle = Arc::new(Self {
            name,
            fs_type,
            size_limit,
            used_bytes: AtomicU64::new(0),
            inodes: IrqMutex::new(Slab::new()),
            root: IrqMutex::new(None),
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

    fn resize_usage(&self, old_len: u64, new_len: u64) -> VfsResult<()> {
        if new_len <= old_len {
            self.used_bytes
                .fetch_sub(old_len - new_len, AtomicOrdering::AcqRel);
            return Ok(());
        }

        let growth = new_len - old_len;
        let mut used = self.used_bytes.load(AtomicOrdering::Acquire);
        loop {
            let new_used = used.checked_add(growth).ok_or(VfsError::StorageFull)?;
            if self.size_limit.is_some_and(|limit| new_used > limit) {
                return Err(VfsError::StorageFull);
            }
            match self.used_bytes.compare_exchange_weak(
                used,
                new_used,
                AtomicOrdering::AcqRel,
                AtomicOrdering::Acquire,
            ) {
                Ok(_) => return Ok(()),
                Err(observed) => used = observed,
            }
        }
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
        self.name
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
        let size_limit = self.size_limit.unwrap_or(DEFAULT_TMPFS_SIZE);
        let blocks = size_limit.div_ceil(STATFS_BLOCK_SIZE);
        let used_blocks = self
            .used_bytes
            .load(AtomicOrdering::Acquire)
            .div_ceil(STATFS_BLOCK_SIZE);
        Ok(StatFs {
            fs_type: self.fs_type,
            block_size: STATFS_BLOCK_SIZE as u32,
            blocks,
            blocks_free: blocks.saturating_sub(used_blocks),
            blocks_available: blocks.saturating_sub(used_blocks),
            file_count: 0,
            free_file_count: 1 << 16,
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
    length: AtomicU64,
    symlink: Mutex<Option<String>>,
}

struct DirContent {
    // VFS dentry-cache operations call tmpfs directory ops while holding
    // IrqMutex guards, so this per-directory map must not use a blocking
    // mutex.
    entries: IrqMutex<HashMap<FileName, InodeRef>>,
    next_cookie: AtomicU64,
}

impl Default for DirContent {
    fn default() -> Self {
        Self {
            entries: IrqMutex::new(HashMap::new()),
            next_cookie: AtomicU64::new(3),
        }
    }
}

enum NodeContent {
    File(FileContent),
    Dir(DirContent),
}

struct Inode {
    fs: Weak<MemoryFs>,
    ino: u64,
    metadata: IrqMutex<Metadata>,
    content: NodeContent,
    // Extended attributes belong to the inode so hard links observe the same
    // values. Syscall xattr paths are sleepable and never hold a directory
    // entries guard while acquiring this lock.
    xattrs: FsMutex<BTreeMap<Vec<u8>, Vec<u8>>>,
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
            fs: Arc::downgrade(fs),
            ino,
            metadata: IrqMutex::new(metadata),
            content,
            xattrs: FsMutex::new(BTreeMap::new()),
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

impl Drop for Inode {
    fn drop(&mut self) {
        let NodeContent::File(content) = &self.content else {
            return;
        };
        let Some(fs) = self.fs.upgrade() else {
            return;
        };
        let length = content.length.load(AtomicOrdering::Acquire);
        fs.used_bytes.fetch_sub(length, AtomicOrdering::AcqRel);
    }
}

fn xattr_not_found() -> VfsError {
    VfsError::DataMissing
}

impl XattrOps for Inode {
    fn get_xattr(&self, name: &[u8]) -> VfsResult<Vec<u8>> {
        self.xattrs
            .lock()
            .get(name)
            .cloned()
            .ok_or_else(xattr_not_found)
    }

    fn list_xattrs(&self) -> VfsResult<Vec<Vec<u8>>> {
        Ok(self.xattrs.lock().keys().cloned().collect())
    }

    fn set_xattr(&self, name: &[u8], value: &[u8], mode: XattrSetMode) -> VfsResult<()> {
        if name.is_empty() {
            return Err(VfsError::InvalidInput);
        }

        let mut xattrs = self.xattrs.lock();
        let exists = xattrs.contains_key(name);
        match mode {
            XattrSetMode::Create if exists => return Err(VfsError::AlreadyExists),
            XattrSetMode::Replace if !exists => return Err(xattr_not_found()),
            XattrSetMode::Upsert | XattrSetMode::Create | XattrSetMode::Replace => {}
        }
        xattrs.insert(name.to_vec(), value.to_vec());
        Ok(())
    }

    fn remove_xattr(&self, name: &[u8]) -> VfsResult<()> {
        self.xattrs
            .lock()
            .remove(name)
            .map(|_| ())
            .ok_or_else(xattr_not_found)
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
                metadata.size = content.length.load(AtomicOrdering::Acquire);
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

    fn xattr_ops(&self) -> Option<&dyn XattrOps> {
        Some(self.inode.as_ref())
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
        let file = self.inode.as_file()?;
        let old_len = file.length.load(AtomicOrdering::Acquire);
        self.fs.resize_usage(old_len, len)?;
        file.length.store(len, AtomicOrdering::Release);
        Ok(())
    }

    fn operate_range(&self, offset: u64, len: u64, operation: FileRangeOperation) -> VfsResult<()> {
        let end = offset.checked_add(len).ok_or(VfsError::InvalidInput)?;
        match operation {
            FileRangeOperation::Allocate(PreallocationMode::KeepSize) => {
                Err(VfsError::OperationNotSupported)
            }
            FileRangeOperation::Allocate(PreallocationMode::ExtendSize)
            | FileRangeOperation::ZeroRange(PreallocationMode::ExtendSize) => {
                let current = self.inode.as_file()?.length.load(AtomicOrdering::Acquire);
                self.set_len(current.max(end))
            }
            FileRangeOperation::PunchHole
            | FileRangeOperation::ZeroRange(PreallocationMode::KeepSize) => Ok(()),
            FileRangeOperation::CollapseRange | FileRangeOperation::InsertRange => {
                Err(VfsError::OperationNotSupported)
            }
        }
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
    fn read_dir(&self, cursor: DirectoryCursor, sink: &mut dyn DirEntrySink) -> VfsResult<usize> {
        let offset = cursor.offset();
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
            if !sink.accept(
                name.as_bytes(),
                ino,
                node_type,
                DirectoryCursor::new(cookie + 1),
            ) {
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
        if node_type == NodeType::Symlink {
            return Err(VfsError::InvalidInput);
        }
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

    fn create_symlink(
        &self,
        name: &str,
        target: &str,
        permission: NodePermission,
        uid: u32,
        gid: u32,
    ) -> VfsResult<DirEntry> {
        let target = target.to_owned();
        let target_len = target.len() as u64;
        let dir = self.inode.as_dir()?;
        {
            let entries = dir.entries.lock();
            if entries.contains_key(name) {
                return Err(VfsError::AlreadyExists);
            }
        }
        self.fs.resize_usage(0, target_len)?;
        let inode = Inode::new(
            &self.fs,
            Some(self.inode.ino),
            NodeType::Symlink,
            permission,
            uid,
            gid,
            TMPFS_NESTED_DIR_ENTRIES_SUBCLASS,
        );
        let NodeContent::File(file) = &inode.content else {
            self.fs
                .used_bytes
                .fetch_sub(target_len, AtomicOrdering::AcqRel);
            drop(self.fs.inodes.lock().remove(inode.ino as usize - 1));
            return Err(VfsError::InvalidData);
        };
        file.length.store(target_len, AtomicOrdering::Release);
        // The symlink payload uses a sleepable mutex. Initialize it before taking the
        // IRQ-safe directory lock so no blocking lock is acquired in atomic context.
        *file.symlink.lock() = Some(target);

        let entry = DirEntry::new_file(
            FileNode::new(MemoryNode::new(self.fs.clone(), inode.clone(), None)),
            NodeType::Symlink,
            Reference::new(
                self.this.as_ref().and_then(WeakDirEntry::upgrade),
                name.to_owned(),
            ),
        );
        let mut entries = dir.entries.lock();
        if entries.contains_key(name) {
            drop(entries);
            drop(entry);
            self.fs
                .used_bytes
                .fetch_sub(target_len, AtomicOrdering::AcqRel);
            release_inode(&self.fs, &inode, 0);
            return Err(VfsError::AlreadyExists);
        }
        let cookie = dir.next_cookie.fetch_add(1, AtomicOrdering::Relaxed);
        entries.insert(
            name.into(),
            InodeRef::new(self.fs.clone(), inode.ino, NodeType::Symlink, cookie),
        );
        Ok(entry)
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
    fn rename(
        &self,
        src_name: &str,
        dst_dir: &DirNode,
        dst_name: &str,
        options: RenameOptions,
    ) -> VfsResult<()> {
        if options.exchange() || options.whiteout() {
            return Err(VfsError::OperationNotSupported);
        }
        let dst_node = dst_dir.downcast::<Self>()?;
        if let Ok(entry) = dst_dir.lookup(dst_name) {
            let src_entry = self.lookup(src_name)?;
            if options.no_replace() {
                return Err(VfsError::AlreadyExists);
            }
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

#[cfg(all(test, axtest))]
fn failed_symlink_capacity_reservation_does_not_publish_name_for_test() -> bool {
    let filesystem = MemoryFs::new_with_size_limit(3);
    let root = filesystem.root_dir();
    let Ok(directory) = root.as_dir() else {
        return false;
    };

    let capacity_failure_is_atomic =
        matches!(
            directory.create_symlink(
                "link",
                "four",
                NodePermission::from_bits_truncate(0o777),
                1000,
                1001,
            ),
            Err(VfsError::StorageFull)
        ) && matches!(directory.lookup("link"), Err(VfsError::NotFound));

    let filesystem = MemoryFs::new_with_size_limit(64);
    let root = filesystem.root_dir();
    let Ok(directory) = root.as_dir() else {
        return false;
    };
    let successful_create_is_readable = directory
        .create_symlink(
            "link",
            "target",
            NodePermission::from_bits_truncate(0o777),
            1000,
            1001,
        )
        .and_then(|entry| entry.read_link())
        .is_ok_and(|target| target == "target");

    capacity_failure_is_atomic && successful_create_is_readable
}

#[cfg(test)]
mod tests {
    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn failed_symlink_capacity_reservation_does_not_publish_name() {
        assert!(super::failed_symlink_capacity_reservation_does_not_publish_name_for_test());
    }
}
