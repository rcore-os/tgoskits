use alloc::{boxed::Box, sync::Arc, vec::Vec};
#[cfg(feature = "ext4")]
use alloc::{collections::BTreeMap, sync::Weak};
#[cfg(feature = "vfs")]
use core::sync::atomic::AtomicBool;
use core::{
    num::NonZeroUsize,
    sync::atomic::{AtomicU8, AtomicU64, Ordering},
    task::Context,
};

use ax_alloc::{UsageKind, global_allocator};
use ax_io::{SeekFrom, prelude::*};
use ax_memory_addr::{PhysAddr, VirtAddr};
use ax_sync::Mutex;
#[cfg(feature = "ext4")]
use axfs_ng_vfs::FilesystemOps;
use axfs_ng_vfs::{
    FileNode, Location, NodeFlags, NodePermission, NodeType, VfsError, VfsResult, path::Path,
};
use axpoll::{IoEvents, Pollable};
use intrusive_collections::{LinkedList, LinkedListAtomicLink, intrusive_adapter};
use lru::LruCache;

use super::FsContext;

#[cfg(feature = "ext4")]
type CachedFileKey = (usize, u64);
#[cfg(feature = "ext4")]
type InodeCacheIndex = BTreeMap<CachedFileKey, Weak<CachedFileShared>>;

#[cfg(feature = "ext4")]
static CACHED_FILE_BY_INODE: spin::LazyLock<spin::Mutex<InodeCacheIndex>> =
    spin::LazyLock::new(|| spin::Mutex::new(BTreeMap::new()));

bitflags::bitflags! {
    /// Flags describing the access mode of an opened file.
    #[derive(Debug, Clone, Copy)]
    pub struct FileFlags: u8 {
        /// Read access.
        const READ = 1;
        /// Write access.
        const WRITE = 2;
        /// Execute access.
        const EXECUTE = 4;
        /// Append mode — writes always go to the end of the file.
        const APPEND = 8;
        /// Path-only handle, no actual I/O is permitted.
        const PATH = 16;
    }
}

/// Results returned by [`OpenOptions::open`].
pub enum OpenResult {
    /// The opened path is a regular file.
    File(File),
    /// The opened path is a directory.
    Dir(Location),
}

impl OpenResult {
    /// Converts into a [`File`], returning an error if this is a directory.
    pub fn into_file(self) -> VfsResult<File> {
        match self {
            Self::File(file) => Ok(file),
            Self::Dir(dir) => {
                drop(dir);
                Err(VfsError::IsADirectory)
            }
        }
    }

    /// Converts into a [`Location`], returning an error if this is a file.
    #[cfg(feature = "vfs")]
    pub fn into_dir(self) -> VfsResult<Location> {
        match self {
            Self::Dir(dir) => Ok(dir),
            Self::File(_) => Err(VfsError::NotADirectory),
        }
    }

    /// Extracts the underlying [`Location`] regardless of variant.
    #[cfg(feature = "vfs")]
    pub fn into_location(self) -> Location {
        match self {
            Self::File(file) => file.location().clone(),
            Self::Dir(dir) => dir,
        }
    }
}

/// Options and flags which can be used to configure how a file is opened.
#[derive(Debug, Clone)]
pub struct OpenOptions {
    // generic
    read: bool,
    write: bool,
    append: bool,
    truncate: bool,
    create: bool,
    create_new: bool,
    directory: bool,
    no_follow: bool,
    direct: bool,
    user: Option<(u32, u32)>,
    path: bool,
    node_type: NodeType,
    // system-specific
    mode: u32,
}

impl OpenOptions {
    /// Creates a blank new set of options ready for configuration.
    pub fn new() -> Self {
        Self {
            // generic
            read: false,
            write: false,
            append: false,
            truncate: false,
            create: false,
            create_new: false,
            directory: false,
            no_follow: false,
            direct: false,
            user: None,
            path: false,
            node_type: NodeType::RegularFile,
            // system-specific
            mode: 0o666,
        }
    }

    /// Sets the option for read access.
    pub fn read(&mut self, read: bool) -> &mut Self {
        self.read = read;
        self
    }

    /// Sets the option for write access.
    pub fn write(&mut self, write: bool) -> &mut Self {
        self.write = write;
        self
    }

    /// Sets the option for the append mode.
    pub fn append(&mut self, append: bool) -> &mut Self {
        self.append = append;
        self
    }

    /// Sets the option for truncating a previous file.
    pub fn truncate(&mut self, truncate: bool) -> &mut Self {
        self.truncate = truncate;
        self
    }

    /// Sets the option to create a new file, or open it if it already exists.
    pub fn create(&mut self, create: bool) -> &mut Self {
        self.create = create;
        self
    }

    /// Sets the option to create a new file, failing if it already exists.
    pub fn create_new(&mut self, create_new: bool) -> &mut Self {
        self.create_new = create_new;
        self
    }

    /// Sets the option to open directory instead.
    #[cfg(feature = "vfs")]
    pub fn directory(&mut self, directory: bool) -> &mut Self {
        self.directory = directory;
        self
    }

    /// Sets the option to not follow symlinks.
    #[cfg(feature = "vfs")]
    pub fn no_follow(&mut self, no_follow: bool) -> &mut Self {
        self.no_follow = no_follow;
        self
    }

    /// Sets the option to open the file with direct I/O.\
    #[cfg(feature = "vfs")]
    pub fn direct(&mut self, direct: bool) -> &mut Self {
        self.direct = direct;
        self
    }

    /// Sets the user and group id to open the file with.
    #[cfg(feature = "vfs")]
    pub fn user(&mut self, uid: u32, gid: u32) -> &mut Self {
        self.user = Some((uid, gid));
        self
    }

    /// Sets the option for path only access.
    #[cfg(feature = "vfs")]
    pub fn path(&mut self, path: bool) -> &mut Self {
        self.path = path;
        self
    }

    /// Sets the node type for the file.
    ///
    /// This will only be used if the file is created.
    #[cfg(feature = "vfs")]
    pub fn node_type(&mut self, node_type: NodeType) -> &mut Self {
        self.node_type = node_type;
        self
    }

    /// Sets the mode bits that a new file will be created with.
    #[cfg(feature = "vfs")]
    pub fn mode(&mut self, mode: u32) -> &mut Self {
        self.mode = mode;
        self
    }

    fn _open(&self, loc: Location) -> VfsResult<OpenResult> {
        let flags = self.to_flags()?;

        // O_CREAT on an existing directory → EISDIR (Linux behavior;
        // CREAT carries an implicit "create regular file" intent that
        // conflicts with an existing directory regardless of access mode).
        // Fixes bug-open-creat-on-existing-dir-no-eisdir.
        // O_PATH path bypasses this — it doesn't actually open / mutate.
        if self.create && loc.is_dir() && !self.path {
            return Err(VfsError::IsADirectory);
        }

        if loc.is_readonly()
            && (flags.intersects(FileFlags::WRITE | FileFlags::APPEND) || self.truncate)
        {
            return Err(VfsError::ReadOnlyFilesystem);
        }

        if self.directory {
            loc.check_is_dir()?;
        }

        // ENXIO on opening a UNIX-domain-socket file. man 2 open §"ENXIO":
        // "The file is a UNIX domain socket." Two exclusions:
        //   (1) O_PATH bypass: socket file can still be O_PATH-opened to get a
        //       location handle.
        //   (2) Caller intends to create a socket (self.node_type == Socket,
        //       used by axnet UnixSocket::bind which mounts /dev/log etc.)
        //       — opening a freshly-created Socket via the create-then-open
        //       path is internal kernel use, not user open(2).
        // Fixes bug-open-unix-socket-no-enxio.
        if !self.path
            && self.node_type != NodeType::Socket
            && loc.metadata()?.node_type == NodeType::Socket
        {
            return Err(VfsError::NoSuchDeviceOrAddress);
        }

        Ok(if loc.is_dir() {
            if self.truncate {
                return Err(VfsError::IsADirectory);
            }
            if flags.contains(FileFlags::WRITE) {
                return Err(VfsError::IsADirectory);
            }
            OpenResult::Dir(loc)
        } else {
            // TODO(mivik): is this correct?
            let non_cacheable_type = matches!(
                loc.metadata()?.node_type,
                NodeType::CharacterDevice | NodeType::Fifo | NodeType::Socket
            );

            let direct = non_cacheable_type
                || self.path
                || self.direct
                || loc.flags().contains(NodeFlags::NON_CACHEABLE);
            let backend = if !direct || loc.flags().contains(NodeFlags::ALWAYS_CACHE) {
                FileBackend::new_cached(loc)?
            } else {
                FileBackend::new_direct(loc)
            };
            if self.truncate {
                backend.set_len(0)?;
            }
            OpenResult::File(File::new(backend, flags))
        })
    }

