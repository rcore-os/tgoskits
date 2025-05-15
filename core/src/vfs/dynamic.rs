use core::{any::Any, cmp::Ordering, time::Duration};

use alloc::{
    borrow::{Cow, ToOwned},
    collections::btree_map::BTreeMap,
    string::String,
    sync::Arc,
    vec::Vec,
};
use axfs_ng_vfs::{
    DirEntry, DirEntrySink, DirNode, DirNodeOps, FileNode, FileNodeOps, Filesystem, FilesystemOps,
    Metadata, MetadataUpdate, NodeOps, NodePermission, NodeType, Reference, StatFs, VfsError,
    VfsResult, WeakDirEntry, path::MAX_NAME_LEN,
};
use axsync::{Mutex, RawMutex};
use slab::Slab;

pub fn dummy_stat_fs(fs_type: u32) -> StatFs {
    StatFs {
        fs_type,
        block_size: 512,
        blocks: 100,
        blocks_free: 100,
        blocks_available: 100,

        file_count: 0,
        free_file_count: 0,

        name_length: MAX_NAME_LEN as _,
        fragment_size: 0,
        mount_flags: 0,
    }
}

pub struct DynamicFs {
    name: String,
    fs_type: u32,
    inodes: Mutex<Slab<()>>,
    root: Mutex<Option<DirEntry<RawMutex>>>,
}
impl DynamicFs {
    pub fn new(name: String,fs_type:u32, root: Arc<dyn DynamicDirOps>) -> Filesystem<RawMutex> {
        let fs = Arc::new(Self {
            name,
            fs_type,
            inodes: Mutex::default(),
            root: Mutex::default(),
        });
        let root = DirEntry::new_dir(
            |this| {
                DirNode::new(DynamicNode::new(
                    fs.clone(),
                    NodeType::Directory,
                    Some(this),
                    DynamicContent::Dir(root),
                ))
            },
            Reference::root(),
        );
        *fs.root.lock() = Some(root.clone());
        Filesystem::new(fs)
    }

    pub fn alloc_inode(&self) -> u64 {
        self.inodes.lock().insert(()) as u64 + 1
    }
    pub fn release_inode(&self, ino: u64) {
        self.inodes.lock().remove(ino as usize - 1);
    }
}
impl FilesystemOps<RawMutex> for DynamicFs {
    fn name(&self) -> &str {
        &self.name
    }

    fn root_dir(&self) -> DirEntry<RawMutex> {
        self.root.lock().clone().unwrap()
    }

    fn stat(&self) -> VfsResult<StatFs> {
        Ok(dummy_stat_fs(self.fs_type))
    }
}

pub trait DynamicFileOps: Send + Sync {
    fn read_all(&self) -> VfsResult<Cow<[u8]>>;
    fn write_all(&self, data: &[u8]) -> VfsResult<()>;
}
pub trait DynamicDirOps: Send + Sync {
    fn list_children(&self) -> Vec<Cow<str>>;
    fn get_child(&self, name: &str) -> VfsResult<(DynamicContent, NodeType)>;
}

impl<F, R> DynamicFileOps for F
where
    F: Fn() -> R + Send + Sync + 'static,
    R: Into<Vec<u8>>,
{
    fn read_all(&self) -> VfsResult<Cow<[u8]>> {
        Ok(Cow::Owned((self)().into()))
    }

    fn write_all(&self, _data: &[u8]) -> VfsResult<()> {
        Err(VfsError::EBADF)
    }
}

pub struct DynamicDir {
    children: BTreeMap<String, (DynamicContent, NodeType)>,
}
impl DynamicDir {
    pub fn new() -> Self {
        Self {
            children: BTreeMap::new(),
        }
    }

