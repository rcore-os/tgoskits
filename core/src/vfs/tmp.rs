use alloc::{
    borrow::ToOwned, collections::btree_map::BTreeMap, string::String, sync::Arc, vec::Vec,
};
use hashbrown::HashMap;
use core::{any::Any, borrow::Borrow, cmp::Ordering, time::Duration};

use axfs_ng_vfs::{
    DeviceId, DirEntry, DirEntrySink, DirNode, DirNodeOps, FileNode, FileNodeOps, Filesystem,
    FilesystemOps, Metadata, MetadataUpdate, NodeOps, NodePermission, NodeType, Reference, StatFs,
    VfsError, VfsResult, WeakDirEntry,
};
use axsync::{Mutex, RawMutex};
use slab::Slab;

use crate::vfs::simple::dummy_stat_fs;

#[derive(PartialEq, Eq, Hash, Clone)]
struct FileName(String);

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
        (index(&self.0), &self.0).cmp(&(index(&other.0), &other.0))
    }
}

impl<T> From<T> for FileName
where
    T: Into<String>,
{
    fn from(name: T) -> Self {
        Self(name.into())
    }
}

impl Borrow<str> for FileName {
    fn borrow(&self) -> &str {
        &self.0
    }
}

/// A simple in-memory filesystem that supports basic file operations.
pub struct MemoryFs {
    inodes: Mutex<Slab<Arc<Inode>>>,
    root: Mutex<Option<DirEntry<RawMutex>>>,
}

impl MemoryFs {
    /// Creates a new empty memory filesystem.
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> Filesystem<RawMutex> {
        let fs = Arc::new(Self {
            inodes: Mutex::new(Slab::new()),
            root: Mutex::default(),
        });
        let root_ino = Inode::new(
            &fs,
            None,
            NodeType::Directory,
            NodePermission::from_bits_truncate(0o755),
        );
        *fs.root.lock() = Some(DirEntry::new_dir(
            |this| DirNode::new(MemoryNode::new(fs.clone(), root_ino, Some(this))),
            Reference::root(),
        ));
        Filesystem::new(fs)
    }

    fn get(&self, ino: u64) -> Arc<Inode> {
        self.inodes.lock()[ino as usize - 1].clone()
    }
}

impl FilesystemOps<RawMutex> for MemoryFs {
    fn name(&self) -> &str {
        "tmpfs"
    }

    fn root_dir(&self) -> DirEntry<RawMutex> {
        self.root.lock().clone().unwrap()
    }

