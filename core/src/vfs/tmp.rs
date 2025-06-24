use core::{any::Any, borrow::Borrow, cmp::Ordering, time::Duration};

use alloc::{
    borrow::ToOwned, collections::btree_map::BTreeMap, string::String, sync::Arc, vec::Vec,
};
use axfs_ng_vfs::{
    DeviceId, DirEntry, DirEntrySink, DirNode, DirNodeOps, FileNode, FileNodeOps, Filesystem,
    FilesystemOps, Metadata, MetadataUpdate, NodeOps, NodePermission, NodeType, Reference, StatFs,
    VfsError, VfsResult, WeakDirEntry,
};
use axsync::{Mutex, RawMutex};
use slab::Slab;

use super::dynamic::dummy_stat_fs;

#[derive(PartialEq, Eq, Clone)]
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

pub struct MemoryFs {
    inodes: Mutex<Slab<Arc<Inode>>>,
    root: Mutex<Option<DirEntry<RawMutex>>>,
}
impl MemoryFs {
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

#[derive(Default)]
struct FileContent {
    content: Mutex<Vec<u8>>,
}
#[derive(Default)]
struct DirContent {
    entries: Mutex<BTreeMap<FileName, InodeRef>>,
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
                metadata.size = content.content.lock().len() as u64;
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
        let content = self.inode.as_file()?.content.lock();
        if offset >= content.len() as u64 {
            return Ok(0);
        }
        let content = &content[offset as usize..];
        let read = buf.len().min(content.len());
        buf[..read].copy_from_slice(&content[..read]);
        Ok(read)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize> {
        let mut content = self.inode.as_file()?.content.lock();
        let end_pos = offset as usize + buf.len();
        if end_pos > content.len() {
            content.resize(end_pos, 0);
        }
        let content = &mut content[offset as usize..];
        let write = content.len().min(buf.len());
        content[..write].copy_from_slice(&buf[..write]);
        Ok(write)
    }

    fn append(&self, buf: &[u8]) -> VfsResult<(usize, u64)> {
        let mut content = self.inode.as_file()?.content.lock();
        content.extend_from_slice(buf);
        Ok((buf.len(), buf.len() as u64))
    }

    fn set_len(&self, len: u64) -> VfsResult<()> {
        let mut content = self.inode.as_file()?.content.lock();
        if len > content.len() as u64 {
            content.resize(len as usize, 0);
        } else {
            content.truncate(len as usize);
        }
        Ok(())
    }

    fn set_symlink(&self, target: &str) -> VfsResult<()> {
        *self.inode.as_file()?.content.lock() = target.as_bytes().to_vec();
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
        release_inode(&self.fs, &self.inode, 0);
    }
}