    pub fn add_file(&mut self, name: impl Into<String>, file: Arc<dyn DynamicFileOps>) {
        self.add_file_with_type(name, file, NodeType::RegularFile);
    }
    pub fn add_dir(&mut self, name: impl Into<String>, dir: Arc<dyn DynamicDirOps>) {
        self.children
            .insert(name.into(), (dir.into(), NodeType::Directory));
    }
    pub fn add_file_with_type(
        &mut self,
        name: impl Into<String>,
        file: Arc<dyn DynamicFileOps>,
        node_type: NodeType,
    ) {
        self.children.insert(name.into(), (file.into(), node_type));
    }
}
impl DynamicDirOps for DynamicDir {
    fn list_children(&self) -> Vec<Cow<str>> {
        self.children.keys().map(|it| it.into()).collect()
    }

    fn get_child(&self, name: &str) -> VfsResult<(DynamicContent, NodeType)> {
        self.children.get(name).cloned().ok_or(VfsError::ENOENT)
    }
}

#[derive(Clone)]
pub enum DynamicContent {
    File(Arc<dyn DynamicFileOps>),
    Dir(Arc<dyn DynamicDirOps>),
}
impl From<Arc<dyn DynamicFileOps>> for DynamicContent {
    fn from(file: Arc<dyn DynamicFileOps>) -> Self {
        Self::File(file)
    }
}
impl From<Arc<dyn DynamicDirOps>> for DynamicContent {
    fn from(dir: Arc<dyn DynamicDirOps>) -> Self {
        Self::Dir(dir)
    }
}

pub struct DynamicNode {
    fs: Arc<DynamicFs>,
    ino: u64,
    metadata: Mutex<Metadata>,
    this: Option<WeakDirEntry<RawMutex>>,
    content: DynamicContent,
}
impl DynamicNode {
    pub fn new(
        fs: Arc<DynamicFs>,
        node_type: NodeType,
        this: Option<WeakDirEntry<RawMutex>>,
        content: DynamicContent,
    ) -> Arc<Self> {
        let ino = fs.alloc_inode();
        let metadata = Metadata {
            device: 0,
            inode: ino,
            nlink: 1,
            mode: if node_type == NodeType::Directory {
                NodePermission::from_bits_truncate(0o755)
            } else {
                NodePermission::from_bits_truncate(0o644)
            },
            node_type,
            uid: 0,
            gid: 0,
            size: 0,
            block_size: 0,
            blocks: 0,
            atime: Duration::default(),
            mtime: Duration::default(),
            ctime: Duration::default(),
        };
        Arc::new(Self {
            fs,
            ino,
            metadata: Mutex::new(metadata),
            this,
            content,
        })
    }

    fn as_file(&self) -> VfsResult<&Arc<dyn DynamicFileOps>> {
        match &self.content {
            DynamicContent::File(file) => Ok(file),
            DynamicContent::Dir(_) => Err(VfsError::EISDIR),
        }
    }

    fn as_dir(&self) -> VfsResult<&Arc<dyn DynamicDirOps>> {
        match &self.content {
            DynamicContent::File(_) => Err(VfsError::ENOTDIR),
            DynamicContent::Dir(dir) => Ok(dir),
        }
    }
}
impl Drop for DynamicNode {
    fn drop(&mut self) {
        self.fs.release_inode(self.ino);
    }
}

impl NodeOps<RawMutex> for DynamicNode {
    fn inode(&self) -> u64 {
        self.ino
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        let mut metadata = self.metadata.lock().clone();
        metadata.size = self.len()?;
        Ok(metadata)
    }

    fn len(&self) -> VfsResult<u64> {
        Ok(match &self.content {
            DynamicContent::File(file) => file.read_all()?.len() as u64,
            DynamicContent::Dir(_) => 0,
        })
    }

    fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()> {
        let mut metadata = self.metadata.lock();
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

impl FileNodeOps<RawMutex> for DynamicNode {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let data = self.as_file()?.read_all()?;
        if offset >= data.len() as u64 {
            return Ok(0);
        }
        let data = &data[offset as usize..];
        let read = data.len().min(buf.len());
        buf[..read].copy_from_slice(&data[..read]);
        Ok(read)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize> {
        let file = self.as_file()?;
        let data = file.read_all()?;
        if offset == 0 && buf.len() >= data.len() {
            file.write_all(buf)?;
            return Ok(buf.len());
        }
        let mut data = data.to_vec();
        let end_pos = offset + buf.len() as u64;
        if end_pos > data.len() as u64 {
            data.resize(end_pos as usize, 0);
        }
        data[offset as usize..].copy_from_slice(buf);
        Ok(buf.len())
    }

    fn append(&self, buf: &[u8]) -> VfsResult<(usize, u64)> {
        let file = self.as_file()?;
        let mut data = file.read_all()?.to_vec();
        data.extend_from_slice(buf);
        file.write_all(&data)?;
        Ok((buf.len(), data.len() as u64))
    }

    fn set_len(&self, len: u64) -> VfsResult<()> {
        let file = self.as_file()?;
        let data = file.read_all()?;
        match len.cmp(&(data.len() as u64)) {
            Ordering::Less => file.write_all(&data[..len as usize]),
            Ordering::Greater => {
                let mut data = data.to_vec();
                data.resize(len as usize, 0);
                file.write_all(&data)
            }
            _ => Ok(()),
        }
    }

    fn set_symlink(&self, target: &str) -> VfsResult<()> {
        self.as_file()?.write_all(target.as_bytes())
    }
}

impl DirNodeOps<RawMutex> for DynamicNode {
    fn read_dir(&self, offset: u64, sink: &mut dyn DirEntrySink) -> VfsResult<usize> {
        let dir = self.as_dir()?;
        let children = dir.list_children();
        let children = [".", ".."]
            .into_iter()
            .chain(children.iter().map(|it| it.as_ref()));

        let this_entry = self.this.as_ref().unwrap().upgrade().unwrap();
        let this_dir = this_entry.as_dir()?;

        let mut count = 0;
        for (i, name) in children.enumerate().skip(offset as usize) {
            let entry = this_dir.lookup(name)?.downcast::<Self>()?;
            if !sink.accept(
                name,
                entry.ino,
                entry.metadata.lock().node_type,
                i as u64 + 1,
            ) {
                break;
            }
            count += 1;
        }

        Ok(count)
    }

    fn lookup(&self, name: &str) -> VfsResult<DirEntry<RawMutex>> {
        let dir = self.as_dir()?;
        let (content, node_type) = dir.get_child(name)?;
        let reference = Reference::new(
            self.this.as_ref().and_then(WeakDirEntry::upgrade),
            name.to_owned(),
        );
        Ok(match &content {
            DynamicContent::Dir(_) => DirEntry::new_dir(
                |this| {
                    DirNode::new(DynamicNode::new(
                        self.fs.clone(),
                        node_type,
                        Some(this),
                        content,
                    ))
                },
                reference,
            ),
            DynamicContent::File(_) => DirEntry::new_file(
                FileNode::new(DynamicNode::new(self.fs.clone(), node_type, None, content)),
                node_type,
                reference,
            ),
        })
    }

    fn create(
        &self,
        _name: &str,
        _node_type: NodeType,
        _permission: NodePermission,
    ) -> VfsResult<DirEntry<RawMutex>> {
        Err(VfsError::EPERM)
    }

    fn link(&self, _name: &str, _node: &DirEntry<RawMutex>) -> VfsResult<DirEntry<RawMutex>> {
        Err(VfsError::EPERM)
    }

    fn unlink(&self, _name: &str) -> VfsResult<()> {
        Err(VfsError::EPERM)
    }

    fn rename(
        &self,
        _src_name: &str,
        _dst_dir: &DirNode<RawMutex>,
        _dst_name: &str,
    ) -> VfsResult<()> {
        Err(VfsError::EPERM)
    }
}
