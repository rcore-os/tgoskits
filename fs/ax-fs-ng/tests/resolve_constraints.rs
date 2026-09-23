//! Integration tests for the openat2-style path-walk constraints
//! ([`ResolveConstraints`]) enforced by `FsContext`.
//!
//! The fixture mounts a small in-memory tree plus a second filesystem at
//! `/a/mnt` so the `RESOLVE_NO_XDEV` mount-boundary cases exercise real mount
//! crossings:
//!
//! ```text
//! /                     (test filesystem root)
//! └── a/
//!     ├── b/
//!     │   └── f/         (plain subdirectory)
//!     ├── rel -> b       (relative symlink)
//!     ├── abs -> /b      (absolute symlink)
//!     ├── up -> ..       (parent symlink)
//!     ├── magic -> /secret (symlink flagged MAGIC_LINK)
//!     └── mnt/           (mountpoint of the second filesystem)
//! ```

use std::{any::Any, collections::BTreeMap, sync::Arc, time::Duration};

use ax_fs_ng::vfs::{FsContext, ResolveConstraints};
use axfs_ng_vfs::{
    DirEntry, DirEntrySink, DirNode, DirNodeOps, DirectoryCursor, FileNode, Filesystem,
    FilesystemOps, Location, Metadata, MetadataUpdate, NodeFlags, NodePermission, NodeType,
    Reference, RenameOptions, VfsError, VfsResult, WeakDirEntry,
};
use axpoll::IoEvents;

/// The tree shape of the fixture filesystem.
#[derive(Clone)]
enum Tree {
    Dir(BTreeMap<String, Tree>),
    Symlink { target: String, magic: bool },
}

impl Tree {
    fn dir(entries: &[(&str, Tree)]) -> Tree {
        Tree::Dir(
            entries
                .iter()
                .map(|(name, tree)| ((*name).to_string(), tree.clone()))
                .collect(),
        )
    }
}

fn fixture_tree() -> Tree {
    Tree::dir(&[(
        "a",
        Tree::dir(&[
            ("b", Tree::dir(&[("f", Tree::Dir(BTreeMap::new()))])),
            (
                "rel",
                Tree::Symlink {
                    target: "b".into(),
                    magic: false,
                },
            ),
            (
                "abs",
                Tree::Symlink {
                    target: "/b".into(),
                    magic: false,
                },
            ),
            (
                "up",
                Tree::Symlink {
                    target: "..".into(),
                    magic: false,
                },
            ),
            (
                "magic",
                Tree::Symlink {
                    target: "/secret".into(),
                    magic: true,
                },
            ),
            ("mnt", Tree::Dir(BTreeMap::new())),
        ]),
    )])
}

/// Directory node serving the fixture tree lazily. The `WeakDirEntry` captured
/// at construction only upgrades from the first lookup onwards, which is what
/// lets child entries carry a real parent reference.
struct TestDir {
    tree: Tree,
    self_weak: WeakDirEntry,
    ino: u64,
}

impl TestDir {
    fn entry(&self, name: &str) -> VfsResult<DirEntry> {
        let Tree::Dir(entries) = &self.tree else {
            return Err(VfsError::NotFound);
        };
        let spec = entries.get(name).ok_or(VfsError::NotFound)?;
        let parent = self.self_weak.upgrade().ok_or(VfsError::NotFound)?;
        match spec {
            Tree::Dir(_) => Ok(DirEntry::new_dir(
                |weak| {
                    DirNode::new(Arc::new(TestDir {
                        tree: spec.clone(),
                        self_weak: weak,
                        ino: self.ino,
                    }))
                },
                Reference::new(Some(parent), name.to_string()),
            )),
            Tree::Symlink { target, magic } => Ok(DirEntry::new_file(
                FileNode::new(Arc::new(TestSymlink {
                    target: target.clone(),
                    magic: *magic,
                    ino: self.ino,
                })),
                NodeType::Symlink,
                Reference::new(Some(parent), name.to_string()),
            )),
        }
    }
}

impl axfs_ng_vfs::NodeOps for TestDir {
    fn inode(&self) -> u64 {
        self.ino
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        Ok(mock_metadata(self.ino, NodeType::Directory))
    }

    fn update_metadata(&self, _update: MetadataUpdate) -> VfsResult<()> {
        Err(VfsError::ReadOnlyFilesystem)
    }

