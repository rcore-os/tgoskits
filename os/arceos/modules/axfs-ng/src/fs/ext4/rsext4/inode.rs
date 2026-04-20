use alloc::{
    borrow::ToOwned,
    format,
    string::{String, ToString},
    sync::Arc,
};
use core::any::Any;

use axfs_ng_vfs::{
    DeviceId, DirEntry, DirEntrySink, DirNode, DirNodeOps, FileNode, FileNodeOps, FilesystemOps,
    Metadata, MetadataUpdate, NodeFlags, NodeOps, NodePermission, NodeType, Reference, VfsError,
    VfsResult, WeakDirEntry,
};
use axpoll::{IoEvents, Pollable};
use rsext4::{BLOCK_SIZE, bmalloc::InodeNumber};

use super::{
    Ext4Filesystem,
    util::{dir_entry_type_to_vfs, inode_to_vfs_type, into_vfs_err, vfs_type_to_dir_entry},
};

pub struct Inode {
    fs: Arc<Ext4Filesystem>,
    ino: InodeNumber,
    this: Option<WeakDirEntry>,
    path: Option<String>,
}

impl Inode {
    pub(crate) fn new(
        fs: Arc<Ext4Filesystem>,
        ino: InodeNumber,
        this: Option<WeakDirEntry>,
        path: Option<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            fs,
            ino,
            this,
            path,
        })
    }

    fn create_entry(
        &self,
        ino: InodeNumber,
        inode: &rsext4::disknode::Ext4Inode,
        name: impl Into<String>,
    ) -> DirEntry {
        let name = name.into();
        let reference = Reference::new(
            self.this.as_ref().and_then(WeakDirEntry::upgrade),
            name.clone(),
        );
        let path = self.dir_path().map(|dir| join_child_path(&dir, &name)).ok();
        if inode.is_dir() {
            DirEntry::new_dir(
                |this| DirNode::new(Inode::new(self.fs.clone(), ino, Some(this), path.clone())),
                reference,
            )
        } else {
            DirEntry::new_file(
                FileNode::new(Inode::new(self.fs.clone(), ino, None, path)),
                inode_to_vfs_type(inode.is_dir(), inode.is_file(), inode.is_symlink()),
                reference,
            )
        }
    }

    fn dir_path(&self) -> VfsResult<String> {
        if let Some(this) = self.this.as_ref().and_then(WeakDirEntry::upgrade) {
            return Ok(this.absolute_path()?.to_string());
        }
        self.path.clone().ok_or(VfsError::InvalidInput)
    }

    fn lookup_locked(&self, name: &str) -> VfsResult<DirEntry> {
        let path = join_child_path(&self.dir_path()?, name);
        let mut state = self.fs.lock();
        let (fs, dev) = state.split();
        let (ino, inode) = rsext4::dir::get_inode_with_num(fs, dev, &path)
            .map_err(into_vfs_err)?
            .ok_or(VfsError::NotFound)?;
        Ok(self.create_entry(ino, &inode, name))
    }

    fn update_ctime_with(
        fs: &mut rsext4::Ext4FileSystem,
        dev: &mut rsext4::Jbd2Dev<super::Ext4Disk>,
        ino: InodeNumber,
    ) -> VfsResult<()> {
        fs.modify_inode(dev, ino, |inode| {
            if cfg!(feature = "times") {
                inode.i_ctime = ax_hal::time::wall_time().as_secs() as u32;
            }
        })
        .map_err(into_vfs_err)
    }
}

