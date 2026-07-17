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

use ax_fs_ng::vfs::{FileLocation, LocationOperationView};
use ax_sync::SpinMutex;
use axfs_ng_vfs::{
    DeviceId, DirEntry, DirEntrySink, DirNode, DirNodeOps, FileNode, FileNodeOps, Filesystem,
    FilesystemOps, FsIoEvents, FsPollable, Metadata, MetadataUpdate, NodeFlags, NodeOps,
    NodePermission, NodeType, Reference, StatFs, VfsError, VfsResult, WeakDirEntry,
};

use crate::pseudofs::dummy_stat_fs;

const COPY_BUF_SIZE: usize = 4096;
const OVERLAY_MAGIC: u32 = 0x794c7630;
const WHITEOUT_DEVICE: DeviceId = DeviceId::new(0, 0);
const OPAQUE_MARKER_NAME: &str = ".wh..wh..opq";

#[derive(Clone)]
pub struct OverlayOptions {
    /// Lower layers ordered from topmost to bottommost.
    pub lower_dirs: Vec<FileLocation>,
    /// Writable upper layer. `None` creates a read-only lower-only overlay.
    pub upper_dir: Option<FileLocation>,
    /// Work directory required by the mount ABI when an upper layer exists.
    pub work_dir: Option<FileLocation>,
}

#[derive(Clone)]
struct OverlayLocation(FileLocation);

impl From<FileLocation> for OverlayLocation {
    fn from(location: FileLocation) -> Self {
        Self(location)
    }
}