    fn filesystem(&self) -> &dyn FilesystemOps {
        &TEST_FS
    }

    fn sync(&self, _data_only: bool) -> VfsResult<()> {
        Ok(())
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

impl DirNodeOps for TestDir {
    fn read_dir(&self, _cursor: DirectoryCursor, _sink: &mut dyn DirEntrySink) -> VfsResult<usize> {
        Err(VfsError::OperationNotSupported)
    }

    fn lookup(&self, name: &str) -> VfsResult<DirEntry> {
        self.entry(name)
    }

    fn create(
        &self,
        _name: &str,
        _node_type: NodeType,
        _permission: NodePermission,
        _uid: u32,
        _gid: u32,
    ) -> VfsResult<DirEntry> {
        Err(VfsError::ReadOnlyFilesystem)
    }

    fn create_symlink(
        &self,
        _name: &str,
        _target: &str,
        _permission: NodePermission,
        _uid: u32,
        _gid: u32,
    ) -> VfsResult<DirEntry> {
        Err(VfsError::ReadOnlyFilesystem)
    }

    fn link(&self, _name: &str, _node: &DirEntry) -> VfsResult<DirEntry> {
        Err(VfsError::ReadOnlyFilesystem)
    }

    fn unlink(&self, _name: &str, _is_dir: bool) -> VfsResult<()> {
        Err(VfsError::ReadOnlyFilesystem)
    }

    fn rename(
        &self,
        _src: &str,
        _dst_dir: &DirNode,
        _dst: &str,
        _options: RenameOptions,
    ) -> VfsResult<()> {
        Err(VfsError::ReadOnlyFilesystem)
    }
}

/// Symlink payload: the target string doubles as the file content, matching
/// how `read_link` reads the node's bytes.
struct TestSymlink {
    target: String,
    magic: bool,
    ino: u64,
}

impl axfs_ng_vfs::NodeOps for TestSymlink {
    fn inode(&self) -> u64 {
        self.ino
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        let mut metadata = mock_metadata(self.ino, NodeType::Symlink);
        metadata.size = self.target.len() as u64;
        Ok(metadata)
    }

    fn update_metadata(&self, _update: MetadataUpdate) -> VfsResult<()> {
        Err(VfsError::ReadOnlyFilesystem)
    }

    fn filesystem(&self) -> &dyn FilesystemOps {
        &TEST_FS
    }

    fn sync(&self, _data_only: bool) -> VfsResult<()> {
        Ok(())
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn flags(&self) -> NodeFlags {
        if self.magic {
            NodeFlags::NON_CACHEABLE | NodeFlags::MAGIC_LINK
        } else {
            NodeFlags::NON_CACHEABLE
        }
    }
}

impl axpoll::Pollable for TestSymlink {
    fn poll(&self) -> IoEvents {
        IoEvents::empty()
    }

    unsafe fn register_shared(
        &self,
        _sink: &mut dyn axpoll::SharedRegistrationSink,
        _events: IoEvents,
    ) {
    }

    unsafe fn register_exclusive(
        &self,
        _sink: &mut dyn axpoll::ExclusiveRegistrationSink,
        _events: IoEvents,
    ) {
    }
}

impl axfs_ng_vfs::FileNodeOps for TestSymlink {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let offset = usize::try_from(offset).map_err(|_| VfsError::InvalidInput)?;
        let Some(remaining) = self.target.as_bytes().get(offset..) else {
            return Ok(0);
        };
        let length = remaining.len().min(buf.len());
        buf[..length].copy_from_slice(&remaining[..length]);
        Ok(length)
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Err(VfsError::ReadOnlyFilesystem)
    }

    fn append(&self, _buf: &[u8]) -> VfsResult<(usize, u64)> {
        Err(VfsError::ReadOnlyFilesystem)
    }

    fn set_len(&self, _len: u64) -> VfsResult<()> {
        Err(VfsError::ReadOnlyFilesystem)
    }
}

/// The shared filesystem-identity every fixture node reports.
static TEST_FS: TestFs = TestFs;

struct TestFs;

impl FilesystemOps for TestFs {
    fn name(&self) -> &str {
        "test"
    }

    fn root_dir(&self) -> DirEntry {
        DirEntry::new_dir(
            |weak| {
                DirNode::new(Arc::new(TestDir {
                    tree: fixture_tree(),
                    self_weak: weak,
                    ino: 1,
                }))
            },
            Reference::root(),
        )
    }

    fn stat(&self) -> VfsResult<axfs_ng_vfs::StatFs> {
        Err(VfsError::OperationNotSupported)
    }
}

fn mock_metadata(ino: u64, node_type: NodeType) -> Metadata {
    Metadata {
        device: 0,
        inode: ino,
        nlink: 1,
        mode: NodePermission::from_bits_truncate(0o755),
        node_type,
        uid: 0,
        gid: 0,
        size: 0,
        block_size: 4096,
        blocks: 0,
        rdev: axfs_ng_vfs::DeviceId(0),
        atime: Duration::ZERO,
        mtime: Duration::ZERO,
        ctime: Duration::ZERO,
    }
}

/// Builds the fixture: the test filesystem with an empty filesystem mounted
/// at `/a/mnt`, plus an `FsContext` rooted at `/`.
fn fixture() -> FsContext {
    let root = axfs_ng_vfs::Mountpoint::new_root(&Filesystem::new(Arc::new(TestFs)));
    let root_location = root.root_location();
    let a = root_location.lookup_no_follow("a").expect("a exists");
    let mnt = a.lookup_no_follow("mnt").expect("mnt exists");
    mnt.mount(&Filesystem::new(Arc::new(EmptyFs)))
        .expect("mount succeeds");
    FsContext::new(root_location)
}

/// An empty directory tree for the second mount.
struct EmptyFs;

impl FilesystemOps for EmptyFs {
    fn name(&self) -> &str {
        "empty"
    }

    fn root_dir(&self) -> DirEntry {
        DirEntry::new_dir(
            |weak| {
                DirNode::new(Arc::new(TestDir {
                    tree: Tree::Dir(BTreeMap::new()),
                    self_weak: weak,
                    ino: 100,
                }))
            },
            Reference::root(),
        )
    }

    fn stat(&self) -> VfsResult<axfs_ng_vfs::StatFs> {
        Err(VfsError::OperationNotSupported)
    }
}

/// Returns an `FsContext` whose current directory is `/a` (the usual dirfd)
/// and that location itself.
fn at_a() -> (FsContext, Location) {
    let context = fixture();
    let root = context.root_dir().clone();
    let a = root.lookup_no_follow("a").expect("a exists");
    (context.with_current_dir(a.clone()).expect("cwd to /a"), a)
}

fn no_checks(_: &Location) -> VfsResult<()> {
    Ok(())
}

fn resolve(
    context: &FsContext,
    path: &str,
    constraints: &ResolveConstraints,
) -> VfsResult<Location> {
    context.resolve_with_constraints(path, constraints, true, no_checks, 0)
}

fn error_of(context: &FsContext, path: &str, constraints: &ResolveConstraints) -> VfsError {
    resolve(context, path, constraints).expect_err("resolution must fail")
}

mod beneath {
    use super::*;

    #[test]
    fn relative_paths_stay_inside_the_base() {
        let (context, a) = at_a();
        let b = a.lookup_no_follow("b").unwrap();
        assert!(
            resolve(&context, "b", &ResolveConstraints::new().beneath())
                .unwrap()
                .ptr_eq(&b)
        );
        assert!(resolve(&context, "b/f", &ResolveConstraints::new().beneath()).is_ok());
        // A `..` that lands back on the base is allowed.
        let up_down = resolve(&context, "b/..", &ResolveConstraints::new().beneath()).unwrap();
        assert!(up_down.ptr_eq(&a));
    }

    #[test]
    fn absolute_paths_are_rejected() {
        let (context, _) = at_a();
        assert_eq!(
            error_of(&context, "/b", &ResolveConstraints::new().beneath()),
            VfsError::CrossesDevices
        );
    }

    #[test]
    fn climbing_above_the_base_is_rejected() {
        let (context, _) = at_a();
        assert_eq!(
            error_of(&context, "..", &ResolveConstraints::new().beneath()),
            VfsError::CrossesDevices
        );
        assert_eq!(
            error_of(&context, "b/../..", &ResolveConstraints::new().beneath()),
            VfsError::CrossesDevices
        );
    }

    #[test]
    fn symlinks_may_not_escape_the_base() {
        let (context, a) = at_a();
        // `up` -> `..` climbs above the base.
        assert_eq!(
            error_of(&context, "up", &ResolveConstraints::new().beneath()),
            VfsError::CrossesDevices
        );
        // An absolute symlink target leaves the base.
        assert_eq!(
            error_of(&context, "abs", &ResolveConstraints::new().beneath()),
            VfsError::CrossesDevices
        );
        // A relative symlink stays inside and resolves normally.
        let b = a.lookup_no_follow("b").unwrap();
        assert!(
            resolve(&context, "rel", &ResolveConstraints::new().beneath())
                .unwrap()
                .ptr_eq(&b)
        );
    }
}

mod in_root {
    use super::*;

    #[test]
    fn absolute_paths_restart_at_the_root() {
        let (context, a) = at_a();
        let constraints = ResolveConstraints::new().in_root(a.clone());
        // `/b` under root=/a is /a/b.
        let b = a.lookup_no_follow("b").unwrap();
        assert!(resolve(&context, "/b", &constraints).unwrap().ptr_eq(&b));
    }

    #[test]
    fn dot_dot_clamps_at_the_root() {
        let (context, a) = at_a();
        let constraints = ResolveConstraints::new().in_root(a.clone());
        // `b/../..` would leave /a; it clamps back to /a instead.
        assert!(
            resolve(&context, "b/../..", &constraints)
                .unwrap()
                .ptr_eq(&a)
        );
    }

    #[test]
    fn beneath_and_in_root_stay_at_the_base() {
        let (context, a) = at_a();
        let constraints = ResolveConstraints::new().in_root(a.clone()).beneath();
        assert!(resolve(&context, "..", &constraints).unwrap().ptr_eq(&a));
    }

    #[test]
    fn symlink_targets_stay_inside_the_root() {
        let (context, a) = at_a();
        let constraints = ResolveConstraints::new().in_root(a.clone());
        // `abs` -> `/b` restarts at the constraint root, landing on /a/b.
        let b = a.lookup_no_follow("b").unwrap();
        assert!(resolve(&context, "abs", &constraints).unwrap().ptr_eq(&b));
    }
}

mod no_xdev {
    use super::*;

    #[test]
    fn crossing_into_the_mount_is_rejected() {
        let (context, _) = at_a();
        assert_eq!(
            error_of(&context, "mnt", &ResolveConstraints::new().no_xdev()),
            VfsError::CrossesDevices
        );
    }

    #[test]
    fn same_mount_paths_are_allowed() {
        let (context, _) = at_a();
        assert!(resolve(&context, "b/f", &ResolveConstraints::new().no_xdev()).is_ok());
    }

    #[test]
    fn climbing_out_of_a_mount_is_rejected() {
        let (context, a) = at_a();
        let mnt = resolve(&context, "mnt", &ResolveConstraints::new()).unwrap();
        let inside = context.with_current_dir(mnt).unwrap();
        // `..` from inside the mount crosses back into /a's filesystem.
        assert_eq!(
            error_of(&inside, "..", &ResolveConstraints::new().no_xdev()),
            VfsError::CrossesDevices
        );
        // The same walk is fine without the constraint.
        let escaped = resolve(&inside, "..", &ResolveConstraints::new()).unwrap();
        assert!(escaped.ptr_eq(&a));
    }
}

mod no_symlinks {
    use super::*;

    #[test]
    fn any_symlink_is_rejected() {
        let (context, _) = at_a();
        let constraints = ResolveConstraints::new().no_symlinks();
        assert_eq!(
            error_of(&context, "rel", &constraints),
            VfsError::FilesystemLoop
        );
        assert_eq!(
            error_of(&context, "rel/f", &constraints),
            VfsError::FilesystemLoop
        );
        assert_eq!(
            error_of(&context, "magic", &constraints),
            VfsError::FilesystemLoop
        );
        // Plain paths still resolve.
        assert!(resolve(&context, "b/f", &constraints).is_ok());
    }
}

mod no_magiclinks {
    use super::*;

    #[test]
    fn magic_links_are_rejected_but_symlinks_are_followed() {
        let (context, a) = at_a();
        let constraints = ResolveConstraints::new().no_magiclinks();
        assert_eq!(
            error_of(&context, "magic", &constraints),
            VfsError::FilesystemLoop
        );
        let b = a.lookup_no_follow("b").unwrap();
        assert!(resolve(&context, "rel", &constraints).unwrap().ptr_eq(&b));

        // Without the constraint the magic link is followed by its displayed
        // target (here: a nonexistent absolute path).
        assert_eq!(
            error_of(&context, "magic", &ResolveConstraints::new()),
            VfsError::NotFound
        );
    }

    #[test]
    fn magic_flag_is_visible_through_locations() {
        let (_, a) = at_a();
        let magic = a.lookup_no_follow("magic").unwrap();
        assert!(magic.is_magic_link());
        let rel = a.lookup_no_follow("rel").unwrap();
        assert!(!rel.is_magic_link());
    }
}

mod cached {
    use super::*;

    #[test]
    fn cached_only_behaves_like_a_plain_resolve() {
        let (context, a) = at_a();
        let b = a.lookup_no_follow("b").unwrap();
        assert!(
            resolve(&context, "b", &ResolveConstraints::new())
                .unwrap()
                .ptr_eq(&b)
        );
    }
}

/// Host-side lock provider: the fixture exercises sleepable-lock paths in
/// `ax-sync` through the same spin-based stub the other host test suites use.
mod host_locks {
    use core::{
        panic::Location,
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use ax_sync::interface::{AcquireResult, ContextState, LOCK_MODE_READ, LockMetadata};

    struct HostLocks;

    #[ax_crate_interface::impl_interface]
    impl ax_sync::interface::SpinOps for HostLocks {
        fn acquire(
            locked: &AtomicBool,
            _metadata: &LockMetadata,
            _addr: usize,
            _context: u8,
            _subclass: u32,
            _caller: &'static Location<'static>,
        ) -> ContextState {
            while locked
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                core::hint::spin_loop();
            }
            ContextState::new(0, 0)
        }

        fn try_acquire(
            locked: &AtomicBool,
            _metadata: &LockMetadata,
            _addr: usize,
            _context: u8,
            _subclass: u32,
            _caller: &'static Location<'static>,
        ) -> AcquireResult {
            AcquireResult::new(
                locked
                    .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok(),
                ContextState::new(0, 0),
            )
        }

        fn release(locked: &AtomicBool, _addr: usize, _context: u8, _state: ContextState) {
            locked.store(false, Ordering::Release);
        }

        fn force_release(locked: &AtomicBool, _addr: usize, _context: u8) {
            locked.store(false, Ordering::Release);
        }

        fn is_locked(locked: &AtomicBool) -> bool {
            locked.load(Ordering::Relaxed)
        }
    }

    const WRITER: usize = 1 << (usize::BITS - 1);

    fn try_rwlock(state: &AtomicUsize, mode: u8) -> bool {
        if mode != LOCK_MODE_READ {
            return state
                .compare_exchange(0, WRITER, Ordering::Acquire, Ordering::Relaxed)
                .is_ok();
        }
        state
            .try_update(Ordering::Acquire, Ordering::Relaxed, |readers| {
                (readers < WRITER - 1).then(|| readers + 1)
            })
            .is_ok()
    }

    #[ax_crate_interface::impl_interface]
    impl ax_sync::interface::RwLockOps for HostLocks {
        fn acquire(
            state: &AtomicUsize,
            _metadata: &LockMetadata,
            _addr: usize,
            _context: u8,
            mode: u8,
            _caller: &'static Location<'static>,
        ) -> ContextState {
            while !try_rwlock(state, mode) {
                core::hint::spin_loop();
            }
            ContextState::new(0, 0)
        }

        fn try_acquire(
            state: &AtomicUsize,
            _metadata: &LockMetadata,
            _addr: usize,
            _context: u8,
            mode: u8,
            _caller: &'static Location<'static>,
        ) -> AcquireResult {
            AcquireResult::new(try_rwlock(state, mode), ContextState::new(0, 0))
        }

        fn release(
            state: &AtomicUsize,
            _addr: usize,
            _context: u8,
            _context_state: ContextState,
            mode: u8,
        ) {
            if mode == LOCK_MODE_READ {
                state.fetch_sub(1, Ordering::Release);
            } else {
                state.store(0, Ordering::Release);
            }
        }

        fn force_read_decrement(state: &AtomicUsize, _addr: usize, _context: u8) {
            state.fetch_sub(1, Ordering::Release);
        }
    }
}
