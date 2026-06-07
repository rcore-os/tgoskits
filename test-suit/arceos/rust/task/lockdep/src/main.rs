#![cfg_attr(any(feature = "ax-std", target_os = "none"), no_std)]
#![cfg_attr(any(feature = "ax-std", target_os = "none"), no_main)]

#[cfg(any(not(target_os = "none"), feature = "ax-std"))]
macro_rules! app {
    ($($item:item)*) => {
        $($item)*
    };
}

#[cfg(not(any(not(target_os = "none"), feature = "ax-std")))]
macro_rules! app {
    ($($item:item)*) => {};
}

app! {

#[macro_use]
#[cfg(feature = "ax-std")]
extern crate ax_std as std;

#[cfg(feature = "ax-std")]
use core::any::Any;

#[cfg(feature = "ax-std")]
use std::{
    string::ToString,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
};

#[cfg(feature = "ax-std")]
use ax_kspin::SpinRaw;

#[cfg(feature = "ax-std")]
use axfs_ng_vfs::{
    DeviceId, DirEntry, DirEntrySink, DirNode, DirNodeOps, FilesystemOps, Metadata,
    MetadataUpdate, NodeFlags, NodeOps, NodePermission, NodeType, Reference, StatFs, VfsError,
    VfsResult, WeakDirEntry,
};

#[cfg(feature = "ax-std")]
const WAIT_UNTIL_RETRY_LIMIT: usize = 10_000_000;

// Manual lockdep cases covered by this app:
// - mutex-single: single-task mutex ABBA
// - mutex-two-task: two-task mutex ABBA
// - spin-single: single-task spin ABBA
// - spin-two-task: two-task spin ABBA
// - mixed-single: single-task spin->mutex then mutex->spin
// - mixed-two-task: two-task spin->mutex then mutex->spin
// - mixed-ms-single: single-task mutex->spin then spin->mutex
// - mixed-ms-two-task: two-task mutex->spin then spin->mutex
// - vfs-cache-single: single-task axfs-ng-vfs dentry cache regression
#[cfg(feature = "ax-std")]
fn lockdep_case() -> &'static str {
    match option_env!("LOCKDEP_CASE") {
        Some(case) => case,
        None => panic!(
            "LOCKDEP_CASE is required; choose one of: mutex-single, mutex-two-task, spin-single, \
             spin-two-task, mixed-single, mixed-two-task, mixed-ms-single, mixed-ms-two-task, \
             vfs-cache-single"
        ),
    }
}

#[cfg(feature = "ax-std")]
fn wait_until(stage: &AtomicUsize, expected: usize) {
    for _ in 0..WAIT_UNTIL_RETRY_LIMIT {
        if stage.load(Ordering::Acquire) == expected {
            return;
        }
        thread::yield_now();
    }
    panic!("wait_until timeout waiting for stage {expected}");
}

#[cfg(feature = "ax-std")]
fn mutex_single_task_abba() {
    let lock_a = Mutex::new(0usize);
    let lock_b = Mutex::new(0usize);

    {
        let _guard_a = lock_a.lock();
        let _guard_b = lock_b.lock();
        println!("mutex-single: recorded A -> B");
    }

    let _guard_b = lock_b.lock();
    assert!(
        lock_a.try_lock().is_some(),
        "try_lock(A) unexpectedly failed without lockdep"
    );
}

#[cfg(feature = "ax-std")]
fn mutex_two_task_abba() {
    let lock_a = Arc::new(Mutex::new(0usize));
    let lock_b = Arc::new(Mutex::new(0usize));
    let stage = Arc::new(AtomicUsize::new(0));

    let thread_lock_a = lock_a.clone();
    let thread_lock_b = lock_b.clone();
    let thread_stage = stage.clone();

    let handle = thread::spawn(move || {
        {
            let _guard_a = thread_lock_a.lock();
            let _guard_b = thread_lock_b.lock();
            println!("mutex-two-task: thread AB recorded A -> B");
        }
        thread_stage.store(1, Ordering::Release);
    });

    wait_until(&stage, 1);
    let _guard_b = lock_b.lock();
    assert!(
        lock_a.try_lock().is_some(),
        "try_lock(A) unexpectedly failed without lockdep"
    );
    handle.join().unwrap();
}

#[cfg(feature = "ax-std")]
fn spin_single_task_abba() {
    let lock_a = SpinRaw::new(0usize);
    let lock_b = SpinRaw::new(0usize);

    {
        let _guard_a = lock_a.lock();
        let _guard_b = lock_b.lock();
        println!("spin-single: recorded A -> B");
    }

    let _guard_b = lock_b.lock();
    assert!(
        lock_a.try_lock().is_some(),
        "try_lock(A) unexpectedly failed without lockdep"
    );
}