    /// Opens a file at the given [`Location`] using these options.
    #[cfg(feature = "vfs")]
    pub fn open_loc(&self, loc: Location) -> VfsResult<OpenResult> {
        if !self.is_valid() {
            return Err(VfsError::InvalidInput);
        }
        self._open(loc)
    }

    /// Opens a file at the given path relative to the provided [`FsContext`].
    pub fn open(&self, context: &FsContext, path: impl AsRef<Path>) -> VfsResult<OpenResult> {
        if !self.is_valid() {
            return Err(VfsError::InvalidInput);
        }

        // Empty pathname → NotFound. man "ENOENT — O_CREAT is not set and
        // the named file does not exist." resolve_parent("") would otherwise
        // return cwd itself which lets open() succeed — wrong per POSIX.
        // openat() does not accept AT_EMPTY_PATH; only specific *at calls do.
        // Fixes bug-openat-empty-path-no-enoent.
        if path.as_ref().as_str().is_empty() {
            return Err(VfsError::NotFound);
        }

        // Trailing-slash check: man — paths with trailing '/' must refer to
        // a directory. Components::parse_forward strips the empty trailing
        // component, so we use Path::has_trailing_slash() to recover the
        // signal. Captured early; the post-resolution check below enforces
        // it. Fixes bug-open-trailing-slash.
        let must_be_dir = path.as_ref().has_trailing_slash();

        let loc = match context.resolve_parent(path.as_ref()) {
            Ok((parent, name)) => {
                // If the path ends with '/', Linux never creates regular
                // files via O_CREAT here — the path explicitly requests a
                // directory, and open() cannot create directories. Suppress
                // create flags BEFORE open_file to avoid creating an inode
                // that the post-check would then reject (codex P1: original
                // ordering left a stale file on disk for failing calls).
                let effective_create = self.create && !must_be_dir;
                let effective_create_new = self.create_new && !must_be_dir;
                let mut loc = parent.open_file(
                    &name,
                    &axfs_ng_vfs::OpenOptions {
                        create: effective_create,
                        create_new: effective_create_new,
                        node_type: self.node_type,
                        permission: NodePermission::from_bits_truncate(self.mode as _),
                        user: self.user,
                    },
                )?;
                if !self.no_follow {
                    // Save the symlink-target path before resolving, so we can
                    // recurse into create-at-target if the target is dangling.
                    let was_symlink = loc.node_type() == NodeType::Symlink;
                    let symlink_target = if was_symlink && self.create {
                        loc.read_link().ok()
                    } else {
                        None
                    };
                    let parent_for_resolve = parent.clone();
                    match context
                        .with_current_dir(parent_for_resolve)?
                        .try_resolve_symlink(loc, &mut 0)
                    {
                        Ok(resolved) => loc = resolved,
                        Err(VfsError::NotFound) if self.create && symlink_target.is_some() => {
                            // O_CREAT on a dangling symlink: man — Linux follows
                            // the symlink and creates the target file (provided
                            // its parent directory exists). Recurse with the
                            // symlink target as the new path.
                            // Fixes bug-open-creat-dangling-no-create.
                            let target = symlink_target.unwrap();
                            return self.open(&context.with_current_dir(parent)?, &target);
                        }
                        Err(e) => return Err(e),
                    }
                } else if loc.node_type() == NodeType::Symlink && !self.path {
                    // O_NOFOLLOW + basename is a symlink + not O_PATH:
                    // man "If the trailing component (i.e., basename) of
                    // pathname is a symbolic link, then the open fails,
                    // with the error ELOOP."
                    //
                    // Precedence: a trailing slash on the original path
                    // forces the resolved entry to be a directory; a
                    // symlink itself is not a directory, so ENOTDIR
                    // takes priority over ELOOP (Linux behavior verified
                    // via host gcc: `open("/tmp/sym/", O_NOFOLLOW)` →
                    // ENOTDIR, not ELOOP). Without this check, starry
                    // returns ELOOP and diverges from Linux.
                    if must_be_dir {
                        return Err(VfsError::NotADirectory);
                    }
                    // Fixes bug-open-nofollow-sym.
                    return Err(VfsError::FilesystemLoop);
                }
                loc
            }
            Err(VfsError::InvalidInput) => {
                // root directory
                context.root_dir().clone()
            }
            Err(err) => return Err(err),
        };

        // Trailing-slash post-check: if the original pathname ended with '/'
        // (other than the root itself), the resolved location MUST be a
        // directory; otherwise return NotADirectory.
        if must_be_dir && !loc.is_dir() {
            return Err(VfsError::NotADirectory);
        }

        self._open(loc)
    }

    pub(crate) fn to_flags(&self) -> VfsResult<FileFlags> {
        // Linux semantic: O_APPEND only adds APPEND bit; it does NOT promote
        // read-only fd to read/write. (Previous code merged (true,_,true) →
        // READ|WRITE|APPEND which silently upgraded RDONLY|APPEND to RW —
        // see bug-open-rdonly-append-promotes-rw.)
        Ok(match (self.read, self.write, self.append) {
            (true, false, false) => FileFlags::READ,
            (false, true, false) => FileFlags::WRITE,
            (true, true, false) => FileFlags::READ | FileFlags::WRITE,
            (true, false, true) => FileFlags::READ | FileFlags::APPEND,
            (false, true, true) => FileFlags::WRITE | FileFlags::APPEND,
            (true, true, true) => FileFlags::READ | FileFlags::WRITE | FileFlags::APPEND,
            (false, false, true) => FileFlags::APPEND, // RDONLY-equivalent + APPEND: pure status
            (false, false, false) => return Err(VfsError::InvalidInput),
        } | if self.path {
            FileFlags::PATH
        } else {
            FileFlags::empty()
        })
    }

    pub(crate) fn is_valid(&self) -> bool {
        if !self.read && !self.write && !self.append {
            return false;
        }
        // Linux multi-fs: RDONLY|TRUNC truncates the file (POSIX VERSIONS
        // says effect is "unspecified", but most fs do truncate). Don't
        // reject. Fixes bug-open-rdonly-trunc-einval.
        // RDWR|APPEND|TRUNC is also explicitly allowed by Linux; the prior
        // restriction "(_,true) && truncate && !create_new → false" was too
        // strict. Fixes bug-open-append-trunc-einval.
        true
    }
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self::new()
    }
}

const PAGE_SIZE: usize = 4096;
const DISK_PAGE_CACHE_CAP: usize = 8192;

/// A single page-sized cache entry backed by a physical page.
#[derive(Debug)]
pub struct PageCache {
    addr: VirtAddr,
    dirty: bool,
    dirty_generation: u64,
    writeback_protecting: bool,
    dirty_during_writeback: bool,
}

impl PageCache {
    fn new() -> VfsResult<Self> {
        let addr = global_allocator()
            .alloc_pages(1, PAGE_SIZE, UsageKind::PageCache)
            .map_err(|err| {
                warn!("Failed to allocate page cache: {:?}", err);
                VfsError::NoMemory
            })?;
        Ok(Self {
            addr: addr.into(),
            dirty: false,
            dirty_generation: 0,
            writeback_protecting: false,
            dirty_during_writeback: false,
        })
    }

    /// Returns the physical address of this page.
    pub fn paddr(&self) -> VfsResult<PhysAddr> {
        crate::os::virt_to_phys(self.addr.as_usize())
            .map(PhysAddr::from)
            .ok_or(VfsError::BadState)
    }