    fn stat(&self) -> VfsResult<StatFs> {
        Ok(dummy_stat_fs(0x01021994))
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

const SPARSE_CHUNK_SIZE: u64 = 4096;

#[derive(Default)]
struct SparseFile {
    chunks: BTreeMap<u64, [u8; SPARSE_CHUNK_SIZE as usize]>,
    length: u64,
}

impl SparseFile {
    fn len(&self) -> u64 {
        self.length
    }

    fn set_len(&mut self, len: u64) {
        if len >= self.length {
            self.length = len;
            return;
        }
        while self.chunks.last_key_value().is_some_and(|it| *it.0 >= len) {
            self.chunks.pop_last();
        }
        self.length = len;
    }

    fn write_at(&mut self, mut data: &[u8], offset: u64) {
        let end = offset + data.len() as u64;
        self.length = self.length.max(end);

        let mut ptr = offset / SPARSE_CHUNK_SIZE * SPARSE_CHUNK_SIZE;
        let mut offset = (offset % SPARSE_CHUNK_SIZE) as usize;
        let mut iter = self.chunks.range_mut(ptr..);
        while !data.is_empty() {
            let (chunk_start, chunk) = if let Some((chunk_start, chunk)) = iter.next()
                && *chunk_start == ptr
            {
                (chunk_start, chunk)
            } else {
                self.chunks.insert(ptr, [0; SPARSE_CHUNK_SIZE as usize]);
                iter = self.chunks.range_mut(ptr..);
                iter.next().unwrap()
            };
            let end = (end - chunk_start).min(SPARSE_CHUNK_SIZE) as usize;
            let (write, rest) = data.split_at(end - offset);
            data = rest;
            chunk[offset..end].copy_from_slice(write);
            offset = 0;
            ptr += SPARSE_CHUNK_SIZE;
        }
    }

    fn read_at(&self, mut buf: &mut [u8], mut offset: u64) -> usize {
        if offset >= self.length {
            return 0;
        }
        let end = (offset + buf.len() as u64).min(self.length);
        buf = &mut buf[..(end - offset) as usize];
        if buf.is_empty() {
            return 0;
        }
        let read_len = buf.len();

        let mut iter = self
            .chunks
            .range((offset / SPARSE_CHUNK_SIZE * SPARSE_CHUNK_SIZE)..);

        while !buf.is_empty() {
            let Some((chunk_start, chunk)) = iter.next() else {
                buf.fill(0);
                break;
            };
            if *chunk_start > offset {
                let gap_size = (*chunk_start - offset) as usize;
                let gap_size = gap_size.min(buf.len());
                let (zero, rest) = buf.split_at_mut(gap_size);
                zero.fill(0);
                buf = rest;
                offset = *chunk_start;
                if buf.is_empty() {
                    break;
                }
            }

            // Calculate the offset within the chunk
            let chunk_offset = (offset - *chunk_start) as usize;
            let available_in_chunk = SPARSE_CHUNK_SIZE as usize - chunk_offset;
            let to_read = buf.len().min(available_in_chunk);

            let (read, rest) = buf.split_at_mut(to_read);
            read.copy_from_slice(&chunk[chunk_offset..chunk_offset + to_read]);
            buf = rest;
            offset += to_read as u64;
        }

        read_len
    }
}

#[derive(Default)]
struct DenseFile {
    content: Vec<u8>,
}

impl DenseFile {
    fn len(&self) -> u64 {
        self.content.len() as u64
    }

    fn set_len(&mut self, len: u64) {
        if len > self.content.len() as u64 {
            self.content.resize(len as usize, 0);
        } else {
            self.content.truncate(len as usize);
        }
    }

    fn write_at(&mut self, data: &[u8], offset: u64) {
        let end = offset + data.len() as u64;
        if end > self.content.len() as u64 {
            self.content.resize(end as usize, 0);
        }
        self.content[offset as usize..end as usize].copy_from_slice(data);
    }

    fn read_at(&self, buf: &mut [u8], offset: u64) -> usize {
        let end = (offset + buf.len() as u64).min(self.content.len() as u64);
        if end <= offset || buf.is_empty() {
            return 0;
        }
        let read_len = (end - offset) as usize;
        buf[..read_len].copy_from_slice(&self.content[offset as usize..end as usize]);
        read_len
    }
}

enum DynamicFile {
    Dense(DenseFile),
    Sparse(SparseFile),
}

impl Default for DynamicFile {
    fn default() -> Self {
        Self::Dense(DenseFile::default())
    }
}

impl DynamicFile {
    fn len(&self) -> u64 {
        match self {
            DynamicFile::Sparse(file) => file.len(),
            DynamicFile::Dense(file) => file.len(),
        }
    }

    fn turn_to_sparse(dense: &mut DenseFile, pos: u64) -> Option<SparseFile> {
        if pos >= dense.len() + 4 * 1024 * 1024 {
            let mut sparse = SparseFile::default();
            sparse.write_at(&dense.content, 0);
            Some(sparse)
        } else {
            None
        }
    }

    fn set_len(&mut self, len: u64) {
        match self {
            DynamicFile::Sparse(file) => file.set_len(len),
            DynamicFile::Dense(file) => {
                if let Some(mut sparse) = Self::turn_to_sparse(file, len) {
                    sparse.set_len(len);
                    *self = Self::Sparse(sparse);
                } else {
                    file.set_len(len);
                }
            }
        }
    }

    fn write_at(&mut self, data: &[u8], offset: u64) {
        match self {
            DynamicFile::Sparse(file) => file.write_at(data, offset),
            DynamicFile::Dense(file) => {
                if let Some(mut sparse) = Self::turn_to_sparse(file, offset) {
                    sparse.write_at(data, offset);
                    *self = Self::Sparse(sparse);
                } else {
                    file.write_at(data, offset);
                }
            }
        }
    }

    fn read_at(&self, buf: &mut [u8], offset: u64) -> usize {
        match self {
            DynamicFile::Sparse(file) => file.read_at(buf, offset),
            DynamicFile::Dense(file) => file.read_at(buf, offset),
        }
    }
}

#[derive(Default)]
struct FileContent {
    content: Mutex<DynamicFile>,
}

#[derive(Default)]
struct DirContent {
    entries: Mutex<HashMap<FileName, InodeRef>>,
}

enum NodeContent {
    File(FileContent),
    Dir(DirContent),
}

struct Inode {
    ino: u64,
    metadata: Mutex<Metadata>,
    content: NodeContent,
}

impl Inode {
    pub fn new(
        fs: &Arc<MemoryFs>,
        parent: Option<u64>,
        node_type: NodeType,
        permission: NodePermission,
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
            uid: 0,
            gid: 0,
            size: 0,
            block_size: 0,
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
            metadata: Mutex::new(metadata),
            content,
        });
        entry.insert(result.clone());
        drop(inodes);
        if let NodeContent::Dir(dir) = &result.content {
            let mut entries = dir.entries.lock();
            entries.insert(".".into(), InodeRef::new(fs.clone(), ino));
            entries.insert(
                "..".into(),
                InodeRef::new(fs.clone(), parent.unwrap_or(ino)),
            );
        }
        result
    }

