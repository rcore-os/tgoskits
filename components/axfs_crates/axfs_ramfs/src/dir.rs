use alloc::{collections::BTreeMap, string::String, sync::Arc};
use core::{any::Any, ops::Deref, task::Context, time::Duration};

use ax_fs_vfs::{
    DeviceId, DirEntry, DirEntrySink, DirNode as VfsDirNode, DirNodeOps, FilesystemOps, Metadata,
    MetadataUpdate, NodeFlags, NodeOps, NodePermission, NodeType, Reference, VfsError, VfsResult,
    WeakDirEntry,
};
use ax_kspin::SpinNoIrq as Mutex;
use axpoll::{IoEvents, Pollable};

use crate::{RamFileSystem, file::FileNode};

pub(crate) struct RamDirNode {
    fs: Arc<RamFileSystem>,
    this: Mutex<WeakDirEntry>,
    inode: u64,
    mode: Mutex<NodePermission>,
    children: Mutex<BTreeMap<String, DirEntry>>,
}

impl RamDirNode {
    pub(crate) fn make(
        fs: Arc<RamFileSystem>,
        this: WeakDirEntry,
        inode: u64,
        mode: NodePermission,
    ) -> VfsDirNode {
        VfsDirNode::new(Arc::new(Self {
            fs,
            this: Mutex::new(this),
            inode,
            mode: Mutex::new(mode),
            children: Mutex::new(BTreeMap::new()),
        }))
    }

    fn make_entry(
        &self,
        name: &str,
        node_type: NodeType,
        mode: NodePermission,
    ) -> VfsResult<DirEntry> {
        let fs = self.fs.clone();
        let reference = Reference::new(self.this.lock().upgrade(), name.into());
        let inode = fs.alloc_inode();
        match node_type {
            NodeType::RegularFile | NodeType::Symlink => Ok(DirEntry::new_file(
                FileNode::make(fs, inode, mode, None),
                node_type,
                reference,
            )),
            NodeType::Directory => Ok(DirEntry::new_dir(
                |this| {
                    VfsDirNode::new(Arc::new(Self {
                        fs,
                        this: Mutex::new(this),
                        inode,
                        mode: Mutex::new(mode),
                        children: Mutex::new(BTreeMap::new()),
                    }))
                },
                reference,
            )),
            _ => Err(VfsError::Unsupported),
        }
    }

    fn rebind_entry(
        &self,
        entry: &DirEntry,
        parent: Option<DirEntry>,
        name: &str,
    ) -> VfsResult<DirEntry> {
        let reference = Reference::new(parent, name.into());
        if entry.is_file() {
            return Ok(DirEntry::new_file(
                ax_fs_vfs::FileNode::new(entry.as_file()?.inner().clone()),
                entry.node_type(),
                reference,
            ));
        }

        let old_dir = entry.as_dir()?.downcast::<Self>()?;
        let node = old_dir.clone();
        let rebound = DirEntry::new_dir_node_cyclic(
            |this| {
                *node.this.lock() = this;
                VfsDirNode::new(node)
            },
            reference,
        );
        Ok(rebound)
    }

    fn rebind_children_to(root: Arc<Self>, parent: &DirEntry) -> VfsResult<()> {
        let mut stack = alloc::vec![(root, parent.clone())];
        while let Some((dir, parent)) = stack.pop() {
            let mut children = dir.children.lock();
            for (name, child) in children.iter_mut() {
                *child = dir.rebind_entry(child, Some(parent.clone()), name)?;
                if child.is_dir() {
                    let child_dir = child.as_dir()?.downcast::<Self>()?;
                    stack.push((child_dir, child.clone()));
                }
            }
        }
        Ok(())
    }

    fn check_replace_target(children: &BTreeMap<String, DirEntry>, name: &str) -> VfsResult<()> {
        if let Some(existing) = children.get(name)
            && existing.is_dir()
            && existing.as_dir()?.has_children()?
        {
            return Err(VfsError::DirectoryNotEmpty);
        }
        Ok(())
    }
}

impl NodeOps for RamDirNode {
    fn inode(&self) -> u64 {
        self.inode
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        Ok(Metadata {
            device: 0,
            inode: self.inode,
            nlink: 1,
            mode: *self.mode.lock(),
            node_type: NodeType::Directory,
            uid: 0,
            gid: 0,
            size: 4096,
            block_size: 4096,
            blocks: 8,
            rdev: DeviceId::default(),
            atime: Duration::default(),
            mtime: Duration::default(),
            ctime: Duration::default(),
        })
    }

    fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()> {
        if let Some(mode) = update.mode {
            *self.mode.lock() = mode;
        }
        Ok(())
    }

    fn filesystem(&self) -> &dyn FilesystemOps {
        self.fs.deref()
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

impl DirNodeOps for RamDirNode {
    fn read_dir(&self, offset: u64, sink: &mut dyn DirEntrySink) -> VfsResult<usize> {
        let mut count = 0;
        let mut off = offset;

        if off == 0 {
            if !sink.accept(".", self.inode, NodeType::Directory, 1) {
                return Ok(0);
            }
            count += 1;
            off = 1;
        }
        if off == 1 {
            let parent_inode = self
                .this
                .lock()
                .upgrade()
                .and_then(|e| e.parent())
                .map_or(self.inode, |p| p.inode());
            if !sink.accept("..", parent_inode, NodeType::Directory, 2) {
                return Ok(count);
            }
            count += 1;
            off = 2;
        }
        for (index, (name, entry)) in self
            .children
            .lock()
            .iter()
            .enumerate()
            .skip((off - 2) as usize)
        {
            if !sink.accept(name, entry.inode(), entry.node_type(), (index + 3) as u64) {
                break;
            }
            count += 1;
        }
        Ok(count)
    }

    fn lookup(&self, name: &str) -> VfsResult<DirEntry> {
        self.children
            .lock()
            .get(name)
            .cloned()
            .ok_or(VfsError::NotFound)
    }

    fn create(
        &self,
        name: &str,
        node_type: NodeType,
        permission: NodePermission,
    ) -> VfsResult<DirEntry> {
        let mut children = self.children.lock();
        if children.contains_key(name) {
            return Err(VfsError::AlreadyExists);
        }
        let entry = self.make_entry(name, node_type, permission)?;
        children.insert(name.into(), entry.clone());
        Ok(entry)
    }

    fn link(&self, name: &str, node: &DirEntry) -> VfsResult<DirEntry> {
        if node.is_dir() {
            return Err(VfsError::OperationNotPermitted);
        }
        let mut children = self.children.lock();
        if children.contains_key(name) {
            return Err(VfsError::AlreadyExists);
        }
        let reference = Reference::new(self.this.lock().upgrade(), name.into());
        let entry = DirEntry::new_file(
            ax_fs_vfs::FileNode::new(node.as_file()?.inner().clone()),
            node.node_type(),
            reference,
        );
        children.insert(name.into(), entry.clone());
        Ok(entry)
    }

    fn unlink(&self, name: &str) -> VfsResult<()> {
        let mut children = self.children.lock();
        let entry = children.get(name).ok_or(VfsError::NotFound)?;
        if let Ok(dir) = entry.as_dir()
            && dir.has_children()?
        {
            return Err(VfsError::DirectoryNotEmpty);
        }
        children.remove(name);
        Ok(())
    }

    fn rename(&self, src_name: &str, dst_dir: &VfsDirNode, dst_name: &str) -> VfsResult<()> {
        let dst = dst_dir.downcast::<Self>()?;
        if core::ptr::eq(self, dst.as_ref()) {
            if src_name == dst_name {
                return Ok(());
            }
            let mut children = self.children.lock();
            let entry = children.get(src_name).cloned().ok_or(VfsError::NotFound)?;
            Self::check_replace_target(&children, dst_name)?;
            let rebound = self.rebind_entry(&entry, self.this.lock().upgrade(), dst_name)?;
            if rebound.is_dir() {
                let dir = rebound.as_dir()?.downcast::<Self>()?;
                Self::rebind_children_to(dir, &rebound)?;
            }
            children.remove(src_name);
            children.insert(dst_name.into(), rebound);
            return Ok(());
        }

        let entry = self.lookup(src_name)?;
        {
            let dst_children = dst.children.lock();
            Self::check_replace_target(&dst_children, dst_name)?;
        }
        let rebound = dst.rebind_entry(&entry, dst.this.lock().upgrade(), dst_name)?;
        if rebound.is_dir() {
            let dir = rebound.as_dir()?.downcast::<Self>()?;
            Self::rebind_children_to(dir, &rebound)?;
        }
        self.children.lock().remove(src_name);
        dst.children.lock().insert(dst_name.into(), rebound);
        Ok(())
    }
}

impl Pollable for RamDirNode {
    fn poll(&self) -> IoEvents {
        IoEvents::IN | IoEvents::OUT
    }

    fn register(&self, _context: &mut Context<'_>, _events: IoEvents) {}
}