    /// Marks this page as dirty so it will be flushed on eviction.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
        if self.writeback_protecting {
            self.dirty_during_writeback = true;
        }
        self.dirty_generation = self.dirty_generation.wrapping_add(1);
    }

    /// Returns a mutable slice over the page data.
    pub fn data(&mut self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.addr.as_mut_ptr(), PAGE_SIZE) }
    }
}

impl Drop for PageCache {
    fn drop(&mut self) {
        if self.dirty {
            warn!("dirty page dropped without flushing");
        }
        global_allocator().dealloc_pages(self.addr.as_usize(), 1, UsageKind::PageCache);
    }
}

/// Eviction listener callback.  Returns `true` if the listener
/// successfully invalidated all mappings for the evicted page.
/// When `false` is returned, the caller must **not** drop the page
/// (doing so would free the physical frame while PTEs still reference
/// it); instead the page is returned to the cache for a later retry.
type EvictListenerFn = Arc<dyn Fn(u32, &PageCache) -> bool + Send + Sync>;
type WritebackProtectListenerFn = Arc<dyn Fn(u32) -> bool + Send + Sync>;

struct DirtyPageSnapshot {
    pn: u32,
    generation: u64,
    data: Box<[u8]>,
    len: usize,
}

struct EvictListener {
    listener: EvictListenerFn,
    writeback_protect: WritebackProtectListenerFn,
    link: LinkedListAtomicLink,
}

intrusive_adapter!(EvictListenerAdapter = Box<EvictListener>: EvictListener { link: LinkedListAtomicLink });

struct CachedFileShared {
    page_cache: Mutex<LruCache<u32, PageCache>>,
    io_lock: Mutex<()>,
    evict_listeners: Mutex<LinkedList<EvictListenerAdapter>>,
    backing: Option<FileNode>,
    len: AtomicU64,
}

impl CachedFileShared {
    pub fn new(len: u64, backing: FileNode) -> Self {
        Self {
            page_cache: Mutex::new(LruCache::new(
                NonZeroUsize::new(DISK_PAGE_CACHE_CAP).unwrap(),
            )),
            io_lock: Mutex::new(()),
            evict_listeners: Mutex::new(LinkedList::default()),
            backing: Some(backing),
            len: AtomicU64::new(len),
        }
    }

    pub fn new_unbounded(len: u64) -> Self {
        Self {
            page_cache: Mutex::new(LruCache::unbounded()),
            io_lock: Mutex::new(()),
            evict_listeners: Mutex::new(LinkedList::default()),
            backing: None,
            len: AtomicU64::new(len),
        }
    }

    fn len(&self) -> u64 {
        self.len.load(Ordering::Acquire)
    }

