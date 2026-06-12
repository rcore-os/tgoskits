//! Minimal overlay filesystem implementation for StarryOS.
//!
//! The overlay view is built from an optional writable upper directory and one
//! or more read-only lower directories. Reads prefer upper entries and then
//! fall back to lower entries. Mutating operations materialize the relevant
//! upper path and copy lower-backed files up before applying changes.
//!
//! This implementation intentionally keeps some Linux overlayfs features
//! conservative: hard links are forced through upper, lower-backed directory
//! rename is rejected, and index/redirect_dir are handled by mount option
//! validation rather than by full semantic support here.

use alloc::{
    collections::BTreeMap,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::{any::Any, task::Context};

use ax_fs::OpenOptions;
use ax_sync::Mutex;
use axfs_ng_vfs::{
    DeviceId, DirEntry, DirEntrySink, DirNode, DirNodeOps, FileNode, FileNodeOps, Filesystem,
    FilesystemOps, Location, Metadata, MetadataUpdate, NodeFlags, NodeOps, NodePermission,
    NodeType, Reference, StatFs, VfsError, VfsResult, WeakDirEntry,
};
use axpoll::{IoEvents, Pollable};

use crate::pseudofs::dummy_stat_fs;

const COPY_BUF_SIZE: usize = 4096;
const OVERLAY_MAGIC: u32 = 0x794c7630;
const WHITEOUT_DEVICE: DeviceId = DeviceId::new(0, 0);
const OPAQUE_MARKER_NAME: &str = ".wh..wh..opq";

#[derive(Clone)]
pub struct OverlayOptions {
    /// Lower layers ordered from topmost to bottommost.
    pub lower_dirs: Vec<Location>,
    /// Writable upper layer. `None` creates a read-only lower-only overlay.
    pub upper_dir: Option<Location>,
    /// Work directory required by the mount ABI when an upper layer exists.
    pub work_dir: Option<Location>,
}

/// Build an overlay filesystem from resolved lower, upper, and work dirs.
pub fn new_overlayfs(options: OverlayOptions) -> VfsResult<Filesystem> {
    if options.lower_dirs.is_empty() {
        return Err(VfsError::InvalidInput);
    }
    if options.upper_dir.is_some() != options.work_dir.is_some() {
        return Err(VfsError::InvalidInput);
    }
    if let Some(upper_dir) = &options.upper_dir {
        upper_dir.check_is_dir()?;
    }
    if let Some(work_dir) = &options.work_dir {
        work_dir.check_is_dir()?;
    }
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
        Vec::new(),
        None,
    );
    *fs.root.lock() = Some(root);
    Ok(Filesystem::new(fs))
}

/// Return the real entry currently backing an overlay location.
///
/// This is used by metadata side stores such as xattrs: overlay dir entries are
/// non-cacheable, so storing data on the temporary overlay entry would be lost
/// after the next path lookup.
pub(crate) fn visible_target(loc: &Location) -> VfsResult<Location> {
    if let Ok(file) = loc.entry().downcast::<OverlayFile>() {
        file.current()
    } else if let Ok(dir) = loc.entry().downcast::<OverlayDir>() {
        dir.current_dir()
    } else {
        Ok(loc.clone())
    }
}

/// Ensure an overlay location is writable and return the real upper entry.
///
/// Non-overlay locations are returned unchanged. For overlay files this may
/// copy a lower-backed file into upper; for overlay directories this
/// materializes the corresponding upper directory path.
pub(crate) fn ensure_copy_up_target(loc: &Location) -> VfsResult<Location> {
    if let Ok(file) = loc.entry().downcast::<OverlayFile>() {
        file.ensure_upper()
    } else if let Ok(dir) = loc.entry().downcast::<OverlayDir>() {
        dir.materialize_upper_dir()
    } else {
        Ok(loc.clone())
    }
}