impl OverlayLocation {
    fn authorize<'operation>(
        &'operation self,
        authority: &'operation LocationOperationView<'_>,
    ) -> VfsResult<LocationOperationView<'operation>> {
        authority.authorize_location(&self.0)
    }

    fn node_type(&self, authority: &LocationOperationView<'_>) -> VfsResult<NodeType> {
        Ok(self.authorize(authority)?.node_type())
    }

    fn metadata(&self, authority: &LocationOperationView<'_>) -> VfsResult<Metadata> {
        self.authorize(authority)?.metadata()
    }

    fn check_is_dir(&self, authority: &LocationOperationView<'_>) -> VfsResult<()> {
        self.authorize(authority)?.check_is_dir()
    }

    fn is_dir(&self, authority: &LocationOperationView<'_>) -> VfsResult<bool> {
        Ok(self.authorize(authority)?.is_dir())
    }

    fn inode(&self, authority: &LocationOperationView<'_>) -> VfsResult<u64> {
        Ok(self.authorize(authority)?.inode())
    }

    fn flags(&self, authority: &LocationOperationView<'_>) -> VfsResult<NodeFlags> {
        Ok(self.authorize(authority)?.node_flags())
    }

    fn len(&self, authority: &LocationOperationView<'_>) -> VfsResult<u64> {
        self.authorize(authority)?.len()
    }

    fn update_metadata(
        &self,
        authority: &LocationOperationView<'_>,
        update: MetadataUpdate,
    ) -> VfsResult<()> {
        self.authorize(authority)?.update_metadata(update)
    }

    fn sync(&self, authority: &LocationOperationView<'_>, data_only: bool) -> VfsResult<()> {
        self.authorize(authority)?.sync(data_only)
    }

    fn lookup_no_follow(
        &self,
        authority: &LocationOperationView<'_>,
        name: &str,
    ) -> VfsResult<Self> {
        self.authorize(authority)?
            .lookup_no_follow(name)?
            .retain()
            .map(Self)
    }

    fn create(
        &self,
        authority: &LocationOperationView<'_>,
        name: &str,
        node_type: NodeType,
        permission: NodePermission,
        uid: u32,
        gid: u32,
    ) -> VfsResult<Self> {
        self.authorize(authority)?
            .create(name, node_type, permission, uid, gid)?
            .retain()
            .map(Self)
    }

    fn link(
        &self,
        authority: &LocationOperationView<'_>,
        name: &str,
        source: &Self,
    ) -> VfsResult<Self> {
        let directory = self.authorize(authority)?;
        let source = source.authorize(authority)?;
        directory.link_child(name, &source)?.retain().map(Self)
    }

    fn rename(
        &self,
        authority: &LocationOperationView<'_>,
        source_name: &str,
        destination: &Self,
        destination_name: &str,
    ) -> VfsResult<()> {
        let source = self.authorize(authority)?;
        let destination = destination.authorize(authority)?;
        source.rename(source_name, &destination, destination_name)
    }

    fn unlink(
        &self,
        authority: &LocationOperationView<'_>,
        name: &str,
        is_dir: bool,
    ) -> VfsResult<()> {
        self.authorize(authority)?.unlink(name, is_dir)
    }

    fn read_dir(
        &self,
        authority: &LocationOperationView<'_>,
        sink: &mut dyn DirEntrySink,
    ) -> VfsResult<usize> {
        self.authorize(authority)?.read_dir(0, sink)
    }

    fn read_at(
        &self,
        authority: &LocationOperationView<'_>,
        buffer: &mut [u8],
        offset: u64,
    ) -> VfsResult<usize> {
        self.authorize(authority)?.read_at(buffer, offset)
    }

    fn write_at(
        &self,
        authority: &LocationOperationView<'_>,
        buffer: &[u8],
        offset: u64,
    ) -> VfsResult<usize> {
        self.authorize(authority)?.write_at(buffer, offset)
    }

    fn append(
        &self,
        authority: &LocationOperationView<'_>,
        buffer: &[u8],
    ) -> VfsResult<(usize, u64)> {
        self.authorize(authority)?.append(buffer)
    }

    fn set_len(&self, authority: &LocationOperationView<'_>, len: u64) -> VfsResult<()> {
        self.authorize(authority)?.set_len(len)
    }

    fn read_link(&self, authority: &LocationOperationView<'_>) -> VfsResult<String> {
        self.authorize(authority)?.read_link()
    }

    fn set_symlink(&self, authority: &LocationOperationView<'_>, target: &str) -> VfsResult<()> {
        self.authorize(authority)?.set_symlink(target)
    }

    fn ioctl(
        &self,
        authority: &LocationOperationView<'_>,
        command: u32,
        argument: usize,
    ) -> VfsResult<usize> {
        self.authorize(authority)?.ioctl(command, argument)
    }

    fn poll(&self, authority: &LocationOperationView<'_>) -> VfsResult<FsIoEvents> {
        Ok(self.authorize(authority)?.poll())
    }

    fn register(
        &self,
        authority: &LocationOperationView<'_>,
        context: &mut Context<'_>,
        events: FsIoEvents,
    ) -> VfsResult<()> {
        self.authorize(authority)?.register(context, events);
        Ok(())
    }

    fn get_user_data<T>(&self, authority: &LocationOperationView<'_>) -> VfsResult<Option<Arc<T>>>
    where
        T: Any + Send + Sync,
    {
        Ok(self.authorize(authority)?.get_user_data::<T>())
    }

    fn get_or_insert_user_data<T>(&self, authority: &LocationOperationView<'_>) -> VfsResult<Arc<T>>
    where
        T: Any + Default + Send + Sync,
    {
        Ok(self
            .authorize(authority)?
            .get_or_insert_user_data_with(T::default))
    }
}

/// Build an overlay filesystem from resolved lower, upper, and work dirs.
pub fn new_overlayfs(
    authority: &LocationOperationView<'_>,
    options: OverlayOptions,
) -> VfsResult<Filesystem> {
    if options.lower_dirs.is_empty() {
        return Err(VfsError::InvalidInput);
    }
    if options.upper_dir.is_some() != options.work_dir.is_some() {
        return Err(VfsError::InvalidInput);
    }
    let lower_dirs = options
        .lower_dirs
        .into_iter()
        .map(OverlayLocation::from)
        .collect::<Vec<_>>();
    let upper_dir = options.upper_dir.map(OverlayLocation::from);
    let work_dir = options.work_dir.map(OverlayLocation::from);
    if let Some(upper_dir) = &upper_dir {
        upper_dir.check_is_dir(authority)?;
    }
    if let Some(work_dir) = &work_dir {
        work_dir.check_is_dir(authority)?;
    }
    for lower in &lower_dirs {
        lower.check_is_dir(authority)?;
    }

    let fs = Arc::new(OverlayFs {
        authority: lower_dirs[0].clone(),
        lower_dirs,
        upper_dir,
        _work_dir: work_dir,
        root: SpinMutex::new(None),
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

/// Clones typed data from the real node currently visible through an admitted
/// overlay operation.
pub(crate) fn visible_user_data<T>(view: &LocationOperationView<'_>) -> VfsResult<Option<Arc<T>>>
where
    T: Any + Send + Sync,
{
    if let Ok(data) =
        view.with_node::<OverlayFile, _>(|file| file.current(view)?.get_user_data::<T>(view))
    {
        return Ok(data);
    }
    if let Ok(data) = view
        .with_node::<OverlayDir, _>(|directory| directory.current_dir()?.get_user_data::<T>(view))
    {
        return Ok(data);
    }
    Ok(view.get_user_data::<T>())
}

/// Returns typed data attached to the writable real node, materializing an
/// overlay upper node when required.
pub(crate) fn writable_user_data<T>(view: &LocationOperationView<'_>) -> VfsResult<Arc<T>>
where
    T: Any + Default + Send + Sync,
{
    if let Ok(data) = view.with_node::<OverlayFile, _>(|file| {
        file.ensure_upper(view)?.get_or_insert_user_data::<T>(view)
    }) {
        return Ok(data);
    }
    if let Ok(data) = view.with_node::<OverlayDir, _>(|directory| {
        directory
            .materialize_upper_dir(view)?
            .get_or_insert_user_data::<T>(view)
    }) {
        return Ok(data);
    }
    Ok(view.get_or_insert_user_data_with(T::default))
}

struct OverlayFs {
    authority: OverlayLocation,
    lower_dirs: Vec<OverlayLocation>,
    upper_dir: Option<OverlayLocation>,
    _work_dir: Option<OverlayLocation>,
    root: SpinMutex<Option<DirEntry>>,
}

impl OverlayFs {
    fn with_generation<T>(
        &self,
        operation: impl for<'operation> FnOnce(&LocationOperationView<'operation>) -> VfsResult<T>,
    ) -> VfsResult<T> {
        self.authority
            .0
            .with_operation(|authority| operation(&authority))
    }
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

fn is_whiteout(operation: &LocationOperationView<'_>, loc: &OverlayLocation) -> VfsResult<bool> {
    if loc.node_type(operation)? != NodeType::CharacterDevice {
        return Ok(false);
    }
    Ok(loc.metadata(operation)?.rdev == WHITEOUT_DEVICE)
}

/// Check whether an upper directory hides all lower entries under the same dir.
fn is_opaque(operation: &LocationOperationView<'_>, dir: &OverlayLocation) -> VfsResult<bool> {
    match dir.lookup_no_follow(operation, OPAQUE_MARKER_NAME) {
        Ok(_) => Ok(true),
        Err(VfsError::NotFound) => Ok(false),
        Err(err) => Err(err),
    }
}

/// Create the Linux overlayfs whiteout marker: char device with rdev 0:0.
fn create_whiteout(
    operation: &LocationOperationView<'_>,
    dir: &OverlayLocation,
    name: &str,
) -> VfsResult<()> {
    let whiteout = dir.create(
        operation,
        name,
        NodeType::CharacterDevice,
        NodePermission::from_bits_truncate(0),
        0,
        0,
    )?;
    whiteout.update_metadata(
        operation,
        MetadataUpdate {
            rdev: Some(WHITEOUT_DEVICE),
            ..Default::default()
        },
    )
}

/// Mark an upper directory as opaque by creating the `.wh..wh..opq` marker.
fn mark_opaque(operation: &LocationOperationView<'_>, dir: &OverlayLocation) -> VfsResult<()> {
    match dir.lookup_no_follow(operation, OPAQUE_MARKER_NAME) {
        Ok(_) => Ok(()),
        Err(VfsError::NotFound) => {
            dir.create(
                operation,
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
    Present(OverlayLocation),
    /// A whiteout exists and must hide any lower entry with the same name.
    Whiteout,
    /// Upper has no entry for this name.
    Missing,
}

/// Lookup in upper without collapsing whiteout and missing into one state.
fn lookup_upper(
    operation: &LocationOperationView<'_>,
    dir: &OverlayLocation,
    name: &str,
) -> VfsResult<UpperLookup> {
    if name == OPAQUE_MARKER_NAME {
        return Ok(UpperLookup::Whiteout);
    }
    match dir.lookup_no_follow(operation, name) {
        Ok(loc) if is_whiteout(operation, &loc)? => Ok(UpperLookup::Whiteout),
        Ok(loc) => Ok(UpperLookup::Present(loc)),
        Err(VfsError::NotFound) => Ok(UpperLookup::Missing),
        Err(err) => Err(err),
    }
}

/// Lookup a visible upper entry, hiding whiteouts from callers that only need
/// a normal entry or no entry.
fn lookup_visible_upper(
    operation: &LocationOperationView<'_>,
    dir: &OverlayLocation,
    name: &str,
) -> VfsResult<Option<OverlayLocation>> {
    match lookup_upper(operation, dir, name)? {
        UpperLookup::Present(loc) => Ok(Some(loc)),
        UpperLookup::Whiteout | UpperLookup::Missing => Ok(None),
    }
}

/// Lookup raw upper entries, including whiteout markers.
fn lookup_any_upper(
    operation: &LocationOperationView<'_>,
    dir: &OverlayLocation,
    name: &str,
) -> VfsResult<Option<OverlayLocation>> {
    match dir.lookup_no_follow(operation, name) {
        Ok(loc) => Ok(Some(loc)),
        Err(VfsError::NotFound) => Ok(None),
        Err(err) => Err(err),
    }
}

/// Lookup lower layers from topmost to bottommost.
fn lookup_lower(
    operation: &LocationOperationView<'_>,
    dirs: &[OverlayLocation],
    name: &str,
) -> VfsResult<Option<OverlayLocation>> {
    for dir in dirs {
        match dir.lookup_no_follow(operation, name) {
            Ok(loc) if is_whiteout(operation, &loc)? => return Ok(None),
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
fn read_names(
    operation: &LocationOperationView<'_>,
    dir: &OverlayLocation,
    names: &mut BTreeMap<String, DirentInfo>,
) -> VfsResult<()> {
    dir.read_dir(operation, &mut |name: &str, ino, node_type, _| {
        if name == "." || name == ".." || name == OPAQUE_MARKER_NAME {
            return true;
        }
        let Ok(loc) = dir.lookup_no_follow(operation, name) else {
            return true;
        };
        if is_whiteout(operation, &loc).unwrap_or(false) {
            names.remove(name);
        } else {
            names.insert(name.to_string(), DirentInfo { ino, node_type });
        }
        true
    })?;
    Ok(())
}

/// Copy regular file contents from lower to a newly-created upper file.
fn copy_file_contents(
    operation: &LocationOperationView<'_>,
    src: &OverlayLocation,
    dst: &OverlayLocation,
) -> VfsResult<()> {
    let mut offset = 0;
    let mut buf = [0u8; COPY_BUF_SIZE];
    loop {
        let read = src.read_at(operation, &mut buf[..], offset)?;
        if read == 0 {
            break;
        }
        let mut written = 0;
        while written < read {
            let n = dst.write_at(operation, &buf[written..read], offset + written as u64)?;
            if n == 0 {
                return Err(VfsError::InvalidData);
            }
            written += n;
        }
        offset += read as u64;
    }
    dst.set_len(operation, src.len(operation)?)?;
    Ok(())
}

/// Copy metadata that should survive copy-up.
fn copy_metadata(
    operation: &LocationOperationView<'_>,
    src: &OverlayLocation,
    dst: &OverlayLocation,
) -> VfsResult<()> {
    let meta = src.metadata(operation)?;
    dst.update_metadata(
        operation,
        MetadataUpdate {
            mode: Some(meta.mode),
            owner: Some((meta.uid, meta.gid)),
            rdev: Some(meta.rdev),
            atime: Some(meta.atime),
            mtime: Some(meta.mtime),
        },
    )
}

/// Copy a lower entry into an upper directory.
fn copy_entry(
    operation: &LocationOperationView<'_>,
    src: &OverlayLocation,
    dst_dir: &OverlayLocation,
    name: &str,
) -> VfsResult<OverlayLocation> {
    let meta = src.metadata(operation)?;
    let dst = dst_dir.create(
        operation,
        name,
        meta.node_type,
        meta.mode,
        meta.uid,
        meta.gid,
    )?;
    match meta.node_type {
        NodeType::RegularFile => copy_file_contents(operation, src, &dst)?,
        NodeType::Symlink => dst.set_symlink(operation, &src.read_link(operation)?)?,
        NodeType::Directory => {}
        _ => {}
    }
    copy_metadata(operation, src, &dst)?;
    Ok(dst)
}

/// Return an existing upper entry or copy the lower entry up.
fn ensure_upper_from_lower(
    operation: &LocationOperationView<'_>,
    upper_dir: &OverlayLocation,
    lower: &OverlayLocation,
    name: &str,
) -> VfsResult<OverlayLocation> {
    if let Some(upper) = lookup_visible_upper(operation, upper_dir, name)? {
        return Ok(upper);
    }
    copy_entry(operation, lower, upper_dir, name)
}

#[derive(Clone)]
struct DirentInfo {
    ino: u64,
    node_type: NodeType,
}

struct OverlayDir {
    fs: Arc<OverlayFs>,
    /// Materialized upper directory for this overlay path, if it exists.
    upper_dir: SpinMutex<Option<OverlayLocation>>,
    /// Lower directories that still participate in this overlay directory.
    lower_dirs: Vec<OverlayLocation>,
    /// Path from overlay root to this directory, used for deferred copy-up.
    path: Vec<String>,
    this: Option<WeakDirEntry>,
}

impl OverlayDir {
    /// Build an overlay directory entry with the corresponding upper/lower set.
    fn entry(
        fs: Arc<OverlayFs>,
        upper_dir: Option<OverlayLocation>,
        lower_dirs: Vec<OverlayLocation>,
        path: Vec<String>,
        parent: Option<DirEntry>,
    ) -> DirEntry {
        DirEntry::new_dir(
            |this| {
                DirNode::new(Arc::new(Self {
                    fs,
                    upper_dir: SpinMutex::new(upper_dir),
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
    fn lower_dirs_for_child_in(
        operation: &LocationOperationView<'_>,
        dirs: &[OverlayLocation],
        name: &str,
    ) -> Vec<OverlayLocation> {
        let mut result = Vec::new();
        for lower_dir in dirs {
            if let Ok(child) = lower_dir.lookup_no_follow(operation, name) {
                if is_whiteout(operation, &child).unwrap_or(false) {
                    break;
                }
                if child
                    .node_type(operation)
                    .is_ok_and(|node_type| node_type == NodeType::Directory)
                {
                    result.push(child);
                }
            }
        }
        result
    }

    fn lower_dirs_for_child(
        &self,
        operation: &LocationOperationView<'_>,
        name: &str,
    ) -> Vec<OverlayLocation> {
        Self::lower_dirs_for_child_in(operation, &self.lower_dirs, name)
    }

    fn child_path(&self, name: &str) -> Vec<String> {
        let mut path = self.path.clone();
        path.push(name.to_string());
        path
    }

    fn existing_upper_dir(&self) -> Option<OverlayLocation> {
        self.upper_dir.lock().clone()
    }

    /// Ensure this overlay directory has a real upper directory.
    ///
    /// Lower-only lookups should not create upper state. This is called only by
    /// operations that must write into upper or need an upper parent for a
    /// copied-up child.
    fn materialize_upper_dir(
        &self,
        operation: &LocationOperationView<'_>,
    ) -> VfsResult<OverlayLocation> {
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
            if let Some(existing) = lookup_visible_upper(operation, &upper_dir, name)? {
                existing.check_is_dir(operation)?;
                upper_dir = existing;
            } else {
                let lower =
                    lookup_lower(operation, &lower_dirs, name)?.ok_or(VfsError::NotFound)?;
                lower.check_is_dir(operation)?;
                upper_dir = copy_entry(operation, &lower, &upper_dir, name)?;
            }
            lower_dirs = Self::lower_dirs_for_child_in(operation, &lower_dirs, name);
        }

        *self.upper_dir.lock() = Some(upper_dir.clone());
        Ok(upper_dir)
    }

    fn current_dir(&self) -> VfsResult<OverlayLocation> {
        self.existing_upper_dir()
            .or_else(|| self.lower_dirs.first().cloned())
            .ok_or(VfsError::NotFound)
    }

    fn lookup_visible_upper_child(
        &self,
        operation: &LocationOperationView<'_>,
        name: &str,
    ) -> VfsResult<Option<OverlayLocation>> {
        match self.existing_upper_dir() {
            Some(upper_dir) => lookup_visible_upper(operation, &upper_dir, name),
            None => Ok(None),
        }
    }

    fn lookup_upper_child(
        &self,
        operation: &LocationOperationView<'_>,
        name: &str,
    ) -> VfsResult<UpperLookup> {
        match self.existing_upper_dir() {
            Some(upper_dir) => lookup_upper(operation, &upper_dir, name),
            None => Ok(UpperLookup::Missing),
        }
    }

    fn lookup_any_upper_child(
        &self,
        operation: &LocationOperationView<'_>,
        name: &str,
    ) -> VfsResult<Option<OverlayLocation>> {
        match self.existing_upper_dir() {
            Some(upper_dir) => lookup_any_upper(operation, &upper_dir, name),
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
        operation: &LocationOperationView<'_>,
        name: &str,
        upper: Option<OverlayLocation>,
        lower: Option<OverlayLocation>,
    ) -> VfsResult<DirEntry> {
        let source = upper
            .as_ref()
            .or(lower.as_ref())
            .ok_or(VfsError::NotFound)?;
        let node_type = source.node_type(operation)?;
        let reference = self.child_reference(name);
        if node_type == NodeType::Directory {
            if let Some(upper) = &upper {
                upper.check_is_dir(operation)?;
            }
            let lower_dirs = self.lower_dirs_for_child(operation, name);
            let path = self.child_path(name);
            let fs = self.fs.clone();
            Ok(DirEntry::new_dir(
                |this| {
                    DirNode::new(Arc::new(Self {
                        fs,
                        upper_dir: SpinMutex::new(upper),
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
                    upper_dir: SpinMutex::new(self.existing_upper_dir()),
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

    fn ensure_no_visible_entry(
        &self,
        operation: &LocationOperationView<'_>,
        name: &str,
    ) -> VfsResult<()> {
        match self.lookup_upper_child(operation, name)? {
            UpperLookup::Present(_) => Err(VfsError::AlreadyExists),
            UpperLookup::Whiteout => Ok(()),
            UpperLookup::Missing => {
                if lookup_lower(operation, &self.lower_dirs, name)?.is_some() {
                    return Err(VfsError::AlreadyExists);
                }
                Ok(())
            }
        }
    }

    /// Remove an old whiteout before creating a fresh upper entry of the same
    /// name.
    fn remove_existing_whiteout(
        &self,
        operation: &LocationOperationView<'_>,
        name: &str,
    ) -> VfsResult<()> {
        if let Some(upper) = self.lookup_any_upper_child(operation, name)?
            && is_whiteout(operation, &upper)?
            && let Some(upper_dir) = self.existing_upper_dir()
        {
            upper_dir.unlink(operation, name, upper.is_dir(operation)?)?;
        }
        Ok(())
    }
}

impl NodeOps for OverlayDir {
    fn inode(&self) -> u64 {
        self.fs
            .with_generation(|operation| self.current_dir()?.inode(operation))
            .unwrap_or(0)
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        self.fs
            .with_generation(|operation| self.current_dir()?.metadata(operation))
    }

    fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()> {
        self.fs.with_generation(|operation| {
            self.materialize_upper_dir(operation)?
                .update_metadata(operation, update)
        })
    }

    fn filesystem(&self) -> &dyn FilesystemOps {
        self.fs.as_ref()
    }

    fn sync(&self, data_only: bool) -> VfsResult<()> {
        self.fs.with_generation(|operation| {
            if let Some(upper_dir) = self.existing_upper_dir() {
                upper_dir.sync(operation, data_only)?;
            }
            Ok(())
        })
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
        self.fs.with_generation(|operation| {
            let mut entries = BTreeMap::new();
            let is_opaque = match self.existing_upper_dir() {
                Some(upper_dir) => is_opaque(operation, &upper_dir)?,
                None => false,
            };
            if !is_opaque {
                for lower in self.lower_dirs.iter().rev() {
                    read_names(operation, lower, &mut entries)?;
                }
            }
            if let Some(upper_dir) = self.existing_upper_dir() {
                read_names(operation, &upper_dir, &mut entries)?;
            }

            let mut emitted = 0;
            for (idx, (name, info)) in entries.into_iter().enumerate().skip(offset as usize) {
                if !sink.accept(&name, info.ino, info.node_type, idx as u64 + 1) {
                    break;
                }
                emitted += 1;
            }
            Ok(emitted)
        })
    }

    /// Lookup one merged child name.
    ///
    /// Unlike read_dir, lookup must explicitly distinguish upper whiteout from
    /// upper missing so path lookup cannot re-expose lower files hidden by
    /// unlink or opaque directory semantics.
    fn lookup(&self, name: &str) -> VfsResult<DirEntry> {
        self.fs.with_generation(
            |operation| match self.lookup_upper_child(operation, name)? {
                UpperLookup::Present(upper) => self.build_entry(operation, name, Some(upper), None),
                UpperLookup::Whiteout => Err(VfsError::NotFound),
                UpperLookup::Missing => {
                    if let Some(upper_dir) = self.existing_upper_dir()
                        && is_opaque(operation, &upper_dir)?
                    {
                        return Err(VfsError::NotFound);
                    }
                    let lower = lookup_lower(operation, &self.lower_dirs, name)?
                        .ok_or(VfsError::NotFound)?;
                    self.build_entry(operation, name, None, Some(lower))
                }
            },
        )
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
        self.fs.with_generation(|operation| {
            self.ensure_no_visible_entry(operation, name)?;
            self.remove_existing_whiteout(operation, name)?;
            let upper = self
                .materialize_upper_dir(operation)?
                .create(operation, name, node_type, permission, uid, gid)?;
            self.build_entry(operation, name, Some(upper), None)
        })
    }

    /// Create a hard link by first ensuring the source lives in upper.
    fn link(&self, name: &str, node: &DirEntry) -> VfsResult<DirEntry> {
        self.fs.with_generation(|operation| {
            self.ensure_no_visible_entry(operation, name)?;
            self.remove_existing_whiteout(operation, name)?;

            let target = node.downcast::<OverlayFile>()?.ensure_upper(operation)?;
            let linked = self
                .materialize_upper_dir(operation)?
                .link(operation, name, &target)?;
            self.build_entry(operation, name, Some(linked), None)
        })
    }

    /// Unlink a visible upper entry and create a whiteout when a lower entry
    /// with the same name exists.
    fn unlink(&self, name: &str, _is_dir: bool) -> VfsResult<()> {
        self.fs.with_generation(|operation| {
            if let Some(upper) = self.lookup_visible_upper_child(operation, name)?
                && let Some(upper_dir) = self.existing_upper_dir()
            {
                upper_dir.unlink(operation, name, upper.is_dir(operation)?)?;
            }
            if lookup_lower(operation, &self.lower_dirs, name)?.is_some() {
                create_whiteout(operation, &self.materialize_upper_dir(operation)?, name)?;
                return Ok(());
            }
            Ok(())
        })
    }

    /// Rename overlay entries with conservative lower-backed directory rules.
    ///
    /// Lower-backed files are copied up before rename. Lower-backed
    /// directories are rejected because full redirect_dir/index semantics are
    /// not implemented.
    fn rename(&self, src_name: &str, dst_dir: &DirNode, dst_name: &str) -> VfsResult<()> {
        self.fs.with_generation(|operation| {
            let dst = dst_dir.downcast::<Self>()?;
            let src = match self.lookup_visible_upper_child(operation, src_name)? {
                Some(upper) => upper,
                None => {
                    let lower = lookup_lower(operation, &self.lower_dirs, src_name)?
                        .ok_or(VfsError::NotFound)?;
                    if lower.is_dir(operation)? {
                        return Err(VfsError::CrossesDevices);
                    }
                    ensure_upper_from_lower(
                        operation,
                        &self.materialize_upper_dir(operation)?,
                        &lower,
                        src_name,
                    )?
                }
            };
            dst.remove_existing_whiteout(operation, dst_name)?;
            self.materialize_upper_dir(operation)?.rename(
                operation,
                src_name,
                &dst.materialize_upper_dir(operation)?,
                dst_name,
            )?;
            if lookup_lower(operation, &self.lower_dirs, src_name)?.is_some() {
                create_whiteout(operation, &self.materialize_upper_dir(operation)?, src_name)?;
            }
            if src.is_dir(operation)?
                && let Some(moved) = dst.lookup_visible_upper_child(operation, dst_name)?
            {
                mark_opaque(operation, &moved)?;
            }
            Ok(())
        })
    }
}

struct OverlayFile {
    fs: Arc<OverlayFs>,
    /// Materialized upper parent directory, if one exists.
    upper_dir: SpinMutex<Option<OverlayLocation>>,
    /// Parent path from overlay root, used to materialize the upper parent.
    parent_path: Vec<String>,
    name: String,
    /// Upper file captured when the entry was built.
    upper: Option<OverlayLocation>,
    /// Lower file captured when the entry was built.
    lower: Option<OverlayLocation>,
}

impl OverlayFile {
    fn existing_upper_dir(&self) -> Option<OverlayLocation> {
        self.upper_dir.lock().clone()
    }

    /// Ensure the parent directory for this file exists in upper.
    fn materialize_upper_dir(
        &self,
        operation: &LocationOperationView<'_>,
    ) -> VfsResult<OverlayLocation> {
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
            if let Some(existing) = lookup_visible_upper(operation, &upper_dir, name)? {
                existing.check_is_dir(operation)?;
                upper_dir = existing;
            } else {
                let lower =
                    lookup_lower(operation, &lower_dirs, name)?.ok_or(VfsError::NotFound)?;
                lower.check_is_dir(operation)?;
                upper_dir = copy_entry(operation, &lower, &upper_dir, name)?;
            }
            lower_dirs = OverlayDir::lower_dirs_for_child_in(operation, &lower_dirs, name);
        }

        *self.upper_dir.lock() = Some(upper_dir.clone());
        Ok(upper_dir)
    }

    /// Return the currently visible backing file.
    fn current(&self, operation: &LocationOperationView<'_>) -> VfsResult<OverlayLocation> {
        if let Some(upper_dir) = self.existing_upper_dir()
            && let Some(upper) = lookup_visible_upper(operation, &upper_dir, &self.name)?
        {
            return Ok(upper);
        }
        self.lower.clone().ok_or(VfsError::NotFound)
    }

    /// Ensure this file has a writable upper backing file.
    fn ensure_upper(&self, operation: &LocationOperationView<'_>) -> VfsResult<OverlayLocation> {
        if let Some(upper_dir) = self.existing_upper_dir()
            && let Some(upper) = lookup_visible_upper(operation, &upper_dir, &self.name)?
        {
            return Ok(upper);
        }
        let lower = self.lower.as_ref().ok_or(VfsError::NotFound)?;
        ensure_upper_from_lower(
            operation,
            &self.materialize_upper_dir(operation)?,
            lower,
            &self.name,
        )
    }
}

impl NodeOps for OverlayFile {
    fn inode(&self) -> u64 {
        self.fs
            .with_generation(|operation| {
                self.upper
                    .as_ref()
                    .or(self.lower.as_ref())
                    .map_or(Ok(0), |location| location.inode(operation))
            })
            .unwrap_or(0)
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        self.fs
            .with_generation(|operation| self.current(operation)?.metadata(operation))
    }

    fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()> {
        self.fs.with_generation(|operation| {
            self.ensure_upper(operation)?
                .update_metadata(operation, update)
        })
    }

    fn filesystem(&self) -> &dyn FilesystemOps {
        self.fs.as_ref()
    }

    fn sync(&self, data_only: bool) -> VfsResult<()> {
        self.fs
            .with_generation(|operation| self.current(operation)?.sync(operation, data_only))
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn flags(&self) -> NodeFlags {
        self.fs
            .with_generation(|operation| self.current(operation)?.flags(operation))
            .map_or(NodeFlags::NON_CACHEABLE, |flags| {
                (flags & !NodeFlags::ALWAYS_CACHE) | NodeFlags::NON_CACHEABLE
            })
    }
}

impl FileNodeOps for OverlayFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        self.fs
            .with_generation(|operation| self.current(operation)?.read_at(operation, buf, offset))
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize> {
        self.fs.with_generation(|operation| {
            self.ensure_upper(operation)?
                .write_at(operation, buf, offset)
        })
    }

    fn append(&self, buf: &[u8]) -> VfsResult<(usize, u64)> {
        self.fs
            .with_generation(|operation| self.ensure_upper(operation)?.append(operation, buf))
    }

    fn set_len(&self, len: u64) -> VfsResult<()> {
        self.fs
            .with_generation(|operation| self.ensure_upper(operation)?.set_len(operation, len))
    }

    fn set_symlink(&self, target: &str) -> VfsResult<()> {
        self.fs.with_generation(|operation| {
            self.ensure_upper(operation)?.set_symlink(operation, target)
        })
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        self.fs
            .with_generation(|operation| self.current(operation)?.ioctl(operation, cmd, arg))
    }
}

impl FsPollable for OverlayFile {
    fn poll(&self) -> FsIoEvents {
        self.fs
            .with_generation(|operation| self.current(operation)?.poll(operation))
            .unwrap_or(FsIoEvents::ERR)
    }

    fn register(&self, context: &mut Context<'_>, events: FsIoEvents) {
        if self
            .fs
            .with_generation(|operation| {
                self.current(operation)?
                    .register(operation, context, events)
            })
            .is_err()
        {
            context.waker().wake_by_ref();
        }
    }
}