    fn update_len_max(&self, len: u64) {
        let mut current = self.len();
        while len > current {
            match self
                .len
                .compare_exchange_weak(current, len, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
    }

    fn set_len(&self, len: u64) {
        self.len.store(len, Ordering::Release);
    }

    fn backing(&self) -> VfsResult<&FileNode> {
        self.backing.as_ref().ok_or(VfsError::InvalidInput)
    }

    fn writeback(&self) -> VfsResult<alloc::vec::Vec<u32>> {
        let (file_len, dirty_keys) = self.begin_writeback_all_dirty();
        self.protect_dirty_pages_before_writeback(&dirty_keys)
            .inspect_err(|_| self.cancel_writeback_tracking(&dirty_keys))?;
        let _io = self.io_lock.lock();
        let result = self.writeback_page_runs(file_len, &dirty_keys);
        self.finish_writeback_tracking(&dirty_keys);
        result?;
        self.backing()?.sync(false)?;
        Ok(dirty_keys)
    }

    fn writeback_pages(&self, pns: &[u32]) -> VfsResult<()> {
        let (file_len, dirty_keys) = self.begin_writeback_pages(pns);
        self.protect_dirty_pages_before_writeback(&dirty_keys)
            .inspect_err(|_| self.cancel_writeback_tracking(&dirty_keys))?;
        let _io = self.io_lock.lock();
        let result = self.writeback_page_runs(file_len, &dirty_keys);
        self.finish_writeback_tracking(&dirty_keys);
        result?;
        self.backing()?.sync(false)?;
        Ok(())
    }

    fn sync(&self, data_only: bool) -> VfsResult<()> {
        let (file_len, dirty_keys) = self.begin_writeback_all_dirty();
        self.protect_dirty_pages_before_writeback(&dirty_keys)
            .inspect_err(|_| self.cancel_writeback_tracking(&dirty_keys))?;
        let _io = self.io_lock.lock();
        let result = self.writeback_page_runs(file_len, &dirty_keys);
        self.finish_writeback_tracking(&dirty_keys);
        result?;
        self.backing()?.sync(data_only)?;
        Ok(())
    }

    #[cfg(feature = "vfs")]
    fn writeback_dirty_for_global_sync(&self) -> VfsResult<()> {
        let (file_len, dirty_keys) = self.begin_writeback_all_dirty();
        if dirty_keys.is_empty() {
            return Ok(());
        }
        self.protect_dirty_pages_before_writeback(&dirty_keys)
            .inspect_err(|_| self.cancel_writeback_tracking(&dirty_keys))?;
        let _io = self.io_lock.lock();
        let result = self.writeback_page_runs(file_len, &dirty_keys);
        self.finish_writeback_tracking(&dirty_keys);
        result
    }

    #[cfg(feature = "vfs")]
    fn has_dirty_pages(&self) -> bool {
        self.page_cache.lock().iter().any(|(_, page)| page.dirty)
    }

    fn begin_writeback_all_dirty(&self) -> (u64, Vec<u32>) {
        self.begin_writeback(None)
    }

    fn begin_writeback_pages(&self, pns: &[u32]) -> (u64, Vec<u32>) {
        self.begin_writeback(Some(pns))
    }

    fn begin_writeback(&self, requested: Option<&[u32]>) -> (u64, Vec<u32>) {
        let _io = self.io_lock.lock();
        let file_len = self.len();
        let mut requested_pns = requested.map(|pns| pns.to_vec());
        if let Some(pns) = requested_pns.as_mut() {
            pns.sort_unstable();
            pns.dedup();
        }
        let mut guard = self.page_cache.lock();
        let dirty_keys = guard
            .iter_mut()
            .filter_map(|(&pn, page)| {
                if !page.dirty {
                    return None;
                }
                if let Some(requested) = requested_pns.as_ref()
                    && requested.binary_search(&pn).is_err()
                {
                    return None;
                }
                let page_start = pn as u64 * PAGE_SIZE as u64;
                let len = file_len.saturating_sub(page_start).min(PAGE_SIZE as u64);
                if len == 0 {
                    return None;
                }
                page.writeback_protecting = true;
                page.dirty_during_writeback = false;
                Some(pn)
            })
            .collect();
        (file_len, dirty_keys)
    }

    fn writeback_page_runs(&self, file_len: u64, pns: &[u32]) -> VfsResult<()> {
        let mut snapshots = self.snapshot_dirty_pages(file_len, pns)?;
        snapshots.sort_by_key(|page| page.pn);

        let mut run_start = 0;
        while run_start < snapshots.len() {
            let mut run_end = run_start + 1;
            while run_end < snapshots.len()
                && snapshots[run_end].pn == snapshots[run_end - 1].pn + 1
                && snapshots[run_end - 1].len == PAGE_SIZE
            {
                run_end += 1;
            }

            let offset = snapshots[run_start].pn as u64 * PAGE_SIZE as u64;
            let run_len = snapshots[run_start..run_end]
                .iter()
                .map(|page| page.len)
                .sum();
            let mut data = alloc::vec::Vec::with_capacity(run_len);
            for page in &snapshots[run_start..run_end] {
                data.extend_from_slice(&page.data[..page.len]);
            }
            self.backing()?.write_at(&data, offset)?;

            {
                let mut guard = self.page_cache.lock();
                for page in &snapshots[run_start..run_end] {
                    if let Some(current) = guard.get_mut(&page.pn)
                        && current.dirty
                        && current.dirty_generation == page.generation
                        && !current.dirty_during_writeback
                    {
                        current.dirty = false;
                    }
                }
            }

            run_start = run_end;
        }
        Ok(())
    }

    fn snapshot_dirty_pages(
        &self,
        file_len: u64,
        pns: &[u32],
    ) -> VfsResult<alloc::vec::Vec<DirtyPageSnapshot>> {
        let mut snapshots = alloc::vec::Vec::new();
        let mut guard = self.page_cache.lock();
        for pn in pns {
            let Some(page) = guard.get_mut(pn) else {
                continue;
            };
            if !page.dirty {
                continue;
            }
            let page_start = *pn as u64 * PAGE_SIZE as u64;
            let len = file_len.saturating_sub(page_start).min(PAGE_SIZE as u64) as usize;
            if len == 0 {
                continue;
            }
            snapshots.push(DirtyPageSnapshot {
                pn: *pn,
                generation: page.dirty_generation,
                data: page.data()[..len].to_vec().into_boxed_slice(),
                len,
            });
        }
        Ok(snapshots)
    }

    fn protect_dirty_pages_before_writeback(&self, pns: &[u32]) -> VfsResult<()> {
        let listeners = self.writeback_protect_listeners();
        for pn in pns {
            for listener in &listeners {
                if !(listener)(*pn) {
                    return Err(VfsError::ResourceBusy);
                }
            }
        }
        Ok(())
    }

    fn writeback_protect_listeners(&self) -> Vec<WritebackProtectListenerFn> {
        self.evict_listeners
            .lock()
            .iter()
            .map(|listener| listener.writeback_protect.clone())
            .collect()
    }

    fn cancel_writeback_tracking(&self, pns: &[u32]) {
        let _io = self.io_lock.lock();
        self.finish_writeback_tracking(pns);
    }

    fn finish_writeback_tracking(&self, pns: &[u32]) {
        let mut guard = self.page_cache.lock();
        for pn in pns {
            if let Some(page) = guard.get_mut(pn) {
                page.writeback_protecting = false;
                page.dirty_during_writeback = false;
            }
        }
    }

    #[cfg(test)]
    fn invoke_writeback_protect_for_test(&self, pns: &[u32]) -> VfsResult<()> {
        self.protect_dirty_pages_before_writeback(pns)
    }

    #[cfg(test)]
    fn io_lock_is_free_for_test(&self) -> bool {
        self.io_lock.try_lock().is_some()
    }

    #[cfg(test)]
    fn listener_lock_is_free_for_test(&self) -> bool {
        self.evict_listeners.try_lock().is_some()
    }

    /// Scan the LRU and evict up to `max` clean pages.
    ///
    /// Two-phase eviction:
    /// 1. Under `page_cache` lock: identify clean pages, pop them from the
    ///    cache, and move them into a local buffer.
    /// 2. Outside `page_cache` lock: invoke evict listeners.  If all
    ///    listeners confirm the PTE unmap, the page is dropped (freeing its
    ///    physical frame).  If any listener cannot unmap (e.g., AddrSpace
    ///    lock contention), the page is re-inserted into the cache to
    ///    prevent use-after-free.
    ///
    /// Returns the number of pages successfully evicted.
    ///
    /// # Lock ordering
    ///
    /// `page_cache` is released before acquiring `evict_listeners`,
    /// eliminating the latent deadlock risk that exists when listeners
    /// are called under the cache lock.
    #[cfg(feature = "vfs")]
    fn try_evict_clean_pages(&self, max: usize) -> usize {
        let limit = max.min(256);

        // Phase 1: Pop clean pages from LRU under page_cache lock.
        // Two-pass: first collect page numbers (borrows cache immutably),
        // then pop by number (borrows cache mutably).
        let mut pending: Vec<(u32, PageCache)> = Vec::new();
        {
            let Some(mut cache) = self.page_cache.try_lock() else {
                return 0;
            };
            let mut to_pop = [0u32; 256];
            let mut cnt = 0;
            for (&pn, page) in cache.iter().rev() {
                if !page.dirty && cnt < limit {
                    to_pop[cnt] = pn;
                    cnt += 1;
                }
            }
            for &pn in to_pop[..cnt].iter() {
                if let Some(page) = cache.pop(&pn) {
                    pending.push((pn, page));
                }
            }
        } // page_cache lock released

        // Phase 2: Invoke listeners outside page_cache lock.
        let mut evicted = 0;
        for (pn, page) in pending.into_iter() {
            let mut all_ok = true;
            for listener in self.evict_listeners.lock().iter() {
                if !(listener.listener)(pn, &page) {
                    all_ok = false;
                    break;
                }
            }
            if all_ok {
                // All listeners confirmed unmap — drop page (frees physical frame).
                drop(page);
                evicted += 1;
            } else {
                // Listener could not unmap (e.g., AddrSpace lock contention).
                // Re-insert page into cache to avoid freeing a physical frame
                // that still has live PTEs pointing to it.
                let mut cache = self.page_cache.lock();
                cache.put(pn, page);
            }
        }
        evicted
    }
}

#[cfg(feature = "vfs")]
struct ReclaimGuard;

#[cfg(feature = "vfs")]
impl Drop for ReclaimGuard {
    fn drop(&mut self) {
        RECLAIM_IN_PROGRESS.store(false, Ordering::Release);
    }
}

#[cfg(feature = "vfs")]
static GLOBAL_CACHED_FILES: spin::RwLock<alloc::vec::Vec<Arc<CachedFileShared>>> =
    spin::RwLock::new(alloc::vec::Vec::new());

#[cfg(feature = "vfs")]
static RECLAIM_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

#[cfg(feature = "vfs")]
pub fn page_cache_reclaim(num_pages: usize) -> usize {
    if RECLAIM_IN_PROGRESS.swap(true, Ordering::AcqRel) {
        return 0;
    }
    let _guard = ReclaimGuard;

    let mut reclaimed = 0;
    let target = num_pages.max(16) * 2;
    let mut file_count = 0;

    if let Some(guard) = GLOBAL_CACHED_FILES.try_read() {
        for file in guard.iter() {
            let freed = file.try_evict_clean_pages(target - reclaimed);
            reclaimed += freed;
            file_count += 1;
            if reclaimed >= target {
                break;
            }
        }
    } else {
        return 0;
    }

    if reclaimed > 0 {
        debug!(
            "page_cache_reclaim: evicted {} clean pages across {} files",
            reclaimed, file_count
        );
    }

    reclaimed
}

#[cfg(feature = "vfs")]
fn register_cached_file(file: &Arc<CachedFileShared>) {
    let mut guard = GLOBAL_CACHED_FILES.write();
    guard.retain(|cached| Arc::strong_count(cached) > 1 || cached.has_dirty_pages());
    guard.push(file.clone());
}

#[cfg(feature = "vfs")]
pub fn sync_all_cached_files(_data_only: bool) -> VfsResult<()> {
    let files = GLOBAL_CACHED_FILES.read().clone();
    let mut first_error = None;
    for file in &files {
        if let Err(err) = file.writeback_dirty_for_global_sync()
            && first_error.is_none()
        {
            first_error = Some(err);
        }
    }

    drop(files);

    let mut guard = GLOBAL_CACHED_FILES.write();
    guard.retain(|cached| Arc::strong_count(cached) > 1 || cached.has_dirty_pages());

    match first_error {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

/// A file handle with an LRU page cache for buffered I/O.
pub struct CachedFile {
    inner: Location,
    shared: Arc<CachedFileShared>,
    in_memory: bool,
}

impl Clone for CachedFile {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            shared: self.shared.clone(),
            in_memory: self.in_memory,
        }
    }
}

enum FileUserData {
    Strong(Arc<CachedFileShared>),
}

impl FileUserData {
    pub fn get(&self) -> Arc<CachedFileShared> {
        match self {
            FileUserData::Strong(strong) => strong.clone(),
        }
    }
}

impl CachedFile {
    /// Returns an existing cached file for `location`, or creates a new one.
    pub fn get_or_create(location: Location) -> VfsResult<Self> {
        let in_memory = location.filesystem().name() == "tmpfs";

        let existing = {
            let guard = location.user_data();
            guard
                .get::<FileUserData>()
                .as_deref()
                .map(FileUserData::get)
        };
        if let Some(shared) = existing {
            return Ok(Self {
                inner: location,
                shared,
                in_memory,
            });
        }

        let len = location.len()?;
        #[cfg(feature = "ext4")]
        let inode_key =
            should_share_cached_file_by_inode(&location).then(|| cached_file_key(&location));
        #[cfg(feature = "ext4")]
        let inode_shared = inode_key.and_then(lookup_inode_cached_file);
        #[cfg(not(feature = "ext4"))]
        let inode_shared: Option<Arc<CachedFileShared>> = None;
        let (created, user_data) = if let Some(shared) = inode_shared {
            (shared.clone(), FileUserData::Strong(shared))
        } else if in_memory {
            let shared = Arc::new(CachedFileShared::new_unbounded(len));
            (shared.clone(), FileUserData::Strong(shared))
        } else {
            let backing = location.entry().as_file()?.clone();
            let shared = Arc::new(CachedFileShared::new(len, backing));
            (shared.clone(), FileUserData::Strong(shared))
        };

        let (shared, is_new) = {
            let mut guard = location.user_data();
            if let Some(shared) = guard
                .get::<FileUserData>()
                .as_deref()
                .map(FileUserData::get)
            {
                (shared, false)
            } else {
                guard.insert(user_data);
                (created, true)
            }
        };

        // In-memory files (tmpfs) have no backing store, so evicting clean
        // pages would lose data. Only register disk-backed files for reclaim.
        #[cfg(feature = "vfs")]
        if is_new && !in_memory {
            register_cached_file(&shared);
        }
        #[cfg(not(feature = "vfs"))]
        let _ = is_new;
        #[cfg(feature = "ext4")]
        if is_new && let Some(key) = inode_key {
            insert_inode_cached_file(key, &shared);
        }

        Ok(Self {
            inner: location,
            shared,
            in_memory,
        })
    }

    /// Returns `true` if both handles refer to the same shared state.
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.shared, &other.shared)
    }