impl NodeOps for Inode {
    fn inode(&self) -> u64 {
        self.ino.as_u64()
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        let mut state = self.fs.lock();
        let (fs, dev) = state.split();
        let inode = fs.get_inode_by_num(dev, self.ino).map_err(into_vfs_err)?;
        Ok(Metadata {
            inode: self.ino.as_u64(),
            device: 0,
            nlink: inode.i_links_count as _,
            mode: NodePermission::from_bits_truncate(inode.i_mode & 0o777),
            node_type: inode_to_vfs_type(inode.is_dir(), inode.is_file(), inode.is_symlink()),
            uid: inode.uid(),
            gid: inode.gid(),
            size: inode.size(),
            block_size: fs.superblock.block_size(),
            blocks: inode.blocks_count(),
            rdev: DeviceId::default(),
            atime: core::time::Duration::from_secs(inode.i_atime as u64),
            mtime: core::time::Duration::from_secs(inode.i_mtime as u64),
            ctime: core::time::Duration::from_secs(inode.i_ctime as u64),
        })
    }

    fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()> {
        {
            let mut state = self.fs.lock();
            let (fs, dev) = state.split();
            fs.modify_inode(dev, self.ino, |inode| {
                if let Some(mode) = update.mode {
                    inode.i_mode = (inode.i_mode & !0o777) | mode.bits();
                }
                if let Some((uid, gid)) = update.owner {
                    inode.i_uid = (uid & 0xffff) as u16;
                    inode.l_i_uid_high = ((uid >> 16) & 0xffff) as u16;
                    inode.i_gid = (gid & 0xffff) as u16;
                    inode.l_i_gid_high = ((gid >> 16) & 0xffff) as u16;
                }
                if let Some(atime) = update.atime {
                    inode.i_atime = atime.as_secs() as u32;
                }
                if let Some(mtime) = update.mtime {
                    inode.i_mtime = mtime.as_secs() as u32;
                }
                if cfg!(feature = "times") {
                    inode.i_ctime = ax_hal::time::wall_time().as_secs() as u32;
                }
            })
            .map_err(into_vfs_err)?;
        }
        self.fs.sync_to_disk()
    }

    fn len(&self) -> VfsResult<u64> {
        let mut state = self.fs.lock();
        let (fs, dev) = state.split();
        fs.get_inode_by_num(dev, self.ino)
            .map(|inode| inode.size())
            .map_err(into_vfs_err)
    }

    fn filesystem(&self) -> &dyn FilesystemOps {
        &*self.fs
    }

    fn sync(&self, _data_only: bool) -> VfsResult<()> {
        self.fs.sync_to_disk()
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::BLOCKING
    }
}

impl FileNodeOps for Inode {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let mut state = self.fs.lock();
        let (fs, dev) = state.split();
        if buf.is_empty() {
            return Ok(0);
        }

        let mut inode = fs.get_inode_by_num(dev, self.ino).map_err(into_vfs_err)?;
        let file_size = inode.size();
        if offset >= file_size {
            return Ok(0);
        }

        let to_read = core::cmp::min(buf.len() as u64, file_size - offset) as usize;
        if to_read == 0 {
            return Ok(0);
        }

        if inode.is_symlink() {
            let total = file_size as usize;
            let to_read = core::cmp::min(buf.len(), total - offset as usize);
            if total <= 60 {
                let mut raw = [0u8; 60];
                for i in 0..15 {
                    raw[i * 4..i * 4 + 4].copy_from_slice(&inode.i_block[i].to_le_bytes());
                }
                let start = offset as usize;
                let end = start + to_read;
                buf[..to_read].copy_from_slice(&raw[start..end]);
                return Ok(to_read);
            }
        }

        if !inode.have_extend_header_and_use_extend() {
            return Err(VfsError::Unsupported);
        }

        let block_bytes = BLOCK_SIZE as u64;
        let end_off = offset + to_read as u64;
        let start_lbn = offset / block_bytes;
        let end_lbn = (end_off - 1) / block_bytes;

