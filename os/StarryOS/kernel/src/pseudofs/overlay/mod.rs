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

mod inode;

use alloc::{
    collections::BTreeMap,
    string::{String, ToString},
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    any::Any,
    sync::atomic::{AtomicU32, Ordering},
};

use ax_fs_ng::vfs::OpenOptions;
use axfs_ng_vfs::{
    DeviceId, DirEntry, DirEntrySink, DirNode, DirNodeOps, DirectoryCursor, FileNode, FileNodeOps,
    FileRangeOperation, Filesystem, FilesystemOps, Location, Metadata, MetadataUpdate,
    MutationCredentials, NameState, NodeFlags, NodeIdentity, NodeOps, NodePermission, NodeType,
    Reference, RenameOptions, StatFs, VfsError, VfsResult, WeakDirEntry,
};
use axpoll::{IoEvents, Pollable};
use inode::{InodeIndex, OverlayInode};

use crate::{pseudofs::dummy_stat_fs, sync::RawSpinLock, task::FsCredentialsSnapshot};

const COPY_BUF_SIZE: usize = 4096;
const OVERLAY_MAGIC: u32 = 0x794c7630;
const WHITEOUT_DEVICE: DeviceId = DeviceId::new(0, 0);
const OPAQUE_MARKER_NAME: &str = ".wh..wh..opq";
static NEXT_OVERLAY_DEVICE: AtomicU32 = AtomicU32::new(1);

pub(crate) struct OverlayOptions {
    /// Lower layers ordered from topmost to bottommost.
    pub lower_dirs: Vec<Location>,
    /// Writable upper layer. `None` creates a read-only lower-only overlay.
    pub upper_dir: Option<Location>,
    /// Work directory required by the mount ABI when an upper layer exists.
    pub work_dir: Option<Location>,
    pub mounter: FsCredentialsSnapshot,
}

/// Build an overlay filesystem from resolved lower, upper, and work dirs.
pub(crate) fn new_overlayfs(options: OverlayOptions) -> VfsResult<Filesystem> {
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
    if let (Some(upper), Some(work)) = (&options.upper_dir, &options.work_dir) {
        if !Arc::ptr_eq(upper.mountpoint(), work.mountpoint()) {
            return Err(VfsError::CrossesDevices);
        }
        if upper.is_descendant_of(work) || work.is_descendant_of(upper) {
            return Err(VfsError::InvalidInput);
        }
    }
    for lower in &options.lower_dirs {
        lower.check_is_dir()?;
    }

    let write_roots: Vec<_> = options
        .upper_dir
        .iter()
        .chain(options.work_dir.iter())
        .cloned()
        .collect();
    let depth = options
        .lower_dirs
        .iter()
        .chain(write_roots.iter())
        .map(|root| root.filesystem().stack_depth())
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .filter(|depth| *depth <= 2)
        .ok_or(VfsError::InvalidInput)?;
    let device = NEXT_OVERLAY_DEVICE
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
            next.checked_add(1)
        })
        .map_err(|_| VfsError::NoMemory)?;
    let inodes = InodeIndex::new();
    let root = inodes.select(
        options.upper_dir.clone(),
        options.lower_dirs.clone(),
        options.upper_dir.is_some(),
    )?;
    let fs = Arc::new_cyclic(|this| OverlayFs {
        this: this.clone(),
        lower_dirs: options.lower_dirs,
        upper_dir: options.upper_dir,
        mounter: options.mounter,
        write_roots,
        depth,
        device: DeviceId::new(0, device).0,
        inodes,
        root,
        root_entry: RawSpinLock::new(None),
    });
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
    this: Weak<Self>,
    lower_dirs: Vec<Location>,
    upper_dir: Option<Location>,
    mounter: FsCredentialsSnapshot,
    write_roots: Vec<Location>,
    depth: u32,
    device: u64,
    inodes: InodeIndex,
    root: Arc<OverlayInode>,
    // root_dir() may be called from VFS mount paths with preemption disabled.
    root_entry: RawSpinLock<Option<WeakDirEntry>>,
}

impl OverlayFs {
    fn credentials(&self) -> MutationCredentials<'_> {
        self.mounter.as_mutation_credentials()
    }
}

impl FilesystemOps for OverlayFs {
    fn name(&self) -> &str {
        "overlay"
    }