    /// Returns `true` if this file is backed by an in-memory filesystem (e.g. tmpfs).
    pub fn in_memory(&self) -> bool {
        self.in_memory
    }

    /// Returns the current length (in bytes) of the backing file.
    pub fn file_len(&self) -> VfsResult<u64> {
        self.inner.len()
    }

    /// Registers a listener that is called when a page is evicted from cache.
    ///
    /// Returns a handle that can later be passed to
    /// [`remove_evict_listener`](Self::remove_evict_listener).
    pub fn add_evict_listener<F>(&self, listener: F) -> usize
    where
        F: Fn(u32, &PageCache) -> bool + Send + Sync + 'static,
    {
        self.add_page_listener(listener, |_| true)
    }

    /// Registers a listener for page eviction and dirty writeback protection.
    ///
    /// The writeback callback is invoked before a dirty cached page is
    /// snapshotted and written to backing storage. Shared mmap users should
    /// remove writable PTEs here so later writes fault and advance the dirty
    /// generation before the cache can be marked clean.
    pub fn add_page_listener<E, W>(&self, evict: E, writeback_protect: W) -> usize
    where
        E: Fn(u32, &PageCache) -> bool + Send + Sync + 'static,
        W: Fn(u32) -> bool + Send + Sync + 'static,
    {
        let pointer = Box::new(EvictListener {
            listener: Arc::new(evict),
            writeback_protect: Arc::new(writeback_protect),
            link: LinkedListAtomicLink::new(),
        });
        let handle = pointer.as_ref() as *const EvictListener as usize;
        self.shared.evict_listeners.lock().push_back(pointer);
        handle
    }

    /// # Safety
    /// The handle must be valid, that means:
    /// - It must be returned by a previous call to `add_evict_listener` on the same `CachedFile`.
    /// - It must not be removed by a previous call to `remove_evict_listener`.
    pub unsafe fn remove_evict_listener(&self, handle: usize) {
        let mut guard = self.shared.evict_listeners.lock();
        let mut cursor = unsafe { guard.cursor_mut_from_ptr(handle as *const EvictListener) };
        cursor.remove();
    }

    fn evict_cache(&self, file: &FileNode, pn: u32, page: &mut PageCache) -> VfsResult<()> {
        for listener in self.shared.evict_listeners.lock().iter() {
            // In the LRU-eviction path (triggered by page_or_insert), the
            // populate process holds AddrSpace and handles the unmap via
            // PopulateCallback.  The listener's return value is irrelevant
            // here — if try_lock fails, the caller is the populate process
            // itself and it will unmap the old page after inserting the new one.
            let _ = (listener.listener)(pn, page);
        }
        if page.dirty {
            let page_start = pn as u64 * PAGE_SIZE as u64;
            let len = (self.shared.len().saturating_sub(page_start)).min(PAGE_SIZE as u64) as usize;
            if len > 0 {
                file.write_at(&page.data()[..len], page_start)?;
            }
            page.dirty = false;
        }
        Ok(())
    }