        let mut written = 0usize;
        for lbn in start_lbn..=end_lbn {
            let lbn_start = lbn * block_bytes;
            let lbn_end = lbn_start + block_bytes;

            let copy_start = core::cmp::max(offset, lbn_start) - lbn_start;
            let copy_end = core::cmp::min(end_off, lbn_end) - lbn_start;
            let copy_len = copy_end.saturating_sub(copy_start);
            if copy_len == 0 {
                continue;
            }

            if let Some(phys) = rsext4::loopfile::resolve_inode_block(dev, &mut inode, lbn as u32)
                .map_err(into_vfs_err)?
            {
                let cached = fs
                    .datablock_cache
                    .get_or_load(dev, phys)
                    .map_err(into_vfs_err)?;
                let data = &cached.data[..block_bytes as usize];
                buf[written..written + copy_len as usize]
                    .copy_from_slice(&data[copy_start as usize..(copy_start + copy_len) as usize]);
            } else {
                for b in &mut buf[written..written + copy_len as usize] {
                    *b = 0;
                }
            }

            written += copy_len as usize;
            if written >= to_read {
                break;
            }
        }

        Ok(written)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize> {
        {
            let mut state = self.fs.lock();
            let (fs, dev) = state.split();
            rsext4::write_file(
                dev,
                fs,
                &self.path.clone().ok_or(VfsError::InvalidInput)?,
                offset,
                buf,
            )
            .map_err(into_vfs_err)?;
        }
        self.fs.sync_to_disk()?;
        Ok(buf.len())
    }

    fn append(&self, buf: &[u8]) -> VfsResult<(usize, u64)> {
        let length = {
            let mut state = self.fs.lock();
            let (fs, dev) = state.split();
            let inode = fs.get_inode_by_num(dev, self.ino).map_err(into_vfs_err)?;
            let length = inode.size();
            rsext4::write_file(
                dev,
                fs,
                &self.path.clone().ok_or(VfsError::InvalidInput)?,
                length,
                buf,
            )
            .map_err(into_vfs_err)?;
            length
        };
        self.fs.sync_to_disk()?;
        Ok((buf.len(), length + buf.len() as u64))
    }

    fn set_len(&self, len: u64) -> VfsResult<()> {
        {
            let mut state = self.fs.lock();
            let (fs, dev) = state.split();
            rsext4::truncate(
                dev,
                fs,
                &self.path.clone().ok_or(VfsError::InvalidInput)?,
                len,
            )
            .map_err(into_vfs_err)?;
        }
        self.fs.sync_to_disk()
    }

    fn set_symlink(&self, target: &str) -> VfsResult<()> {
        let Some(_path) = self.path.clone() else {
            return Err(VfsError::InvalidInput);
        };

        {
            let mut state = self.fs.lock();
            let (fs, dev) = state.split();
            let mut inode = fs.get_inode_by_num(dev, self.ino).map_err(into_vfs_err)?;

            if !inode.is_symlink() {
                return Err(VfsError::InvalidInput);
            }

            if let Ok(blocks) = rsext4::loopfile::resolve_inode_block_allextend(fs, dev, &mut inode)
            {
                for blk in blocks.values() {
                    let _ = fs.free_block(dev, *blk);
                }
            }

            let target_bytes = target.as_bytes();
            let target_len = target_bytes.len();
            inode.i_size_lo = (target_len as u64 & 0xffffffff) as u32;
            inode.i_size_high = ((target_len as u64) >> 32) as u32;
            inode.i_blocks_lo = 0;
            inode.l_i_blocks_high = 0;
            inode.i_block = [0; 15];

            if target_len == 0 {
                inode.i_flags &= !rsext4::disknode::Ext4Inode::EXT4_EXTENTS_FL;
            } else if target_len <= 60 {
                inode.i_flags &= !rsext4::disknode::Ext4Inode::EXT4_EXTENTS_FL;
                let mut raw = [0u8; 60];
                raw[..target_len].copy_from_slice(target_bytes);
                for i in 0..15 {
                    inode.i_block[i] = u32::from_le_bytes([
                        raw[i * 4],
                        raw[i * 4 + 1],
                        raw[i * 4 + 2],
                        raw[i * 4 + 3],
                    ]);
                }
            } else {
                if !fs.superblock.has_extents() {
                    return Err(VfsError::Unsupported);
                }

                let mut data_blocks = alloc::vec::Vec::new();
                let mut remaining = target_len;
                let mut src_off = 0usize;
                while remaining > 0 {
                    let blk = fs.alloc_block(dev).map_err(into_vfs_err)?;
                    let write_len = core::cmp::min(remaining, BLOCK_SIZE);
                    fs.datablock_cache
                        .modify_new(dev, blk, |data| {
                            for b in data.iter_mut() {
                                *b = 0;
                            }
                            let end = src_off + write_len;
                            data[..write_len].copy_from_slice(&target_bytes[src_off..end]);
                        })
                        .map_err(into_vfs_err)?;
                    data_blocks.push(blk);
                    remaining -= write_len;
                    src_off += write_len;
                }

                let used_datablocks = data_blocks.len() as u64;
                let iblocks_used = used_datablocks.saturating_mul(BLOCK_SIZE as u64 / 512) as u32;
                inode.i_blocks_lo = iblocks_used;
                inode.l_i_blocks_high = 0;
                rsext4::file::build_file_block_mapping(fs, &mut inode, &data_blocks, dev);
            }

            fs.modify_inode(dev, self.ino, |on_disk| {
                *on_disk = inode;
            })
            .map_err(into_vfs_err)?;
        }

        self.fs.sync_to_disk()
    }
}