#[cfg(feature = "ax-std")]
fn spin_two_task_abba() {
    let lock_a = Arc::new(SpinRaw::new(0usize));
    let lock_b = Arc::new(SpinRaw::new(0usize));
    let stage = Arc::new(AtomicUsize::new(0));

    let thread_lock_a = lock_a.clone();
    let thread_lock_b = lock_b.clone();
    let thread_stage = stage.clone();

    let handle = thread::spawn(move || {
        let _guard_a = thread_lock_a.lock();
        let _guard_b = thread_lock_b.lock();
        println!("spin-two-task: thread AB recorded A -> B");
        thread_stage.store(1, Ordering::Release);
    });

    wait_until(&stage, 1);
    let _guard_b = lock_b.lock();
    assert!(
        lock_a.try_lock().is_some(),
        "try_lock(A) unexpectedly failed without lockdep"
    );
    handle.join().unwrap();
}

#[cfg(feature = "ax-std")]
fn mixed_single_task_abba() {
    let lock_a = SpinRaw::new(0usize);
    let lock_b = Mutex::new(0usize);

    {
        let _guard_a = lock_a.lock();
        let _guard_b = lock_b.lock();
        println!("mixed-single: recorded spin A -> mutex B");
    }

    let _guard_b = lock_b.lock();
    assert!(
        lock_a.try_lock().is_some(),
        "try_lock(A) unexpectedly failed without lockdep"
    );
}

#[cfg(feature = "ax-std")]
fn mixed_two_task_abba() {
    let lock_a = Arc::new(SpinRaw::new(0usize));
    let lock_b = Arc::new(Mutex::new(0usize));
    let stage = Arc::new(AtomicUsize::new(0));

    let thread_lock_a = lock_a.clone();
    let thread_lock_b = lock_b.clone();
    let thread_stage = stage.clone();

    let handle = thread::spawn(move || {
        {
            let _guard_a = thread_lock_a.lock();
            let _guard_b = thread_lock_b.lock();
            println!("mixed-two-task: thread AB recorded spin A -> mutex B");
        }
        thread_stage.store(1, Ordering::Release);
    });

    wait_until(&stage, 1);
    let _guard_b = lock_b.lock();
    assert!(
        lock_a.try_lock().is_some(),
        "try_lock(A) unexpectedly failed without lockdep"
    );
    handle.join().unwrap();
}

#[cfg(feature = "ax-std")]
fn mixed_ms_single_task_abba() {
    let lock_a = Mutex::new(0usize);
    let lock_b = SpinRaw::new(0usize);

    {
        let _guard_a = lock_a.lock();
        let _guard_b = lock_b.lock();
        println!("mixed-ms-single: recorded mutex A -> spin B");
    }

    let _guard_b = lock_b.lock();
    assert!(
        lock_a.try_lock().is_some(),
        "try_lock(A) unexpectedly failed without lockdep"
    );
}

#[cfg(feature = "ax-std")]
fn mixed_ms_two_task_abba() {
    let lock_a = Arc::new(Mutex::new(0usize));
    let lock_b = Arc::new(SpinRaw::new(0usize));
    let stage = Arc::new(AtomicUsize::new(0));

    let thread_lock_a = lock_a.clone();
    let thread_lock_b = lock_b.clone();
    let thread_stage = stage.clone();

    let handle = thread::spawn(move || {
        {
            let _guard_a = thread_lock_a.lock();
            let _guard_b = thread_lock_b.lock();
            println!("mixed-ms-two-task: thread AB recorded mutex A -> spin B");
        }
        thread_stage.store(1, Ordering::Release);
    });

    wait_until(&stage, 1);
    let _guard_b = lock_b.lock();
    assert!(
        lock_a.try_lock().is_some(),
        "try_lock(A) unexpectedly failed without lockdep"
    );
    handle.join().unwrap();
}

#[cfg(feature = "ax-std")]
struct TestFs;

#[cfg(feature = "ax-std")]
impl FilesystemOps for TestFs {
    fn name(&self) -> &str {
        "lockdep-vfs-test"
    }

    fn root_dir(&self) -> DirEntry {
        panic!("not used by lockdep-vfs-test")
    }

    fn stat(&self) -> VfsResult<StatFs> {
        Err(VfsError::Unsupported)
    }
}

#[cfg(feature = "ax-std")]
static TEST_FS: TestFs = TestFs;

#[cfg(feature = "ax-std")]
struct TestDir {
    inode: u64,
    this: WeakDirEntry,
    renamed: AtomicBool,
}