    fn page_or_insert<'a>(
        &self,
        file: &FileNode,
        cache: &'a mut LruCache<u32, PageCache>,
        pn: u32,
        read_backing: bool,
    ) -> VfsResult<(&'a mut PageCache, Option<(u32, PageCache)>)> {
        // TODO: Matching the result of `get_mut` confuses compiler. See
        // https://users.rust-lang.org/t/return-do-not-release-mutable-borrow/55757.
        if cache.contains(&pn) {
            return Ok((cache.get_mut(&pn).unwrap(), None));
        }
        let mut evicted = None;
        if cache.len() >= cache.cap().get() {
            // Cache is full, remove the least recently used page
            if let Some((pn, mut page)) = cache.pop_lru() {
                self.evict_cache(file, pn, &mut page)?;
                evicted = Some((pn, page));
            }
        }

        let mut page = PageCache::new()?;
        if self.in_memory || !read_backing {
            page.data().fill(0);
        } else {
            // `PageCache::new()` does not zero the freshly allocated frame, and
            // `FileNodeOps::read_at` short-reads at EOF (rsext4/fat return only the
            // bytes actually read, leaving the rest of the buffer untouched). Zero the
            // tail beyond the read length so a partial last page never exposes stale
            // physical memory past EOF — POSIX/Linux require those bytes to read as 0
            // (e.g. an mmap of a 100-byte file must see `[100, PAGE_SIZE)` as zero).
            let read = file.read_at(page.data(), pn as u64 * PAGE_SIZE as u64)?;
            page.data()[read..].fill(0);
        }
        cache.put(pn, page);
        Ok((cache.get_mut(&pn).unwrap(), evicted))
    }

    /// Marks one cached mmap page dirty through the shared cached-I/O protocol.
    pub fn mark_mmap_dirty_page(&self, pn: u32) -> VfsResult<()> {
        if self.in_memory {
            return Ok(());
        }
        let _io = self.shared.io_lock.lock();
        let mut guard = self.shared.page_cache.lock();
        guard.get_mut(&pn).ok_or(VfsError::BadState)?.mark_dirty();
        Ok(())
    }

    /// Invokes `f` with the cached page at `pn`, loading it from disk if absent.
    ///
    /// If loading the page causes an eviction, the evicted `(page_number, page)`
    /// pair is also passed to `f`.
    pub fn with_page_or_insert<R>(
        &self,
        pn: u32,
        f: impl FnOnce(&mut PageCache, Option<(u32, PageCache)>) -> VfsResult<R>,
    ) -> VfsResult<R> {
        let _io = self.shared.io_lock.lock();
        let mut guard = self.shared.page_cache.lock();
        let (page, evicted) =
            self.page_or_insert(self.inner.entry().as_file()?, &mut guard, pn, true)?;
        f(page, evicted)
    }

    /// Reads data from the file at `offset` into `dst`.
    pub fn read_at(&self, mut dst: impl Write + IoBufMut, offset: u64) -> VfsResult<usize> {
        let _io = self.shared.io_lock.lock();
        let len = self.shared.len();
        let end = offset.saturating_add(dst.remaining_mut() as u64).min(len);
        if end <= offset {
            return Ok(0);
        }

        let file = self.inner.entry().as_file()?;
        let mut scratch = PageCache::new()?;
        let mut read = 0;
        let mut current = offset;
        while current < end {
            let pn = (current / PAGE_SIZE as u64) as u32;
            let page_start = pn as u64 * PAGE_SIZE as u64;
            let page_offset = (current - page_start) as usize;
            let chunk_len = (end - page_start).min(PAGE_SIZE as u64) as usize - page_offset;

            {
                let mut guard = self.shared.page_cache.lock();
                let page = self.page_or_insert(file, &mut guard, pn, true)?.0;
                scratch.data()[..chunk_len]
                    .copy_from_slice(&page.data()[page_offset..page_offset + chunk_len]);
            }

            dst.write_all(&scratch.data()[..chunk_len])?;
            read += chunk_len;
            current += chunk_len as u64;
        }

        Ok(read)
    }

    fn write_at_locked(&self, mut buf: impl Read + IoBuf, offset: u64) -> VfsResult<usize> {
        let file = self.inner.entry().as_file()?;
        let end = offset.saturating_add(buf.remaining() as u64);
        let old_len = self.shared.len();
        if end > old_len {
            file.set_len(end)?;
            self.shared.update_len_max(end);
        }

        let mut scratch = PageCache::new()?;
        let mut written = 0;
        let mut current = offset;
        while current < end && buf.remaining() > 0 {
            let pn = (current / PAGE_SIZE as u64) as u32;
            let page_start = pn as u64 * PAGE_SIZE as u64;
            let page_offset = (current - page_start) as usize;
            let chunk_len =
                ((PAGE_SIZE - page_offset).min(buf.remaining())).min((end - current) as usize);
            let n = buf.read(&mut scratch.data()[..chunk_len])?;
            if n == 0 {
                break;
            }
            self.shared.update_len_max(current + n as u64);

            {
                let mut guard = self.shared.page_cache.lock();
                let read_backing = page_start < old_len && !(page_offset == 0 && n == PAGE_SIZE);
                let page = self.page_or_insert(file, &mut guard, pn, read_backing)?.0;
                page.data()[page_offset..page_offset + n].copy_from_slice(&scratch.data()[..n]);
                if !self.in_memory {
                    page.mark_dirty();
                }
            }

            written += n;
            current += n as u64;
        }

        Ok(written)
    }

    /// Writes `buf` to the file at `offset`.
    pub fn write_at(&self, buf: impl Read + IoBuf, offset: u64) -> VfsResult<usize> {
        let _io = self.shared.io_lock.lock();
        self.write_at_locked(buf, offset)
    }

    /// Appends `buf` to the end of the file. Returns `(bytes_written, new_end)`.
    pub fn append(&self, buf: impl Read + IoBuf) -> VfsResult<(usize, u64)> {
        let _io = self.shared.io_lock.lock();
        let len = self.shared.len();
        self.write_at_locked(buf, len)
            .map(|written| (written, len + written as u64))
    }

    /// Truncates or extends the file to `len` bytes.
    pub fn set_len(&self, len: u64) -> VfsResult<()> {
        let _io = self.shared.io_lock.lock();
        let file = self.inner.entry().as_file()?;
        let old_len = self.shared.len();
        file.set_len(len)?;
        self.shared.set_len(len);

        let old_last_page = (old_len / PAGE_SIZE as u64) as u32;
        let new_last_page = (len / PAGE_SIZE as u64) as u32;
        if old_len < len {
            let mut guard = self.shared.page_cache.lock();
            if let Some(page) = guard.get_mut(&old_last_page) {
                let page_start = old_last_page as u64 * PAGE_SIZE as u64;
                let old_page_offset = (old_len - page_start) as usize;
                let new_page_offset = (len - page_start).min(PAGE_SIZE as u64) as usize;
                page.data()[old_page_offset..new_page_offset].fill(0);
                // Mark dirty so the zeroed gap is written back: ext4 `set_len`
                // only updates `i_size`, it does not clear the bytes on disk, so
                // a clean eviction + reload would otherwise resurrect stale data.
                page.dirty = true;
            }
        } else if len < old_len {
            let mut guard = self.shared.page_cache.lock();
            // Linux `truncate(len)` zeroes the tail of the partial last page, so a
            // later extend or `mmap` reads those bytes as zero. Without this, a
            // shrink that leaves a partial last page (e.g. sqlite's
            // `ftruncate(<-shm>, 3)`) keeps stale bytes there; a subsequent mmap of
            // the regrown file then sees the stale tail, so a fresh reader trusts a
            // stale wal-index header instead of recovering (juicefs sqlite WAL
            // cross-process reopen failure). This branch also covers shrinking
            // within a single page, where neither old branch ran at all.
            let tail = (len % PAGE_SIZE as u64) as usize;
            if tail != 0
                && let Some(page) = guard.get_mut(&new_last_page)
            {
                page.data()[tail..].fill(0);
                // Mark dirty so the zeroed tail is written back: ext4 `set_len`
                // updates `i_size` but leaves the on-disk bytes past it intact, so
                // a clean eviction + reload (or mmap fault) would otherwise reload
                // the stale tail from disk.
                page.dirty = true;
            }
            // Remove all pages that are wholly beyond the new length.
            // TODO(mivik): can this be more efficient?
            let keys = guard
                .iter()
                .map(|(k, _)| *k)
                .filter(|it| *it > new_last_page)
                .collect::<Vec<_>>();
            for pn in keys {
                if let Some(mut page) = guard.pop(&pn)
                    && !self.in_memory
                {
                    // Don't write back pages since they're discarded
                    page.dirty = false;
                    self.evict_cache(file, pn, &mut page)?;
                }
            }
        }
        Ok(())
    }

    pub fn writeback(&self) -> VfsResult<alloc::vec::Vec<u32>> {
        if self.in_memory {
            return Ok(alloc::vec::Vec::new());
        }
        self.shared.writeback()
    }

    pub fn writeback_pages(&self, pns: &[u32]) -> VfsResult<()> {
        if self.in_memory {
            return Ok(());
        }
        self.shared.writeback_pages(pns)
    }

    pub fn dirty_pages_in_range(&self, start_pn: u32, end_pn: u32) -> alloc::vec::Vec<u32> {
        let _io = self.shared.io_lock.lock();
        let guard = self.shared.page_cache.lock();
        guard
            .iter()
            .filter_map(|(&pn, page)| {
                if page.dirty && pn >= start_pn && pn < end_pn {
                    Some(pn)
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn clear_dirty_pages(&self, pns: &[u32]) {
        let _io = self.shared.io_lock.lock();
        let mut guard = self.shared.page_cache.lock();
        for pn in pns {
            if let Some(page) = guard.get_mut(pn) {
                page.dirty = false;
                page.dirty_generation = page.dirty_generation.wrapping_add(1);
            }
        }
    }

    /// Flushes all cached pages back to disk.
    pub fn sync(&self, data_only: bool) -> VfsResult<()> {
        if self.in_memory {
            return Ok(());
        }
        self.shared.sync(data_only)
    }

    /// Returns a reference to the underlying [`Location`].
    pub fn location(&self) -> &Location {
        &self.inner
    }
}

#[cfg(feature = "ext4")]
fn should_share_cached_file_by_inode(location: &Location) -> bool {
    location.filesystem().name() == "ext4"
}

#[cfg(feature = "ext4")]
fn filesystem_key(filesystem: &dyn FilesystemOps) -> usize {
    filesystem as *const dyn FilesystemOps as *const () as usize
}

#[cfg(feature = "ext4")]
fn cached_file_key(location: &Location) -> CachedFileKey {
    (filesystem_key(location.filesystem()), location.inode())
}

#[cfg(feature = "ext4")]
fn lookup_inode_cached_file(key: CachedFileKey) -> Option<Arc<CachedFileShared>> {
    let mut cache = CACHED_FILE_BY_INODE.lock();
    match cache.get(&key).and_then(Weak::upgrade) {
        Some(shared) => Some(shared),
        None => {
            cache.remove(&key);
            None
        }
    }
}

#[cfg(feature = "ext4")]
fn insert_inode_cached_file(key: CachedFileKey, shared: &Arc<CachedFileShared>) {
    CACHED_FILE_BY_INODE
        .lock()
        .insert(key, Arc::downgrade(shared));
}

#[cfg(feature = "ext4")]
pub(crate) fn forget_cached_file_key(filesystem: &dyn FilesystemOps, inode: u64) {
    if filesystem.name() == "ext4" {
        CACHED_FILE_BY_INODE
            .lock()
            .remove(&(filesystem_key(filesystem), inode));
    }
}

impl Drop for CachedFile {
    fn drop(&mut self) {
        // Linux close(2) does not imply fsync(2). Disk-backed page cache is
        // retained by the inode user_data and written by explicit sync paths.
    }
}

/// Low-level interface for file operations.
#[derive(Clone)]
pub enum FileBackend {
    /// File I/O goes through the page cache.
    Cached(CachedFile),
    /// File I/O bypasses the page cache and hits the VFS directly.
    Direct(Location),
}

impl FileBackend {
    pub(crate) fn new_direct(location: Location) -> Self {
        Self::Direct(location)
    }

    pub(crate) fn new_cached(location: Location) -> VfsResult<Self> {
        Ok(Self::Cached(CachedFile::get_or_create(location)?))
    }

    /// Returns the backend-visible file length.
    pub fn len(&self) -> VfsResult<u64> {
        match self {
            Self::Cached(cached) => Ok(cached.shared.len()),
            Self::Direct(loc) => loc.len(),
        }
    }

    /// Returns whether the backend-visible file length is zero.
    pub fn is_empty(&self) -> VfsResult<bool> {
        self.len().map(|len| len == 0)
    }

    /// Reads data from the file at `offset` into `dst`.
    pub fn read_at(&self, mut dst: impl Write + IoBufMut, mut offset: u64) -> VfsResult<usize> {
        match self {
            Self::Cached(cached) => cached.read_at(dst, offset),
            Self::Direct(loc) => {
                let mut total = 0;
                while !dst.is_full() {
                    let read = match dst.read_from(&mut ax_io::read_fn(|buf| {
                        loc.entry().as_file()?.read_at(buf, offset).inspect(|read| {
                            offset += *read as u64;
                        })
                    })) {
                        Ok(read) => read,
                        Err(VfsError::WouldBlock) if total > 0 => break,
                        Err(err) => return Err(err),
                    };
                    if read == 0 {
                        break;
                    }
                    total += read;
                }
                Ok(total)
            }
        }
    }

    /// Writes `src` to the file at `offset`.
    pub fn write_at(&self, mut src: impl Read + IoBuf, mut offset: u64) -> VfsResult<usize> {
        match self {
            Self::Cached(cached) => cached.write_at(src, offset),
            Self::Direct(loc) => {
                let mut total = 0;
                let mut buf = [0; ax_io::DEFAULT_BUF_SIZE];
                while !src.is_empty() {
                    let limit = src.remaining().min(buf.len());
                    let read = src.read(&mut buf[..limit])?;
                    if read == 0 {
                        break;
                    }
                    let mut chunk_written = 0;
                    while chunk_written < read {
                        let written = match loc
                            .entry()
                            .as_file()?
                            .write_at(&buf[chunk_written..read], offset)
                        {
                            Ok(written) => written,
                            Err(VfsError::WouldBlock) if total > 0 => return Ok(total),
                            Err(err) => return Err(err),
                        };
                        if written == 0 {
                            return Ok(total);
                        }
                        offset += written as u64;
                        total += written;
                        chunk_written += written;
                    }
                }
                Ok(total)
            }
        }
    }

    /// Appends `src` to the end of the file. Returns `(bytes_written, new_end)`.
    pub fn append(&self, mut src: impl Read + IoBuf) -> VfsResult<(usize, u64)> {
        match self {
            Self::Cached(cached) => cached.append(src),
            Self::Direct(loc) => {
                let mut total = 0;
                let mut end = loc.entry().as_file()?.len()?;
                while src.remaining() > 0 {
                    let chunk = src.remaining().min(ax_io::DEFAULT_BUF_SIZE);
                    let written = match src.write_to(&mut ax_io::write_fn(|buf| {
                        loc.entry().as_file()?.append(buf).map(|(n, offset)| {
                            end = offset;
                            n
                        })
                    })) {
                        Ok(written) => written,
                        Err(VfsError::WouldBlock) if total > 0 => break,
                        Err(err) => return Err(err),
                    };
                    if written == 0 {
                        break;
                    }
                    total += written;
                    if written < chunk {
                        break;
                    }
                }
                Ok((total, end))
            }
        }
    }

    /// Returns a reference to the underlying [`Location`].
    pub fn location(&self) -> &Location {
        match self {
            Self::Cached(cached) => cached.location(),
            Self::Direct(loc) => loc,
        }
    }

    /// Flushes cached data (and optionally metadata) to disk.
    pub fn sync(&self, data_only: bool) -> VfsResult<()> {
        match self {
            Self::Cached(cached) => cached.sync(data_only),
            Self::Direct(loc) => loc.entry().as_file()?.sync(data_only),
        }
    }

    /// Truncates or extends the file to `len` bytes.
    pub fn set_len(&self, len: u64) -> VfsResult<()> {
        match self {
            Self::Cached(cached) => cached.set_len(len),
            Self::Direct(loc) => loc.entry().as_file()?.set_len(len),
        }
    }
}

/// Provides `std::fs::File`-like interface.
pub struct File {
    inner: FileBackend,
    flags: AtomicU8,
    position: Option<Mutex<u64>>,
    #[cfg(feature = "times")]
    access_flags: AtomicU8,
}

impl File {
    /// Creates a new [`File`] from a [`FileBackend`] and access flags.
    pub fn new(inner: FileBackend, flags: FileFlags) -> Self {
        // man 2 open: "The file offset is set to the beginning of the file"
        // — initial position is always 0, regardless of O_APPEND.
        // O_APPEND only relocates the offset BEFORE EACH WRITE (handled in
        // `write()` via the `access(FileFlags::APPEND)` branch). Setting
        // initial position to EOF would break read() on RDONLY|APPEND
        // (read sees EOF immediately) — see bug-open-rdonly-append-promotes-rw.
        let position = if inner.location().flags().contains(NodeFlags::STREAM) {
            None
        } else {
            Some(Mutex::new(0))
        };
        Self {
            inner,
            flags: AtomicU8::new(flags.bits()),
            position,
            #[cfg(feature = "times")]
            access_flags: AtomicU8::new(0),
        }
    }

    /// Opens an existing file for reading.
    pub fn open(context: &FsContext, path: impl AsRef<Path>) -> VfsResult<Self> {
        OpenOptions::new()
            .read(true)
            .open(context, path.as_ref())
            .and_then(OpenResult::into_file)
    }

    /// Opens a file for writing, creating it if it does not exist and
    /// truncating it if it does.
    pub fn create(context: &FsContext, path: impl AsRef<Path>) -> VfsResult<Self> {
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(context, path.as_ref())
            .and_then(OpenResult::into_file)
    }

    /// Checks that the file has the required `flags` and returns the backend.
    pub fn access(&self, flags: FileFlags) -> VfsResult<&FileBackend> {
        if self.flags().contains(flags) && !self.is_path() {
            if self.inner.location().is_readonly()
                && flags.intersects(FileFlags::WRITE | FileFlags::APPEND)
            {
                return Err(VfsError::ReadOnlyFilesystem);
            }
            Ok(&self.inner)
        } else {
            Err(VfsError::BadFileDescriptor)
        }
    }

    /// Returns `true` if this is a path-only handle (no I/O permitted).
    pub fn is_path(&self) -> bool {
        self.flags().contains(FileFlags::PATH)
    }

    /// Returns the access flags this file was opened with.
    pub fn flags(&self) -> FileFlags {
        FileFlags::from_bits_truncate(self.flags.load(Ordering::Acquire))
    }

    /// Atomically sets or clears a single flag bit.
    pub fn set_flag(&self, flag: FileFlags, enabled: bool) {
        let bits = flag.bits();
        if enabled {
            self.flags.fetch_or(bits, Ordering::AcqRel);
        } else {
            self.flags.fetch_and(!bits, Ordering::AcqRel);
        }
    }

    /// Returns the file's current read/write cursor, or `None` for stream
    /// nodes (sockets / pipes / `STREAM`-flagged) that have no addressable
    /// position. Read-only snapshot; does not move the cursor.
    pub fn position(&self) -> Option<u64> {
        self.position.as_ref().map(|m| *m.lock())
    }

    /// Returns a reference to the underlying [`FileBackend`].
    pub fn backend(&self) -> VfsResult<&FileBackend> {
        self.access(FileFlags::empty())?;
        Ok(&self.inner)
    }

    /// Returns a reference to the underlying [`Location`].
    pub fn location(&self) -> &Location {
        self.inner.location()
    }

    /// Reads a number of bytes starting from a given offset.
    pub fn read_at(&self, dst: impl Write + IoBufMut, offset: u64) -> VfsResult<usize> {
        self.access(FileFlags::READ)?.read_at(dst, offset)
    }

    /// Writes a number of bytes starting from a given offset.
    pub fn write_at(&self, src: impl Read + IoBuf, offset: u64) -> VfsResult<usize> {
        self.access(FileFlags::WRITE)?.write_at(src, offset)
    }

    /// Attempts to sync OS-internal file content and metadata to disk.
    ///
    /// If `data_only` is `true`, only the file data is synced, not the
    /// metadata.
    pub fn sync(&self, data_only: bool) -> VfsResult<()> {
        self.access(FileFlags::empty())?;
        self.inner.sync(data_only)
    }

    /// Reads data from the current position, advancing the cursor.
    pub fn read(&self, dst: impl Write + IoBufMut) -> ax_io::Result<usize> {
        #[cfg(feature = "times")]
        {
            self.access_flags.fetch_or(1, Ordering::AcqRel);
        }
        if let Some(pos) = self.position.as_ref() {
            let mut pos = pos.lock();
            self.read_at(dst, *pos).inspect(|n| {
                *pos += *n as u64;
            })
        } else {
            self.read_at(dst, 0)
        }
    }

    /// Writes data at the current position (or appends), advancing the cursor.
    pub fn write(&self, src: impl Read + IoBuf) -> ax_io::Result<usize> {
        #[cfg(feature = "times")]
        {
            self.access_flags.fetch_or(3, Ordering::AcqRel);
        }
        // WRITE bit is mandatory for any write path, regardless of whether
        // APPEND is set. Otherwise O_RDONLY|O_APPEND fd would silently
        // succeed writes (since access(APPEND) only checks the APPEND bit).
        // Fixes bug-open-rdonly-append-promotes-rw (the part inside axfs).
        self.access(FileFlags::WRITE)?;
        if let Some(pos) = self.position.as_ref() {
            let mut pos = pos.lock();
            if let Ok(f) = self.access(FileFlags::APPEND) {
                f.append(src).map(|(written, new_size)| {
                    *pos = new_size;
                    written
                })
            } else {
                self.write_at(src, *pos).inspect(|n| {
                    *pos += *n as u64;
                })
            }
        } else {
            self.write_at(src, 0)
        }
    }

    /// Flushes any internally buffered data. Currently a no-op.
    pub fn flush(&self) -> ax_io::Result {
        self.access(FileFlags::empty())?;
        Ok(())
    }
}

impl Read for &File {
    fn read(&mut self, buf: &mut [u8]) -> ax_io::Result<usize> {
        (*self).read(buf)
    }
}

impl Write for &File {
    fn write(&mut self, buf: &[u8]) -> ax_io::Result<usize> {
        (*self).write(buf)
    }

    fn flush(&mut self) -> ax_io::Result {
        (*self).flush()
    }
}

impl Seek for &File {
    fn seek(&mut self, pos: SeekFrom) -> ax_io::Result<u64> {
        self.access(FileFlags::empty())?;

        if let Some(guard) = self.position.as_ref() {
            let mut guard = guard.lock();
            let new_pos = match pos {
                SeekFrom::Start(pos) => pos,
                SeekFrom::End(off) => {
                    let size = self.inner.len()?;
                    size.checked_add_signed(off).ok_or(VfsError::InvalidInput)?
                }
                SeekFrom::Current(off) => guard
                    .checked_add_signed(off)
                    .ok_or(VfsError::InvalidInput)?,
            };
            *guard = new_pos;
            Ok(new_pos)
        } else {
            Ok(0)
        }
    }
}

impl Pollable for File {
    fn poll(&self) -> IoEvents {
        self.inner.location().poll()
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        self.inner.location().register(context, events)
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    #[test]
    fn writeback_protect_listener_runs_without_cached_io_lock() {
        let shared = Arc::new(CachedFileShared::new_unbounded(0));
        let observed_unlocked = Arc::new(AtomicBool::new(false));
        let observed = observed_unlocked.clone();
        let listener_shared = shared.clone();

        shared
            .evict_listeners
            .lock()
            .push_back(Box::new(EvictListener {
                listener: Arc::new(|_, _| true),
                writeback_protect: Arc::new(move |_| {
                    observed.store(
                        listener_shared.io_lock_is_free_for_test(),
                        Ordering::Release,
                    );
                    true
                }),
                link: LinkedListAtomicLink::new(),
            }));

        shared.invoke_writeback_protect_for_test(&[0]).unwrap();

        assert!(observed_unlocked.load(Ordering::Acquire));
    }

    #[test]
    fn writeback_protect_listener_runs_without_listener_lock() {
        let shared = Arc::new(CachedFileShared::new_unbounded(0));
        let observed_unlocked = Arc::new(AtomicBool::new(false));
        let observed = observed_unlocked.clone();
        let listener_shared = shared.clone();

        shared
            .evict_listeners
            .lock()
            .push_back(Box::new(EvictListener {
                listener: Arc::new(|_, _| true),
                writeback_protect: Arc::new(move |_| {
                    observed.store(
                        listener_shared.listener_lock_is_free_for_test(),
                        Ordering::Release,
                    );
                    true
                }),
                link: LinkedListAtomicLink::new(),
            }));

        shared.invoke_writeback_protect_for_test(&[0]).unwrap();

        assert!(observed_unlocked.load(Ordering::Acquire));
    }

    #[test]
    fn writeback_protect_does_not_hold_listener_lock_while_invoking_callbacks() {
        let shared = Arc::new(CachedFileShared::new_unbounded(0));
        let observed_unlocked = Arc::new(AtomicBool::new(false));
        let observed = observed_unlocked.clone();
        let listener_shared = shared.clone();

        shared
            .evict_listeners
            .lock()
            .push_back(Box::new(EvictListener {
                listener: Arc::new(|_, _| true),
                writeback_protect: Arc::new(move |_| {
                    observed.store(
                        listener_shared.evict_listeners.try_lock().is_some(),
                        Ordering::Release,
                    );
                    true
                }),
                link: LinkedListAtomicLink::new(),
            }));

        shared.protect_dirty_pages_before_writeback(&[0]).unwrap();

        assert!(observed_unlocked.load(Ordering::Acquire));
    }
}

#[cfg(feature = "times")]
impl Drop for File {
    fn drop(&mut self) {
        let flags = self.access_flags.load(Ordering::Acquire);
        if flags != 0 {
            let mut update = axfs_ng_vfs::MetadataUpdate::default();
            if flags & 1 != 0 {
                update.atime = Some(crate::os::wall_time());
            }
            if flags & 2 != 0 {
                update.mtime = Some(crate::os::wall_time());
            }
            if let Err(err) = self.inner.location().update_metadata(update) {
                warn!("Failed to update file times on drop: {err:?}");
            }
        }
    }
}