impl Pollable for Inode {
    fn poll(&self) -> IoEvents {
        IoEvents::IN | IoEvents::OUT
    }

    fn register(&self, _context: &mut core::task::Context<'_>, _events: IoEvents) {}
}

impl DirNodeOps for Inode {
    fn read_dir(&self, offset: u64, sink: &mut dyn DirEntrySink) -> VfsResult<usize> {
        let mut state = self.fs.lock();
        let (fs, dev) = state.split();
        let mut inode = fs.get_inode_by_num(dev, self.ino).map_err(into_vfs_err)?;

        let blocks = rsext4::loopfile::resolve_inode_block_allextend(fs, dev, &mut inode)
            .map_err(into_vfs_err)?;

        let mut idx = 0u64;
        let mut count = 0usize;
        for &phys in blocks.values() {
            let cached = fs
                .datablock_cache
                .get_or_load(dev, phys)
                .map_err(into_vfs_err)?;
            let data = &cached.data[..BLOCK_SIZE];
            let iter = rsext4::entries::DirEntryIterator::new(data);
            for (entry, _) in iter {
                if entry.inode == 0 {
                    continue;
                }
                if idx < offset {
                    idx += 1;
                    continue;
                }
                let name = core::str::from_utf8(entry.name)
                    .map_err(|_| VfsError::InvalidData)?
                    .to_owned();
                let node_type = dir_entry_type_to_vfs(entry.file_type);
                idx += 1;
                if !sink.accept(&name, entry.inode as u64, node_type, idx) {
                    return Ok(count);
                }
                count += 1;
            }
        }

        Ok(count)
    }

    fn lookup(&self, name: &str) -> VfsResult<DirEntry> {
        if name == "." {
            return self
                .this
                .as_ref()
                .and_then(WeakDirEntry::upgrade)
                .ok_or(VfsError::NotFound);
        }
        if name == ".." {
            return self
                .this
                .as_ref()
                .and_then(WeakDirEntry::upgrade)
                .and_then(|entry| entry.parent())
                .ok_or(VfsError::NotFound);
        }
        self.lookup_locked(name)
    }