struct OverlayFs {
    lower_dirs: Vec<Location>,
    upper_dir: Option<Location>,
    _work_dir: Option<Location>,
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

fn is_whiteout(loc: &Location) -> VfsResult<bool> {
    if loc.node_type() != NodeType::CharacterDevice {
        return Ok(false);
    }
    Ok(loc.metadata()?.rdev == WHITEOUT_DEVICE)
}

/// Check whether an upper directory hides all lower entries under the same dir.
fn is_opaque(dir: &Location) -> VfsResult<bool> {
    match dir.lookup_no_follow(OPAQUE_MARKER_NAME) {
        Ok(_) => Ok(true),
        Err(VfsError::NotFound) => Ok(false),
        Err(err) => Err(err),
    }
}

/// Create the Linux overlayfs whiteout marker: char device with rdev 0:0.
fn create_whiteout(dir: &Location, name: &str) -> VfsResult<()> {
    let whiteout = dir.create(
        name,
        NodeType::CharacterDevice,
        NodePermission::from_bits_truncate(0),
        0,
        0,
    )?;
    whiteout.update_metadata(MetadataUpdate {
        rdev: Some(WHITEOUT_DEVICE),
        ..Default::default()
    })
}

/// Mark an upper directory as opaque by creating the `.wh..wh..opq` marker.
fn mark_opaque(dir: &Location) -> VfsResult<()> {
    match dir.lookup_no_follow(OPAQUE_MARKER_NAME) {
        Ok(_) => Ok(()),
        Err(VfsError::NotFound) => {
            dir.create(
                OPAQUE_MARKER_NAME,
                NodeType::RegularFile,
                NodePermission::from_bits_truncate(0),
                0,
                0,
            )?;
            Ok(())
        }
        Err(err) => Err(err),
    }
}

enum UpperLookup {
    /// A normal visible upper entry exists.
    Present(Location),
    /// A whiteout exists and must hide any lower entry with the same name.
    Whiteout,
    /// Upper has no entry for this name.
    Missing,
}

/// Lookup in upper without collapsing whiteout and missing into one state.
fn lookup_upper(dir: &Location, name: &str) -> VfsResult<UpperLookup> {
    if name == OPAQUE_MARKER_NAME {
        return Ok(UpperLookup::Whiteout);
    }
    match dir.lookup_no_follow(name) {
        Ok(loc) if is_whiteout(&loc)? => Ok(UpperLookup::Whiteout),
        Ok(loc) => Ok(UpperLookup::Present(loc)),
        Err(VfsError::NotFound) => Ok(UpperLookup::Missing),
        Err(err) => Err(err),
    }
}

/// Lookup a visible upper entry, hiding whiteouts from callers that only need
/// a normal entry or no entry.
fn lookup_visible_upper(dir: &Location, name: &str) -> VfsResult<Option<Location>> {
    match lookup_upper(dir, name)? {
        UpperLookup::Present(loc) => Ok(Some(loc)),
        UpperLookup::Whiteout | UpperLookup::Missing => Ok(None),
    }
}

/// Lookup raw upper entries, including whiteout markers.
fn lookup_any_upper(dir: &Location, name: &str) -> VfsResult<Option<Location>> {
    match dir.lookup_no_follow(name) {
        Ok(loc) => Ok(Some(loc)),
        Err(VfsError::NotFound) => Ok(None),
        Err(err) => Err(err),
    }
}

/// Lookup lower layers from topmost to bottommost.
fn lookup_lower(dirs: &[Location], name: &str) -> VfsResult<Option<Location>> {
    for dir in dirs {
        match dir.lookup_no_follow(name) {
            Ok(loc) if is_whiteout(&loc)? => return Ok(None),
            Ok(loc) => return Ok(Some(loc)),
            Err(VfsError::NotFound) => {}
            Err(err) => return Err(err),
        }
    }
    Ok(None)
}

/// Merge one directory's names into a read_dir map.
///
/// Whiteouts remove earlier lower names from the merged view, while opaque
/// markers are hidden from users.
fn read_names(dir: &Location, names: &mut BTreeMap<String, DirentInfo>) -> VfsResult<()> {
    dir.read_dir(0, &mut |name: &str, ino, node_type, _| {
        if name == "." || name == ".." || name == OPAQUE_MARKER_NAME {
            return true;
        }
        let Ok(loc) = dir.lookup_no_follow(name) else {
            return true;
        };
        if is_whiteout(&loc).unwrap_or(false) {
            names.remove(name);
        } else {
            names.insert(name.to_string(), DirentInfo { ino, node_type });
        }
        true
    })?;
    Ok(())
}

/// Copy regular file contents from lower to a newly-created upper file.
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

/// Copy metadata that should survive copy-up.
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

/// Copy a lower entry into an upper directory.
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

/// Return an existing upper entry or copy the lower entry up.
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

#[derive(Clone)]
struct DirentInfo {
    ino: u64,
    node_type: NodeType,
}

struct OverlayDir {
    fs: Arc<OverlayFs>,
    /// Materialized upper directory for this overlay path, if it exists.
    upper_dir: Mutex<Option<Location>>,
    /// Lower directories that still participate in this overlay directory.
    lower_dirs: Vec<Location>,
    /// Path from overlay root to this directory, used for deferred copy-up.
    path: Vec<String>,
    this: Option<WeakDirEntry>,
}

impl OverlayDir {
    /// Build an overlay directory entry with the corresponding upper/lower set.
    fn entry(
        fs: Arc<OverlayFs>,
        upper_dir: Option<Location>,
        lower_dirs: Vec<Location>,
        path: Vec<String>,
        parent: Option<DirEntry>,
    ) -> DirEntry {
        DirEntry::new_dir(
            |this| {
                DirNode::new(Arc::new(Self {
                    fs,
                    upper_dir: Mutex::new(upper_dir),
                    lower_dirs,
                    path,
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

    /// Collect lower child directories that should be visible below `name`.
    ///
    /// The list is used when constructing an overlay child directory. A lower
    /// whiteout stops the search because it hides lower layers beneath it.
    fn lower_dirs_for_child_in(dirs: &[Location], name: &str) -> Vec<Location> {
        let mut result = Vec::new();
        for lower_dir in dirs {
            if let Ok(child) = lower_dir.lookup_no_follow(name) {
                if is_whiteout(&child).unwrap_or(false) {
                    break;
                }
                if child.node_type() == NodeType::Directory {
                    result.push(child);
                }
            }
        }
        result
    }

    fn lower_dirs_for_child(&self, name: &str) -> Vec<Location> {
        Self::lower_dirs_for_child_in(&self.lower_dirs, name)
    }

    fn child_path(&self, name: &str) -> Vec<String> {
        let mut path = self.path.clone();
        path.push(name.to_string());
        path
    }

    fn existing_upper_dir(&self) -> Option<Location> {
        self.upper_dir.lock().clone()
    }

    /// Ensure this overlay directory has a real upper directory.
    ///
    /// Lower-only lookups should not create upper state. This is called only by
    /// operations that must write into upper or need an upper parent for a
    /// copied-up child.
    fn materialize_upper_dir(&self) -> VfsResult<Location> {
        if let Some(upper_dir) = self.existing_upper_dir() {
            return Ok(upper_dir);
        }

        let mut upper_dir = self
            .fs
            .upper_dir
            .clone()
            .ok_or(VfsError::ReadOnlyFilesystem)?;
        let mut lower_dirs = self.fs.lower_dirs.clone();
        for name in &self.path {
            if let Some(existing) = lookup_visible_upper(&upper_dir, name)? {
                existing.check_is_dir()?;
                upper_dir = existing;
            } else {
                let lower = lookup_lower(&lower_dirs, name)?.ok_or(VfsError::NotFound)?;
                lower.check_is_dir()?;
                upper_dir = copy_entry(&lower, &upper_dir, name)?;
            }
            lower_dirs = Self::lower_dirs_for_child_in(&lower_dirs, name);
        }

        *self.upper_dir.lock() = Some(upper_dir.clone());
        Ok(upper_dir)
    }

    fn current_dir(&self) -> VfsResult<Location> {
        self.existing_upper_dir()
            .or_else(|| self.lower_dirs.first().cloned())
            .ok_or(VfsError::NotFound)
    }

    fn lookup_visible_upper_child(&self, name: &str) -> VfsResult<Option<Location>> {
        match self.existing_upper_dir() {
            Some(upper_dir) => lookup_visible_upper(&upper_dir, name),
            None => Ok(None),
        }
    }

    fn lookup_upper_child(&self, name: &str) -> VfsResult<UpperLookup> {
        match self.existing_upper_dir() {
            Some(upper_dir) => lookup_upper(&upper_dir, name),
            None => Ok(UpperLookup::Missing),
        }
    }

    fn lookup_any_upper_child(&self, name: &str) -> VfsResult<Option<Location>> {
        match self.existing_upper_dir() {
            Some(upper_dir) => lookup_any_upper(&upper_dir, name),
            None => Ok(None),
        }
    }

    /// Build the overlay child direntry that users see.
    ///
    /// Directory children keep both their upper and lower candidates. File
    /// children store the current upper/lower locations and copy up lazily on
    /// writes or metadata changes.
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
            if let Some(upper) = &upper {
                upper.check_is_dir()?;
            }
            let lower_dirs = self.lower_dirs_for_child(name);
            let path = self.child_path(name);
            let fs = self.fs.clone();
            Ok(DirEntry::new_dir(
                |this| {
                    DirNode::new(Arc::new(Self {
                        fs,
                        upper_dir: Mutex::new(upper),
                        lower_dirs,
                        path,
                        this: Some(this),
                    }))
                },
                reference,
            ))
        } else {
            Ok(DirEntry::new_file(
                FileNode::new(Arc::new(OverlayFile {
                    fs: self.fs.clone(),
                    upper_dir: Mutex::new(self.existing_upper_dir()),
                    parent_path: self.path.clone(),
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
        match self.lookup_upper_child(name)? {
            UpperLookup::Present(_) => Err(VfsError::AlreadyExists),
            UpperLookup::Whiteout => Ok(()),
            UpperLookup::Missing => {
                if lookup_lower(&self.lower_dirs, name)?.is_some() {
                    return Err(VfsError::AlreadyExists);
                }
                Ok(())
            }
        }
    }

    /// Remove an old whiteout before creating a fresh upper entry of the same
    /// name.
    fn remove_existing_whiteout(&self, name: &str) -> VfsResult<()> {
        if let Some(upper) = self.lookup_any_upper_child(name)?
            && is_whiteout(&upper)?
            && let Some(upper_dir) = self.existing_upper_dir()
        {
            upper_dir.unlink(name, upper.is_dir())?;
        }
        Ok(())
    }
}

impl NodeOps for OverlayDir {
    fn inode(&self) -> u64 {
        self.current_dir().map_or(0, |loc| loc.inode())
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        self.current_dir()?.metadata()
    }

    fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()> {
        self.materialize_upper_dir()?.update_metadata(update)
    }

    fn filesystem(&self) -> &dyn FilesystemOps {
        self.fs.as_ref()
    }

    fn sync(&self, data_only: bool) -> VfsResult<()> {
        if let Some(upper_dir) = self.existing_upper_dir() {
            upper_dir.sync(data_only)?;
        }
        Ok(())
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE
    }
}

impl DirNodeOps for OverlayDir {
    /// Return the merged directory view.
    ///
    /// Lower layers are merged first from bottom to top, then upper entries
    /// override them. Whiteouts delete lower names, and opaque upper dirs skip
    /// lower merging entirely.
    fn read_dir(&self, offset: u64, sink: &mut dyn DirEntrySink) -> VfsResult<usize> {
        let mut entries = BTreeMap::new();
        let is_opaque = match self.existing_upper_dir() {
            Some(upper_dir) => is_opaque(&upper_dir)?,
            None => false,
        };
        if !is_opaque {
            for lower in self.lower_dirs.iter().rev() {
                read_names(lower, &mut entries)?;
            }
        }
        if let Some(upper_dir) = self.existing_upper_dir() {
            read_names(&upper_dir, &mut entries)?;
        }

        let mut emitted = 0;
        for (idx, (name, info)) in entries.into_iter().enumerate().skip(offset as usize) {
            if !sink.accept(&name, info.ino, info.node_type, idx as u64 + 1) {
                break;
            }
            emitted += 1;
        }
        Ok(emitted)
    }

    /// Lookup one merged child name.
    ///
    /// Unlike read_dir, lookup must explicitly distinguish upper whiteout from
    /// upper missing so path lookup cannot re-expose lower files hidden by
    /// unlink or opaque directory semantics.
    fn lookup(&self, name: &str) -> VfsResult<DirEntry> {
        match self.lookup_upper_child(name)? {
            UpperLookup::Present(upper) => self.build_entry(name, Some(upper), None),
            UpperLookup::Whiteout => Err(VfsError::NotFound),
            UpperLookup::Missing => {
                if let Some(upper_dir) = self.existing_upper_dir()
                    && is_opaque(&upper_dir)?
                {
                    return Err(VfsError::NotFound);
                }
                let lower = lookup_lower(&self.lower_dirs, name)?.ok_or(VfsError::NotFound)?;
                self.build_entry(name, None, Some(lower))
            }
        }
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
            .materialize_upper_dir()?
            .create(name, node_type, permission, uid, gid)?;
        self.build_entry(name, Some(upper), None)
    }

    /// Create a hard link by first ensuring the source lives in upper.
    fn link(&self, name: &str, node: &DirEntry) -> VfsResult<DirEntry> {
        self.ensure_no_visible_entry(name)?;
        self.remove_existing_whiteout(name)?;

        let target = node.downcast::<OverlayFile>()?.ensure_upper()?;
        let linked = self.materialize_upper_dir()?.link(name, &target)?;
        self.build_entry(name, Some(linked), None)
    }

    /// Unlink a visible upper entry and create a whiteout when a lower entry
    /// with the same name exists.
    fn unlink(&self, name: &str, _is_dir: bool) -> VfsResult<()> {
        if let Some(upper) = self.lookup_visible_upper_child(name)?
            && let Some(upper_dir) = self.existing_upper_dir()
        {
            upper_dir.unlink(name, upper.is_dir())?;
        }
        if lookup_lower(&self.lower_dirs, name)?.is_some() {
            create_whiteout(&self.materialize_upper_dir()?, name)?;
            return Ok(());
        }
        Ok(())
    }

    /// Rename overlay entries with conservative lower-backed directory rules.
    ///
    /// Lower-backed files are copied up before rename. Lower-backed
    /// directories are rejected because full redirect_dir/index semantics are
    /// not implemented.
    fn rename(&self, src_name: &str, dst_dir: &DirNode, dst_name: &str) -> VfsResult<()> {
        let dst = dst_dir.downcast::<Self>()?;
        let src = match self.lookup_visible_upper_child(src_name)? {
            Some(upper) => upper,
            None => {
                let lower = lookup_lower(&self.lower_dirs, src_name)?.ok_or(VfsError::NotFound)?;
                if lower.is_dir() {
                    return Err(VfsError::CrossesDevices);
                }
                ensure_upper_from_lower(&self.materialize_upper_dir()?, &lower, src_name)?
            }
        };
        dst.remove_existing_whiteout(dst_name)?;
        self.materialize_upper_dir()?
            .rename(src_name, &dst.materialize_upper_dir()?, dst_name)?;
        if lookup_lower(&self.lower_dirs, src_name)?.is_some() {
            create_whiteout(&self.materialize_upper_dir()?, src_name)?;
        }
        if src.is_dir()
            && let Some(moved) = dst.lookup_visible_upper_child(dst_name)?
        {
            mark_opaque(&moved)?;
        }
        Ok(())
    }
}

struct OverlayFile {
    fs: Arc<OverlayFs>,
    /// Materialized upper parent directory, if one exists.
    upper_dir: Mutex<Option<Location>>,
    /// Parent path from overlay root, used to materialize the upper parent.
    parent_path: Vec<String>,
    name: String,
    /// Upper file captured when the entry was built.
    upper: Option<Location>,
    /// Lower file captured when the entry was built.
    lower: Option<Location>,
}

impl OverlayFile {
    fn existing_upper_dir(&self) -> Option<Location> {
        self.upper_dir.lock().clone()
    }

    /// Ensure the parent directory for this file exists in upper.
    fn materialize_upper_dir(&self) -> VfsResult<Location> {
        if let Some(upper_dir) = self.existing_upper_dir() {
            return Ok(upper_dir);
        }

        let mut upper_dir = self
            .fs
            .upper_dir
            .clone()
            .ok_or(VfsError::ReadOnlyFilesystem)?;
        let mut lower_dirs = self.fs.lower_dirs.clone();
        for name in &self.parent_path {
            if let Some(existing) = lookup_visible_upper(&upper_dir, name)? {
                existing.check_is_dir()?;
                upper_dir = existing;
            } else {
                let lower = lookup_lower(&lower_dirs, name)?.ok_or(VfsError::NotFound)?;
                lower.check_is_dir()?;
                upper_dir = copy_entry(&lower, &upper_dir, name)?;
            }
            lower_dirs = OverlayDir::lower_dirs_for_child_in(&lower_dirs, name);
        }

        *self.upper_dir.lock() = Some(upper_dir.clone());
        Ok(upper_dir)
    }

    /// Return the currently visible backing file.
    fn current(&self) -> VfsResult<Location> {
        if let Some(upper_dir) = self.existing_upper_dir()
            && let Some(upper) = lookup_visible_upper(&upper_dir, &self.name)?
        {
            return Ok(upper);
        }
        self.lower.clone().ok_or(VfsError::NotFound)
    }

    /// Ensure this file has a writable upper backing file.
    fn ensure_upper(&self) -> VfsResult<Location> {
        if let Some(upper_dir) = self.existing_upper_dir()
            && let Some(upper) = lookup_visible_upper(&upper_dir, &self.name)?
        {
            return Ok(upper);
        }
        let lower = self.lower.as_ref().ok_or(VfsError::NotFound)?;
        ensure_upper_from_lower(&self.materialize_upper_dir()?, lower, &self.name)
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