    fn root_dir(&self) -> DirEntry {
        if let Some(root) = self
            .root_entry
            .lock_irqsave()
            .as_ref()
            .and_then(WeakDirEntry::upgrade)
        {
            return root;
        }
        // No backend callback or sleeping gate is needed to rebuild this alias.
        let candidate = OverlayDir::entry(
            self.this.upgrade().expect("live overlay receiver"),
            self.root.clone(),
            Reference::root(),
        );
        let mut root = self.root_entry.lock_irqsave();
        let selected = root
            .as_ref()
            .and_then(WeakDirEntry::upgrade)
            .unwrap_or_else(|| {
                *root = Some(candidate.downgrade());
                candidate.clone()
            });
        drop(root);
        drop(candidate);
        selected
    }

    fn is_readonly(&self) -> bool {
        self.upper_dir.is_none()
    }

    fn stack_depth(&self) -> u32 {
        self.depth
    }

    fn write_backing_roots(&self) -> &[Location] {
        &self.write_roots
    }

    fn read_backing_roots(&self) -> &[Location] {
        &self.lower_dirs
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
    match dir.lookup_backing(OPAQUE_MARKER_NAME) {
        Ok(_) => Ok(true),
        Err(VfsError::NotFound) => Ok(false),
        Err(err) => Err(err),
    }
}

/// Create the Linux overlayfs whiteout marker: char device with rdev 0:0.
fn create_whiteout(
    dir: &Location,
    name: &str,
    credentials: &MutationCredentials<'_>,
) -> VfsResult<()> {
    dir.create_initialized(
        name,
        axfs_ng_vfs::CreateAttributes {
            content: axfs_ng_vfs::CreateContent::Node(NodeType::CharacterDevice, WHITEOUT_DEVICE),
            permission: NodePermission::from_bits_truncate(0),
        },
        credentials,
    )?;
    Ok(())
}

/// Mark an upper directory as opaque by creating the `.wh..wh..opq` marker.
fn mark_opaque(dir: &Location, credentials: &MutationCredentials<'_>) -> VfsResult<()> {
    match dir.lookup_backing(OPAQUE_MARKER_NAME) {
        Ok(_) => Ok(()),
        Err(VfsError::NotFound) => {
            dir.create(
                OPAQUE_MARKER_NAME,
                NodeType::RegularFile,
                NodePermission::from_bits_truncate(0),
                0,
                0,
                credentials,
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
    match dir.lookup_backing(name) {
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
    match dir.lookup_backing(name) {
        Ok(loc) => Ok(Some(loc)),
        Err(VfsError::NotFound) => Ok(None),
        Err(err) => Err(err),
    }
}

/// Lookup lower layers from topmost to bottommost.
fn lookup_lower(dirs: &[Location], name: &str) -> VfsResult<Option<Location>> {
    for dir in dirs {
        match dir.lookup_backing(name) {
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
    dir.read_dir(
        DirectoryCursor::START,
        &mut |name: &[u8], ino, node_type, _| {
            let Ok(name) = core::str::from_utf8(name) else {
                return true;
            };
            if name == "." || name == ".." || name == OPAQUE_MARKER_NAME {
                return true;
            }
            let Ok(loc) = dir.lookup_backing(name) else {
                return true;
            };
            if is_whiteout(&loc).unwrap_or(false) {
                names.remove(name);
            } else {
                names.insert(name.to_string(), DirentInfo { ino, node_type });
            }
            true
        },
    )?;
    Ok(())
}

/// Copy regular file contents from lower to a newly-created upper file.
fn copy_file_contents(src: &Location, dst: &Location) -> VfsResult<()> {
    let src_file = open_read(src.clone())?;
    let dst_file = open_write(dst.clone())?;

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

fn open_read(loc: Location) -> VfsResult<ax_fs_ng::File> {
    OpenOptions::new().read(true).open_loc(loc)?.into_file()
}

fn open_write(loc: Location) -> VfsResult<ax_fs_ng::File> {
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
fn copy_entry(
    src: &Location,
    dst_dir: &Location,
    name: &str,
    credentials: &MutationCredentials<'_>,
) -> VfsResult<Location> {
    let meta = src.metadata()?;
    let dst = match meta.node_type {
        NodeType::Symlink => {
            let target = src.read_link()?;
            dst_dir.create_symlink(name, &target, meta.mode, meta.uid, meta.gid, credentials)?
        }
        _ => dst_dir.create(
            name,
            meta.node_type,
            meta.mode,
            meta.uid,
            meta.gid,
            credentials,
        )?,
    };
    match meta.node_type {
        NodeType::RegularFile => copy_file_contents(src, &dst)?,
        NodeType::Symlink => {}
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
    credentials: &MutationCredentials<'_>,
) -> VfsResult<Location> {
    if let Some(upper) = lookup_visible_upper(upper_dir, name)? {
        return Ok(upper);
    }
    copy_entry(lower, upper_dir, name, credentials)
}

#[derive(Clone)]
struct DirentInfo {
    ino: u64,
    node_type: NodeType,
}

struct OverlayDir {
    fs: Arc<OverlayFs>,
    inode: Arc<OverlayInode>,
    this: WeakDirEntry,
}

impl OverlayDir {
    fn entry(fs: Arc<OverlayFs>, inode: Arc<OverlayInode>, reference: Reference) -> DirEntry {
        DirEntry::new_dir(
            |this| DirNode::new(Arc::new(Self { fs, inode, this })),
            reference,
        )
    }

    fn child_reference(&self, name: &str) -> Reference {
        Reference::new(self.this.upgrade(), name.to_string())
    }

    /// Collect lower child directories that should be visible below `name`.
    ///
    /// The list is used when constructing an overlay child directory. A lower
    /// whiteout stops the search because it hides lower layers beneath it.
    fn lower_dirs_for_child_in(dirs: &[Location], name: &str) -> Vec<Location> {
        let mut result = Vec::new();
        for lower_dir in dirs {
            if let Ok(child) = lower_dir.lookup_backing(name) {
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
        Self::lower_dirs_for_child_in(&self.inode.lower, name)
    }

    fn existing_upper_dir(&self) -> Option<Location> {
        self.inode.upper()
    }

    /// Use the live alias relationship; a renamed parent keeps its entity.
    fn materialize_upper_dir(&self) -> VfsResult<Location> {
        if let Some(upper) = self.existing_upper_dir() {
            return Ok(upper);
        }
        let entry = self.this.upgrade().ok_or(VfsError::NotFound)?;
        let snapshot = entry.name_snapshot();
        if snapshot.state() != NameState::Linked {
            return Err(VfsError::OperationNotSupported);
        }
        let parent = snapshot
            .parent()
            .ok_or(VfsError::BadState)?
            .downcast::<Self>()?;
        let lower = self.inode.lower.first().ok_or(VfsError::NotFound)?;
        let upper = ensure_upper_from_lower(
            &parent.materialize_upper_dir()?,
            lower,
            snapshot.name(),
            &self.fs.credentials(),
        )?;
        self.fs.inodes.bind_upper(&self.inode, &upper);
        drop(self.inode.replace_upper(upper.clone()));
        Ok(upper)
    }

    fn current_dir(&self) -> VfsResult<Location> {
        self.inode.current()
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
        let lower = if source.node_type() == NodeType::Directory {
            self.lower_dirs_for_child(name)
        } else {
            lower.into_iter().collect()
        };
        let inode = self
            .fs
            .inodes
            .select(upper, lower, self.fs.upper_dir.is_some())?;
        let reference = self.child_reference(name);
        if inode.kind == NodeType::Directory {
            Ok(Self::entry(self.fs.clone(), inode, reference))
        } else {
            let file = Arc::new(OverlayFile {
                fs: self.fs.clone(),
                inode,
                this: RawSpinLock::new(None),
            });
            let entry = DirEntry::new_file(FileNode::new(file.clone()), file.inode.kind, reference);
            *file.this.lock_irqsave() = Some(entry.downgrade());
            Ok(entry)
        }
    }

    fn ensure_no_visible_entry(&self, name: &str) -> VfsResult<()> {
        match self.lookup_upper_child(name)? {
            UpperLookup::Present(_) => Err(VfsError::AlreadyExists),
            UpperLookup::Whiteout => Ok(()),
            UpperLookup::Missing => {
                if lookup_lower(&self.inode.lower, name)?.is_some() {
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
            upper_dir.unlink(name, upper.is_dir(), &self.fs.credentials())?;
        }
        Ok(())
    }
}

impl NodeOps for OverlayDir {
    fn inode(&self) -> u64 {
        self.inode.number
    }

    fn identity(&self) -> Option<NodeIdentity> {
        Some(self.inode.identity(self.fs.as_ref()))
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        self.inode.metadata(self.fs.device)
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
    fn map_extents(
        &self,
        offset: u64,
        len: u64,
        target: axfs_ng_vfs::FileExtentTarget,
        extent_limit: usize,
    ) -> VfsResult<axfs_ng_vfs::FileExtentMap> {
        self.current_dir()?
            .entry()
            .as_dir()?
            .inner()
            .map_extents(offset, len, target, extent_limit)
    }

    /// Return the merged directory view.
    ///
    /// Lower layers are merged first from bottom to top, then upper entries
    /// override them. Whiteouts delete lower names, and opaque upper dirs skip
    /// lower merging entirely.
    fn read_dir(&self, cursor: DirectoryCursor, sink: &mut dyn DirEntrySink) -> VfsResult<usize> {
        let mut entries = BTreeMap::new();
        let is_opaque = match self.existing_upper_dir() {
            Some(upper_dir) => is_opaque(&upper_dir)?,
            None => false,
        };
        if !is_opaque {
            for lower in self.inode.lower.iter().rev() {
                read_names(lower, &mut entries)?;
            }
        }
        if let Some(upper_dir) = self.existing_upper_dir() {
            read_names(&upper_dir, &mut entries)?;
        }

        let mut emitted = 0;
        for (idx, (name, info)) in entries
            .into_iter()
            .enumerate()
            .skip(cursor.offset() as usize)
        {
            if !sink.accept(
                name.as_bytes(),
                info.ino,
                info.node_type,
                DirectoryCursor::new(idx as u64 + 1),
            ) {
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
                let lower = lookup_lower(&self.inode.lower, name)?.ok_or(VfsError::NotFound)?;
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
        if node_type == NodeType::Symlink {
            return Err(VfsError::InvalidInput);
        }
        self.ensure_no_visible_entry(name)?;
        self.remove_existing_whiteout(name)?;
        let upper = self.materialize_upper_dir()?.create(
            name,
            node_type,
            permission,
            uid,
            gid,
            &self.fs.credentials(),
        )?;
        self.build_entry(name, Some(upper), None)
    }

    fn create_symlink(
        &self,
        name: &str,
        target: &str,
        permission: NodePermission,
        uid: u32,
        gid: u32,
    ) -> VfsResult<DirEntry> {
        self.ensure_no_visible_entry(name)?;
        self.remove_existing_whiteout(name)?;
        let upper = self.materialize_upper_dir()?.create_symlink(
            name,
            target,
            permission,
            uid,
            gid,
            &self.fs.credentials(),
        )?;
        self.build_entry(name, Some(upper), None)
    }

    /// Create a hard link by first ensuring the source lives in upper.
    fn link(&self, name: &str, node: &DirEntry) -> VfsResult<DirEntry> {
        self.ensure_no_visible_entry(name)?;
        self.remove_existing_whiteout(name)?;

        let target = node.downcast::<OverlayFile>()?.ensure_upper()?;
        let linked = self
            .materialize_upper_dir()?
            .link(name, &target, &self.fs.credentials())?;
        self.build_entry(name, Some(linked), None)
    }

    /// Unlink a visible upper entry and create a whiteout when a lower entry
    /// with the same name exists.
    fn unlink(&self, name: &str, _is_dir: bool) -> VfsResult<()> {
        if let Some(upper) = self.lookup_visible_upper_child(name)?
            && let Some(upper_dir) = self.existing_upper_dir()
        {
            upper_dir.unlink(name, upper.is_dir(), &self.fs.credentials())?;
        }
        if lookup_lower(&self.inode.lower, name)?.is_some() {
            create_whiteout(&self.materialize_upper_dir()?, name, &self.fs.credentials())?;
            return Ok(());
        }
        Ok(())
    }

    /// Rename overlay entries with conservative lower-backed directory rules.
    ///
    /// Lower-backed files are copied up before rename. Lower-backed
    /// directories are rejected because full redirect_dir/index semantics are
    /// not implemented.
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
        let dst = dst_dir.downcast::<Self>()?;
        if options.no_replace() {
            match dst.lookup(dst_name) {
                Ok(_) => return Err(VfsError::AlreadyExists),
                Err(VfsError::NotFound) => {}
                Err(error) => return Err(error),
            }
        }
        let src = match self.lookup_visible_upper_child(src_name)? {
            Some(upper) => upper,
            None => {
                let lower = lookup_lower(&self.inode.lower, src_name)?.ok_or(VfsError::NotFound)?;
                if lower.is_dir() {
                    return Err(VfsError::CrossesDevices);
                }
                ensure_upper_from_lower(
                    &self.materialize_upper_dir()?,
                    &lower,
                    src_name,
                    &self.fs.credentials(),
                )?
            }
        };
        dst.remove_existing_whiteout(dst_name)?;
        self.materialize_upper_dir()?.rename_with_options(
            src_name,
            &dst.materialize_upper_dir()?,
            dst_name,
            options,
            &self.fs.credentials(),
        )?;
        if lookup_lower(&self.inode.lower, src_name)?.is_some() {
            create_whiteout(
                &self.materialize_upper_dir()?,
                src_name,
                &self.fs.credentials(),
            )?;
        }
        if src.is_dir()
            && let Some(moved) = dst.lookup_visible_upper_child(dst_name)?
        {
            mark_opaque(&moved, &self.fs.credentials())?;
        }
        Ok(())
    }
}

struct OverlayFile {
    fs: Arc<OverlayFs>,
    inode: Arc<OverlayInode>,
    this: RawSpinLock<Option<WeakDirEntry>>,
}

impl OverlayFile {
    fn current(&self) -> VfsResult<Location> {
        self.inode.current()
    }

    fn ensure_upper(&self) -> VfsResult<Location> {
        if let Some(upper) = self.inode.upper() {
            return Ok(upper);
        }
        let entry = self
            .this
            .lock_irqsave()
            .as_ref()
            .and_then(WeakDirEntry::upgrade)
            .ok_or(VfsError::NotFound)?;
        let snapshot = entry.name_snapshot();
        if snapshot.state() != NameState::Linked {
            return Err(VfsError::OperationNotSupported);
        }
        let parent = snapshot
            .parent()
            .ok_or(VfsError::BadState)?
            .downcast::<OverlayDir>()?;
        let lower = self.inode.lower.first().ok_or(VfsError::NotFound)?;
        let upper = ensure_upper_from_lower(
            &parent.materialize_upper_dir()?,
            lower,
            snapshot.name(),
            &self.fs.credentials(),
        )?;
        self.fs.inodes.bind_upper(&self.inode, &upper);
        drop(self.inode.replace_upper(upper.clone()));
        Ok(upper)
    }
}

impl NodeOps for OverlayFile {
    fn inode(&self) -> u64 {
        self.inode.number
    }

    fn identity(&self) -> Option<NodeIdentity> {
        Some(self.inode.identity(self.fs.as_ref()))
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        self.inode.metadata(self.fs.device)
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
            (loc.flags() & !NodeFlags::ALWAYS_CACHE) | NodeFlags::NON_CACHEABLE
        })
    }
}

impl FileNodeOps for OverlayFile {
    fn resolve_file_io(
        &self,
        request: axfs_ng_vfs::FileIoRequest<'_>,
    ) -> VfsResult<axfs_ng_vfs::FileIoTarget> {
        if !core::ptr::addr_eq(request.location().filesystem(), self.fs.as_ref())
            || request.location().inode() != self.inode.number
        {
            return Err(VfsError::BadState);
        }
        let target = if request.is_write() {
            self.inode.upper().ok_or(VfsError::BadState)?
        } else {
            self.inode.current()?
        };
        request.forward_to(target, &self.fs.credentials())
    }

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

    fn operate_range(&self, offset: u64, len: u64, operation: FileRangeOperation) -> VfsResult<()> {
        open_write(self.ensure_upper()?)?
            .backend()?
            .operate_range(offset, len, operation)
    }

    fn map_extents(
        &self,
        offset: u64,
        len: u64,
        target: axfs_ng_vfs::FileExtentTarget,
        extent_limit: usize,
    ) -> VfsResult<axfs_ng_vfs::FileExtentMap> {
        self.current()?
            .entry()
            .as_file()?
            .map_extents(offset, len, target, extent_limit)
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

    unsafe fn register_shared(
        &self,
        sink: &mut dyn axpoll::SharedRegistrationSink,
        events: IoEvents,
    ) {
        if let Ok(loc) = self.current() {
            unsafe { loc.entry().register_shared(sink, events) };
        }
    }

    unsafe fn register_exclusive(
        &self,
        sink: &mut dyn axpoll::ExclusiveRegistrationSink,
        events: IoEvents,
    ) {
        if let Ok(loc) = self.current() {
            unsafe { loc.entry().register_exclusive(sink, events) };
        }
    }
}