#[cfg(feature = "ax-std")]
impl TestDir {
    fn new_child(&self, name: &str) -> DirEntry {
        DirEntry::new_dir(
            |this| {
                DirNode::new(Arc::new(Self {
                    inode: self.inode + 100,
                    this,
                    renamed: AtomicBool::new(false),
                }))
            },
            Reference::new(self.this.upgrade(), name.to_string()),
        )
    }

    fn new_entry(inode: u64, name: &str) -> DirEntry {
        DirEntry::new_dir(
            |this| {
                DirNode::new(Arc::new(Self {
                    inode,
                    this,
                    renamed: AtomicBool::new(false),
                }))
            },
            Reference::new(None, name.to_string()),
        )
    }
}

#[cfg(feature = "ax-std")]
impl NodeOps for TestDir {
    fn inode(&self) -> u64 {
        self.inode
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        Ok(Metadata {
            device: 0,
            inode: self.inode,
            nlink: 1,
            mode: NodePermission::default(),
            node_type: NodeType::Directory,
            uid: 0,
            gid: 0,
            size: 0,
            block_size: 4096,
            blocks: 0,
            rdev: DeviceId::default(),
            atime: Default::default(),
            mtime: Default::default(),
            ctime: Default::default(),
        })
    }

    fn update_metadata(&self, _update: MetadataUpdate) -> VfsResult<()> {
        Ok(())
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
        NodeFlags::empty()
    }
}

#[cfg(feature = "ax-std")]
impl DirNodeOps for TestDir {
    fn read_dir(&self, _offset: u64, _sink: &mut dyn DirEntrySink) -> VfsResult<usize> {
        Ok(0)
    }

    fn lookup(&self, name: &str) -> VfsResult<DirEntry> {
        if name == "new" && !self.renamed.load(Ordering::Acquire) {
            return Err(VfsError::NotFound);
        }
        Ok(self.new_child(name))
    }

    fn create(
        &self,
        _name: &str,
        _node_type: NodeType,
        _permission: NodePermission,
        _uid: u32,
        _gid: u32,
    ) -> VfsResult<DirEntry> {
        Err(VfsError::Unsupported)
    }

    fn link(&self, _name: &str, _node: &DirEntry) -> VfsResult<DirEntry> {
        Err(VfsError::Unsupported)
    }

    fn unlink(&self, _name: &str, _is_dir: bool) -> VfsResult<()> {
        Err(VfsError::Unsupported)
    }

    fn rename(&self, _src_name: &str, _dst_dir: &DirNode, dst_name: &str) -> VfsResult<()> {
        if dst_name == "new" {
            self.renamed.store(true, Ordering::Release);
        }
        Ok(())
    }
}

#[cfg(feature = "ax-std")]
fn vfs_cache_single_task_abba() {
    let dir = TestDir::new_entry(1, "dir");

    {
        let _guard = dir.user_data();
        let _child = dir.as_dir().unwrap().lookup("child").unwrap();
        println!("vfs-cache-single: exercised dentry user_data -> dir cache");
    }

    dir.as_dir()
        .unwrap()
        .insert_cache("old".to_string(), TestDir::new_entry(2, "old"));
    dir.as_dir()
        .unwrap()
        .rename("old", dir.as_dir().unwrap(), "new")
        .unwrap();
    println!("vfs-cache-single: rename completed without cache/user_data inversion");
}

#[cfg(feature = "ax-std")]
fn run_case(case: &str) {
    match case {
        "mutex-single" => mutex_single_task_abba(),
        "mutex-two-task" => mutex_two_task_abba(),
        "spin-single" => spin_single_task_abba(),
        "spin-two-task" => spin_two_task_abba(),
        "mixed-single" => mixed_single_task_abba(),
        "mixed-two-task" => mixed_two_task_abba(),
        "mixed-ms-single" => mixed_ms_single_task_abba(),
        "mixed-ms-two-task" => mixed_ms_two_task_abba(),
        "vfs-cache-single" => vfs_cache_single_task_abba(),
        other => panic!("unsupported LOCKDEP_CASE: {other}"),
    }
}

#[cfg_attr(feature = "ax-std", unsafe(no_mangle))]
fn main() {
    println!("lockdep regression test start");

    #[cfg(feature = "ax-std")]
    {
        println!("running case: {}", lockdep_case());
        run_case(lockdep_case());
    }
    #[cfg(expected_lockdep)]
    panic!("lockdep did not report an expected lock order inversion");
    #[cfg(not(expected_lockdep))]
    println!("All tests passed!");
}

}