    fn create(
        &self,
        name: &str,
        node_type: NodeType,
        permission: NodePermission,
    ) -> VfsResult<DirEntry> {
        let Some(dir_path) = self.dir_path().ok() else {
            return Err(VfsError::InvalidInput);
        };
        let path = join_child_path(&dir_path, name);
        let ino = {
            let mut state = self.fs.lock();
            let (fs, dev) = state.split();
            if rsext4::dir::get_inode_with_num(fs, dev, &path)
                .map_err(into_vfs_err)?
                .is_some()
            {
                return Err(VfsError::AlreadyExists);
            }

            if node_type == NodeType::Directory {
                rsext4::mkdir(dev, fs, &path).map_err(into_vfs_err)?;
            } else {
                let file_type = vfs_type_to_dir_entry(node_type).ok_or(VfsError::InvalidData)?;
                rsext4::mkfile(dev, fs, &path, None, Some(file_type)).map_err(into_vfs_err)?;
            };

            let (ino, _inode) = rsext4::dir::get_inode_with_num(fs, dev, &path)
                .map_err(into_vfs_err)?
                .ok_or(VfsError::NotFound)?;

            let mode_bits = permission.bits();
            fs.modify_inode(dev, ino, |node| {
                node.i_mode = (node.i_mode & !0o777) | mode_bits;
            })
            .map_err(into_vfs_err)?;
            Self::update_ctime_with(fs, dev, ino)?;
            ino
        };

        self.fs.sync_to_disk()?;

        let reference = Reference::new(
            self.this.as_ref().and_then(WeakDirEntry::upgrade),
            name.to_owned(),
        );
        Ok(if node_type == NodeType::Directory {
            DirEntry::new_dir(
                |this| DirNode::new(Inode::new(self.fs.clone(), ino, Some(this), Some(path))),
                reference,
            )
        } else {
            DirEntry::new_file(
                FileNode::new(Inode::new(self.fs.clone(), ino, None, Some(path))),
                node_type,
                reference,
            )
        })
    }

    fn link(&self, name: &str, node: &DirEntry) -> VfsResult<DirEntry> {
        let dir_path = self.dir_path()?;
        let link_path = join_child_path(&dir_path, name);
        let target_path = node.absolute_path()?.to_string();
        {
            let mut state = self.fs.lock();
            let (fs, dev) = state.split();

            if rsext4::dir::get_inode_with_num(fs, dev, &target_path)
                .map_err(into_vfs_err)?
                .is_none()
            {
                return Err(VfsError::NotFound);
            }
            if rsext4::dir::get_inode_with_num(fs, dev, &link_path)
                .map_err(into_vfs_err)?
                .is_some()
            {
                return Err(VfsError::AlreadyExists);
            }

            rsext4::link(fs, dev, &link_path, &target_path).map_err(into_vfs_err)?;
            let target_ino = InodeNumber::new(node.inode() as u32).map_err(into_vfs_err)?;
            Self::update_ctime_with(fs, dev, target_ino)?;
        }
        self.fs.sync_to_disk()?;
        self.lookup_locked(name)
    }

    fn unlink(&self, name: &str) -> VfsResult<()> {
        let dir_path = self.dir_path()?;
        let path = join_child_path(&dir_path, name);
        {
            let mut state = self.fs.lock();
            let (fs, dev) = state.split();
            if rsext4::dir::get_inode_with_num(fs, dev, &path)
                .map_err(into_vfs_err)?
                .is_none()
            {
                return Err(VfsError::NotFound);
            }
            rsext4::unlink(fs, dev, &path).map_err(into_vfs_err)?;
        }
        self.fs.sync_to_disk()
    }

    fn rename(&self, src_name: &str, dst_dir: &DirNode, dst_name: &str) -> VfsResult<()> {
        let dst_dir: Arc<Self> = dst_dir.downcast().map_err(|_| VfsError::InvalidInput)?;
        let src_path = join_child_path(&self.dir_path()?, src_name);
        let dst_path = join_child_path(&dst_dir.dir_path()?, dst_name);
        {
            let mut state = self.fs.lock();
            let (fs, dev) = state.split();
            rsext4::rename(dev, fs, &src_path, &dst_path).map_err(into_vfs_err)?;
        }
        self.fs.sync_to_disk()
    }
}

fn join_child_path(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else {
        format!("{parent}/{name}")
    }
}
