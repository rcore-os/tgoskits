use alloc::{
    collections::BTreeMap,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::{
    any::Any,
    sync::atomic::{AtomicBool, Ordering},
    task::Context,
};

use ax_fs::OpenOptions;
use ax_sync::Mutex;
use axfs_ng_vfs::{
    DirEntry, DirEntrySink, DirNode, DirNodeOps, FileNode, FileNodeOps, Filesystem, FilesystemOps,
    Location, Metadata, MetadataUpdate, NodeFlags, NodeOps, NodePermission, NodeType, Reference,
    StatFs, VfsError, VfsResult, WeakDirEntry,
};
use axpoll::{IoEvents, Pollable};

use crate::pseudofs::dummy_stat_fs;

const COPY_BUF_SIZE: usize = 4096;
const OVERLAY_MAGIC: u32 = 0x794c7630;

struct OverlayMarker {
    whiteout: AtomicBool,
    opaque: AtomicBool,
}

impl Default for OverlayMarker {
    fn default() -> Self {
        Self {
            whiteout: AtomicBool::new(false),
            opaque: AtomicBool::new(false),
        }
    }
}

#[derive(Clone)]
pub struct OverlayOptions {
    pub lower_dirs: Vec<Location>,
    pub upper_dir: Location,
    pub work_dir: Location,
}

pub fn new_overlayfs(options: OverlayOptions) -> VfsResult<Filesystem> {
    if options.lower_dirs.is_empty() {
        return Err(VfsError::InvalidInput);
    }
    options.upper_dir.check_is_dir()?;
    options.work_dir.check_is_dir()?;
    for lower in &options.lower_dirs {
        lower.check_is_dir()?;
    }

    let fs = Arc::new(OverlayFs {
        lower_dirs: options.lower_dirs,
        upper_dir: options.upper_dir,
        _work_dir: options.work_dir,
        root: Mutex::new(None),
    });
    let root = OverlayDir::entry(
        fs.clone(),
        fs.upper_dir.clone(),
        fs.lower_dirs.clone(),
        None,
    );
    *fs.root.lock() = Some(root);
    Ok(Filesystem::new(fs))
}

struct OverlayFs {
    lower_dirs: Vec<Location>,
    upper_dir: Location,
    _work_dir: Location,
    root: Mutex<Option<DirEntry>>,
}

impl FilesystemOps for OverlayFs {
    fn name(&self) -> &str {
        "overlay"
    }

    fn root_dir(&self) -> DirEntry {
        self.root.lock().clone().unwrap()
    }

    fn stat(&self) -> VfsResult<StatFs> {
        Ok(dummy_stat_fs(OVERLAY_MAGIC))
    }
}

fn marker(entry: &DirEntry) -> Option<Arc<OverlayMarker>> {
    entry.user_data().get::<OverlayMarker>()
}

fn is_whiteout(entry: &DirEntry) -> bool {
    marker(entry).is_some_and(|m| m.whiteout.load(Ordering::Acquire))
}

fn is_opaque(entry: &DirEntry) -> bool {
    marker(entry).is_some_and(|m| m.opaque.load(Ordering::Acquire))
}

fn mark_whiteout(entry: &DirEntry) {
    let marker = entry.user_data().get_or_insert_with(OverlayMarker::default);
    marker.whiteout.store(true, Ordering::Release);
}

fn mark_opaque(entry: &DirEntry) {
    let marker = entry.user_data().get_or_insert_with(OverlayMarker::default);
    marker.opaque.store(true, Ordering::Release);
}

fn lookup_visible_upper(dir: &Location, name: &str) -> VfsResult<Option<Location>> {
    match dir.lookup_no_follow(name) {
        Ok(loc) if is_whiteout(loc.entry()) => Ok(None),
        Ok(loc) => Ok(Some(loc)),
        Err(VfsError::NotFound) => Ok(None),
        Err(err) => Err(err),
    }
}

fn lookup_any_upper(dir: &Location, name: &str) -> VfsResult<Option<Location>> {
    match dir.lookup_no_follow(name) {
        Ok(loc) => Ok(Some(loc)),
        Err(VfsError::NotFound) => Ok(None),
        Err(err) => Err(err),
    }
}

fn lookup_lower(dirs: &[Location], name: &str) -> VfsResult<Option<Location>> {
    for dir in dirs {
        match dir.lookup_no_follow(name) {
            Ok(loc) => return Ok(Some(loc)),
            Err(VfsError::NotFound) => {}
            Err(err) => return Err(err),
        }
    }
    Ok(None)
}

fn read_names(
    dir: &Location,
    include_whiteouts: bool,
    names: &mut BTreeMap<String, DirentInfo>,
) -> VfsResult<()> {
    dir.read_dir(0, &mut |name: &str, ino, node_type, _| {
        if name == "." || name == ".." {
            return true;
        }
        let Ok(loc) = dir.lookup_no_follow(name) else {
            return true;
        };
        if is_whiteout(loc.entry()) {
            if include_whiteouts {
                names.insert(name.to_string(), DirentInfo { ino, node_type });
            } else {
                names.remove(name);
            }
        } else {
            names.insert(name.to_string(), DirentInfo { ino, node_type });
        }
        true
    })?;
    Ok(())
}

fn copy_file_contents(src: &Location, dst: &Location) -> VfsResult<()> {
    let src_file = OpenOptions::new()
        .read(true)
        .open_loc(src.clone())?
        .into_file()?;
    let dst_file = OpenOptions::new()
        .write(true)
        .open_loc(dst.clone())?
        .into_file()?;

    let mut offset = 0;
    let mut buf = [0u8; COPY_BUF_SIZE];
    loop {
        let read = src_file.read_at(&mut buf[..], offset)?;
        if read == 0 {
            break;
        }
        let mut written = 0;
        while written < read {
            let n = dst_file.write_at(&buf[written..read], offset + written as u64)?;
            if n == 0 {
                return Err(VfsError::InvalidData);
            }
            written += n;
        }
        offset += read as u64;
    }
    dst_file.backend()?.set_len(src.len()?)?;
    Ok(())
}

fn open_read(loc: Location) -> VfsResult<ax_fs::File> {
    OpenOptions::new().read(true).open_loc(loc)?.into_file()
}

fn open_write(loc: Location) -> VfsResult<ax_fs::File> {
    OpenOptions::new().write(true).open_loc(loc)?.into_file()
}

fn copy_metadata(src: &Location, dst: &Location) -> VfsResult<()> {
    let meta = src.metadata()?;
    dst.update_metadata(MetadataUpdate {
        mode: Some(meta.mode),
        owner: Some((meta.uid, meta.gid)),
        rdev: Some(meta.rdev),
        atime: Some(meta.atime),
        mtime: Some(meta.mtime),
    })
}

fn copy_entry(src: &Location, dst_dir: &Location, name: &str) -> VfsResult<Location> {
    let meta = src.metadata()?;
    let dst = dst_dir.create(name, meta.node_type, meta.mode, meta.uid, meta.gid)?;
    match meta.node_type {
        NodeType::RegularFile => copy_file_contents(src, &dst)?,
        NodeType::Symlink => dst.entry().as_file()?.set_symlink(&src.read_link()?)?,
        NodeType::Directory => {}
        _ => {}
    }
    copy_metadata(src, &dst)?;
    Ok(dst)
}

fn ensure_upper_from_lower(
    upper_dir: &Location,
    lower: &Location,
    name: &str,
) -> VfsResult<Location> {
    if let Some(upper) = lookup_visible_upper(upper_dir, name)? {
        return Ok(upper);
    }
    copy_entry(lower, upper_dir, name)
}

fn ensure_upper_dir(
    upper_dir: &Location,
    lower_dirs: &[Location],
    name: &str,
) -> VfsResult<Location> {
    if let Some(upper) = lookup_visible_upper(upper_dir, name)? {
        upper.check_is_dir()?;
        return Ok(upper);
    }
    let lower = lookup_lower(lower_dirs, name)?.ok_or(VfsError::NotFound)?;
    lower.check_is_dir()?;
    copy_entry(&lower, upper_dir, name)
}

#[derive(Clone)]
struct DirentInfo {
    ino: u64,
    node_type: NodeType,
}

struct OverlayDir {
    fs: Arc<OverlayFs>,
    upper_dir: Location,
    lower_dirs: Vec<Location>,
    this: Option<WeakDirEntry>,
}

impl OverlayDir {
    fn entry(
        fs: Arc<OverlayFs>,
        upper_dir: Location,
        lower_dirs: Vec<Location>,
        parent: Option<DirEntry>,
    ) -> DirEntry {
        DirEntry::new_dir(
            |this| {
                DirNode::new(Arc::new(Self {
                    fs,
                    upper_dir,
                    lower_dirs,
                    this: Some(this),
                }))
            },
            parent.map_or_else(Reference::root, |p| Reference::new(Some(p), String::new())),
        )
    }

    fn child_reference(&self, name: &str) -> Reference {
        Reference::new(
            self.this.as_ref().and_then(WeakDirEntry::upgrade),
            name.to_string(),
        )
    }

    fn lower_dirs_for_child(&self, name: &str) -> Vec<Location> {
        let mut result = Vec::new();
        for lower_dir in &self.lower_dirs {
            if let Ok(child) = lower_dir.lookup_no_follow(name)
                && child.node_type() == NodeType::Directory
            {
                result.push(child);
            }
        }
        result
    }

    fn build_entry(
        &self,
        name: &str,
        upper: Option<Location>,
        lower: Option<Location>,
    ) -> VfsResult<DirEntry> {
        let source = upper
            .as_ref()
            .or(lower.as_ref())
            .ok_or(VfsError::NotFound)?;
        let node_type = source.node_type();
        let reference = self.child_reference(name);
        if node_type == NodeType::Directory {
            let upper_dir = match upper {
                Some(loc) => loc,
                None => ensure_upper_dir(&self.upper_dir, &self.lower_dirs, name)?,
            };
            let lower_dirs = self.lower_dirs_for_child(name);
            let fs = self.fs.clone();
            Ok(DirEntry::new_dir(
                |this| {
                    DirNode::new(Arc::new(Self {
                        fs,
                        upper_dir,
                        lower_dirs,
                        this: Some(this),
                    }))
                },
                reference,
            ))
        } else {
            Ok(DirEntry::new_file(
                FileNode::new(Arc::new(OverlayFile {
                    fs: self.fs.clone(),
                    upper_dir: self.upper_dir.clone(),
                    name: name.to_string(),
                    upper,
                    lower,
                })),
                node_type,
                reference,
            ))
        }
    }

    fn ensure_no_visible_entry(&self, name: &str) -> VfsResult<()> {
        if lookup_visible_upper(&self.upper_dir, name)?.is_some()
            || lookup_lower(&self.lower_dirs, name)?.is_some()
        {
            return Err(VfsError::AlreadyExists);
        }
        Ok(())
    }

    fn remove_existing_whiteout(&self, name: &str) -> VfsResult<()> {
        if let Some(upper) = lookup_any_upper(&self.upper_dir, name)?
            && is_whiteout(upper.entry())
        {
            self.upper_dir.unlink(name, upper.is_dir())?;
        }
        Ok(())
    }
}

impl NodeOps for OverlayDir {
    fn inode(&self) -> u64 {
        self.upper_dir.inode()
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        self.upper_dir.metadata()
    }

    fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()> {
        self.upper_dir.update_metadata(update)
    }

    fn filesystem(&self) -> &dyn FilesystemOps {
        self.fs.as_ref()
    }

    fn sync(&self, data_only: bool) -> VfsResult<()> {
        self.upper_dir.sync(data_only)
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE
    }
}

impl DirNodeOps for OverlayDir {
    fn read_dir(&self, offset: u64, sink: &mut dyn DirEntrySink) -> VfsResult<usize> {
        let mut entries = BTreeMap::new();
        if !is_opaque(self.upper_dir.entry()) {
            for lower in self.lower_dirs.iter().rev() {
                read_names(lower, true, &mut entries)?;
            }
        }
        read_names(&self.upper_dir, false, &mut entries)?;

        let mut emitted = 0;
        for (idx, (name, info)) in entries.into_iter().enumerate().skip(offset as usize) {
            if !sink.accept(&name, info.ino, info.node_type, idx as u64 + 1) {
                break;
            }
            emitted += 1;
        }
        Ok(emitted)
    }

    fn lookup(&self, name: &str) -> VfsResult<DirEntry> {
        let upper = lookup_visible_upper(&self.upper_dir, name)?;
        let lower = if upper.is_some() {
            None
        } else {
            lookup_lower(&self.lower_dirs, name)?
        };
        if upper.is_none() && lower.is_none() {
            return Err(VfsError::NotFound);
        }
        self.build_entry(name, upper, lower)
    }

    fn is_cacheable(&self) -> bool {
        false
    }

    fn create(
        &self,
        name: &str,
        node_type: NodeType,
        permission: NodePermission,
        uid: u32,
        gid: u32,
    ) -> VfsResult<DirEntry> {
        self.ensure_no_visible_entry(name)?;
        self.remove_existing_whiteout(name)?;
        let upper = self
            .upper_dir
            .create(name, node_type, permission, uid, gid)?;
        self.build_entry(name, Some(upper), None)
    }

    fn link(&self, _name: &str, _node: &DirEntry) -> VfsResult<DirEntry> {
        Err(VfsError::CrossesDevices)
    }

    fn unlink(&self, name: &str) -> VfsResult<()> {
        if let Some(upper) = lookup_visible_upper(&self.upper_dir, name)? {
            self.upper_dir.unlink(name, upper.is_dir())?;
        }
        if lookup_lower(&self.lower_dirs, name)?.is_some() {
            let whiteout = self.upper_dir.create(
                name,
                NodeType::RegularFile,
                NodePermission::from_bits_truncate(0),
                0,
                0,
            )?;
            mark_whiteout(whiteout.entry());
            return Ok(());
        }
        Ok(())
    }

    fn rename(&self, src_name: &str, dst_dir: &DirNode, dst_name: &str) -> VfsResult<()> {
        let dst = dst_dir.downcast::<Self>()?;
        let src = match lookup_visible_upper(&self.upper_dir, src_name)? {
            Some(upper) => upper,
            None => {
                let lower = lookup_lower(&self.lower_dirs, src_name)?.ok_or(VfsError::NotFound)?;
                ensure_upper_from_lower(&self.upper_dir, &lower, src_name)?
            }
        };
        dst.remove_existing_whiteout(dst_name)?;
        self.upper_dir.rename(src_name, &dst.upper_dir, dst_name)?;
        if lookup_lower(&self.lower_dirs, src_name)?.is_some() {
            let whiteout = self.upper_dir.create(
                src_name,
                NodeType::RegularFile,
                NodePermission::from_bits_truncate(0),
                0,
                0,
            )?;
            mark_whiteout(whiteout.entry());
        }
        if src.is_dir()
            && let Some(moved) = lookup_visible_upper(&dst.upper_dir, dst_name)?
        {
            mark_opaque(moved.entry());
        }
        Ok(())
    }
}

struct OverlayFile {
    fs: Arc<OverlayFs>,
    upper_dir: Location,
    name: String,
    upper: Option<Location>,
    lower: Option<Location>,
}

impl OverlayFile {
    fn current(&self) -> VfsResult<Location> {
        if let Some(upper) = lookup_visible_upper(&self.upper_dir, &self.name)? {
            return Ok(upper);
        }
        self.lower.clone().ok_or(VfsError::NotFound)
    }

    fn ensure_upper(&self) -> VfsResult<Location> {
        if let Some(upper) = lookup_visible_upper(&self.upper_dir, &self.name)? {
            return Ok(upper);
        }
        let lower = self.lower.as_ref().ok_or(VfsError::NotFound)?;
        ensure_upper_from_lower(&self.upper_dir, lower, &self.name)
    }
}

impl NodeOps for OverlayFile {
    fn inode(&self) -> u64 {
        self.upper
            .as_ref()
            .or(self.lower.as_ref())
            .map_or(0, Location::inode)
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        self.current()?.metadata()
    }

    fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()> {
        self.ensure_upper()?.update_metadata(update)
    }

    fn filesystem(&self) -> &dyn FilesystemOps {
        self.fs.as_ref()
    }

    fn sync(&self, data_only: bool) -> VfsResult<()> {
        self.current()?.sync(data_only)
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn flags(&self) -> NodeFlags {
        self.current().map_or(NodeFlags::NON_CACHEABLE, |loc| {
            loc.flags() | NodeFlags::NON_CACHEABLE
        })
    }
}

impl FileNodeOps for OverlayFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        open_read(self.current()?)?.read_at(buf, offset)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize> {
        open_write(self.ensure_upper()?)?.write_at(buf, offset)
    }

    fn append(&self, buf: &[u8]) -> VfsResult<(usize, u64)> {
        let upper = self.ensure_upper()?;
        let len = upper.len()?;
        open_write(upper)?
            .write_at(buf, len)
            .map(|written| (written, len + written as u64))
    }

    fn set_len(&self, len: u64) -> VfsResult<()> {
        open_write(self.ensure_upper()?)?.backend()?.set_len(len)
    }

    fn set_symlink(&self, target: &str) -> VfsResult<()> {
        self.ensure_upper()?.entry().as_file()?.set_symlink(target)
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        self.current()?.ioctl(cmd, arg)
    }
}

impl Pollable for OverlayFile {
    fn poll(&self) -> IoEvents {
        self.current()
            .map_or(IoEvents::ERR, |loc| loc.entry().poll())
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        if let Ok(loc) = self.current() {
            loc.entry().register(context, events);
        }
    }
}