    fn as_file(&self) -> VfsResult<&FileContent> {
        match self.content {
            NodeContent::File(ref content) => Ok(content),
            _ => Err(VfsError::EISDIR),
        }
    }

    fn as_dir(&self) -> VfsResult<&DirContent> {
        match self.content {
            NodeContent::Dir(ref content) => Ok(content),
            _ => Err(VfsError::ENOTDIR),
        }
    }
}

struct InodeRef {
    fs: Arc<MemoryFs>,
    ino: u64,
}

impl InodeRef {
    pub fn new(fs: Arc<MemoryFs>, ino: u64) -> Self {
        fs.get(ino).metadata.lock().nlink += 1;
        Self { fs, ino }
    }

    fn get(&self) -> Arc<Inode> {
        self.fs.get(self.ino)
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
    this: Option<WeakDirEntry<RawMutex>>,
}

impl MemoryNode {
    pub fn new(
        fs: Arc<MemoryFs>,
        inode: Arc<Inode>,
        this: Option<WeakDirEntry<RawMutex>>,
    ) -> Arc<Self> {
        Arc::new(Self { fs, inode, this })
    }

    fn new_entry(
        &self,
        name: &str,
        node_type: NodeType,
        inode: Arc<Inode>,
    ) -> VfsResult<DirEntry<RawMutex>> {
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
}

impl NodeOps<RawMutex> for MemoryNode {
    fn inode(&self) -> u64 {
        self.inode.ino
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        let mut metadata = self.inode.metadata.lock().clone();
        match &self.inode.content {
            NodeContent::File(content) => {
                metadata.size = content.content.lock().len();
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
        if let Some(atime) = update.atime {
            metadata.atime = atime;
        }
        if let Some(mtime) = update.mtime {
            metadata.mtime = mtime;
        }
        Ok(())
    }

    fn filesystem(&self) -> &dyn FilesystemOps<RawMutex> {
        self.fs.as_ref()
    }

    fn sync(&self, _data_only: bool) -> VfsResult<()> {
        Ok(())
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

impl FileNodeOps<RawMutex> for MemoryNode {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        Ok(self.inode.as_file()?.content.lock().read_at(buf, offset))
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize> {
        self.inode.as_file()?.content.lock().write_at(buf, offset);
        Ok(buf.len())
    }

    fn append(&self, buf: &[u8]) -> VfsResult<(usize, u64)> {
        let mut content = self.inode.as_file()?.content.lock();
        let length = content.len();
        content.write_at(buf, length);
        Ok((buf.len(), content.len()))
    }

    fn set_len(&self, len: u64) -> VfsResult<()> {
        self.inode.as_file()?.content.lock().set_len(len);
        Ok(())
    }

    fn set_symlink(&self, target: &str) -> VfsResult<()> {
        let mut content = self.inode.as_file()?.content.lock();
        content.set_len(target.len() as u64);
        content.write_at(target.as_bytes(), 0);
        Ok(())
    }
}

impl DirNodeOps<RawMutex> for MemoryNode {
    fn read_dir(&self, offset: u64, sink: &mut dyn DirEntrySink) -> VfsResult<usize> {
        let mut count = 0;
        for (i, (name, entry)) in self
            .inode
            .as_dir()?
            .entries
            .lock()
            .iter()
            .enumerate()
            .skip(offset as usize)
        {
            if !sink.accept(
                &name.0,
                entry.ino,
                entry.get().metadata.lock().node_type,
                i as u64 + 1,
            ) {
                return Ok(count);
            }
            count += 1;
        }
        Ok(count)
    }

    fn lookup(&self, name: &str) -> VfsResult<DirEntry<RawMutex>> {
        let dir = self.inode.as_dir()?;
        let entries = dir.entries.lock();

        let entry = entries.get(name).ok_or(VfsError::ENOENT)?;
        let inode = entry.get();
        let node_type = inode.metadata.lock().node_type;
        self.new_entry(name, node_type, inode)
    }

    fn create(
        &self,
        name: &str,
        node_type: NodeType,
        permission: NodePermission,
    ) -> VfsResult<DirEntry<RawMutex>> {
        let dir = self.inode.as_dir()?;
        let mut entries = dir.entries.lock();

        if entries.contains_key(name) {
            return Err(VfsError::EEXIST);
        }
        let inode = Inode::new(&self.fs, Some(self.inode.ino), node_type, permission);
        entries.insert(name.into(), InodeRef::new(self.fs.clone(), inode.ino));
        self.new_entry(name, node_type, inode)
    }

    fn link(&self, name: &str, target: &DirEntry<RawMutex>) -> VfsResult<DirEntry<RawMutex>> {
        let dir = self.inode.as_dir()?;
        let mut entries = dir.entries.lock();

        let target = target.downcast::<Self>()?;

        if entries.contains_key(name) {
            return Err(VfsError::EEXIST);
        }
        let inode = target.inode.clone();
        let node_type = target.metadata()?.node_type;
        entries.insert(name.into(), InodeRef::new(self.fs.clone(), inode.ino));
        self.new_entry(name, node_type, inode)
    }

    fn unlink(&self, name: &str) -> VfsResult<()> {
        let dir = self.inode.as_dir()?;
        let mut entries = dir.entries.lock();

        let Some(entry) = entries.get(name) else {
            return Err(VfsError::ENOENT);
        };
        if let NodeContent::Dir(DirContent { entries }) = &entry.get().content
            && entries.lock().len() > 2
        {
            return Err(VfsError::ENOTEMPTY);
        }
        entries.remove(name);

        Ok(())
    }

    // TODO: atomicity
    fn rename(&self, src_name: &str, dst_dir: &DirNode<RawMutex>, dst_name: &str) -> VfsResult<()> {
        let dst_node = dst_dir.downcast::<Self>()?;
        if let Ok(entry) = dst_dir.lookup(dst_name) {
            let src_entry = self.lookup(src_name)?;
            if entry.inode() == src_entry.inode() {
                return Ok(());
            }
        }

        let src_entry = self
            .inode
            .as_dir()?
            .entries
            .lock()
            .remove(src_name)
            .ok_or(VfsError::ENOENT)?;
        dst_node
            .inode
            .as_dir()?
            .entries
            .lock()
            .insert(dst_name.into(), src_entry);
        Ok(())
    }
}

impl Drop for MemoryNode {
    fn drop(&mut self) {
        if let NodeContent::Dir(dir) = &self.inode.content {
            dir.entries.lock().clear();
        }
        release_inode(&self.fs, &self.inode, 0);
    }
}
