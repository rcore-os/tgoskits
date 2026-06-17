use core::any::Any;
use std::{
    string::ToString,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    vec::Vec,
};

use ax_kspin::{SpinNoIrq, SpinRaw};
use axfs_ng_vfs::{
    DeviceId, DirEntry, DirEntrySink, DirNode, DirNodeOps, FilesystemOps, Metadata, MetadataUpdate,
    NodeFlags, NodeOps, NodePermission, NodeType, Reference, StatFs, VfsError, VfsResult,
    WeakDirEntry,
};

const WAIT_UNTIL_RETRY_LIMIT: usize = 10_000_000;

pub fn run() -> crate::TestResult {
    run_spin_no_irq_counter();
    mutex_single_task_abba();
    mutex_two_task_abba();
    spin_single_task_abba();
    spin_two_task_abba();
    mixed_single_task_abba();
    mixed_two_task_abba();
    mixed_ms_single_task_abba();
    mixed_ms_two_task_abba();
    vfs_cache_single_task_abba();
    Ok(())
}

fn run_spin_no_irq_counter() {
    static GLOBAL: SpinNoIrq<usize> = SpinNoIrq::new(0);
    *GLOBAL.lock() = 0;

    let shared = Arc::new(SpinNoIrq::new(0usize));
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let shared = shared.clone();
        tasks.push(thread::spawn(move || {
            for _ in 0..32 {
                *shared.lock() += 1;
                *GLOBAL.lock() += 1;
                thread::yield_now();
            }
        }));
    }

    for task in tasks {
        task.join().unwrap();
    }

    assert_eq!(*shared.lock(), 4 * 32);
    assert_eq!(*GLOBAL.lock(), 4 * 32);
}

fn wait_until(stage: &AtomicUsize, expected: usize) {
    for _ in 0..WAIT_UNTIL_RETRY_LIMIT {
        if stage.load(Ordering::Acquire) == expected {
            return;
        }
        thread::yield_now();
    }
    panic!("wait_until timeout waiting for stage {expected}");
}

fn mutex_single_task_abba() {
    let lock_a = Mutex::new(0usize);
    let lock_b = Mutex::new(0usize);

    {
        let _guard_a = lock_a.lock();
        let _guard_b = lock_b.lock();
    }

    let _guard_b = lock_b.lock();
    assert!(
        lock_a.try_lock().is_some(),
        "try_lock(A) unexpectedly failed without lockdep"
    );
}

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

fn spin_single_task_abba() {
    let lock_a = SpinRaw::new(0usize);
    let lock_b = SpinRaw::new(0usize);

    {
        let _guard_a = lock_a.lock();
        let _guard_b = lock_b.lock();
    }

    let _guard_b = lock_b.lock();
    assert!(
        lock_a.try_lock().is_some(),
        "try_lock(A) unexpectedly failed without lockdep"
    );
}

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

fn mixed_single_task_abba() {
    let lock_a = SpinRaw::new(0usize);
    let lock_b = Mutex::new(0usize);

    {
        let _guard_a = lock_a.lock();
        let _guard_b = lock_b.lock();
    }

    let _guard_b = lock_b.lock();
    assert!(
        lock_a.try_lock().is_some(),
        "try_lock(A) unexpectedly failed without lockdep"
    );
}

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

fn mixed_ms_single_task_abba() {
    let lock_a = Mutex::new(0usize);
    let lock_b = SpinRaw::new(0usize);

    {
        let _guard_a = lock_a.lock();
        let _guard_b = lock_b.lock();
    }

    let _guard_b = lock_b.lock();
    assert!(
        lock_a.try_lock().is_some(),
        "try_lock(A) unexpectedly failed without lockdep"
    );
}

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

struct TestFs;

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

static TEST_FS: TestFs = TestFs;

struct TestDir {
    inode: u64,
    this: WeakDirEntry,
    renamed: AtomicBool,
}

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

fn vfs_cache_single_task_abba() {
    let dir = TestDir::new_entry(1, "dir");

    {
        let _guard = dir.user_data();
        let _child = dir.as_dir().unwrap().lookup("child").unwrap();
    }

    dir.as_dir()
        .unwrap()
        .insert_cache("old".to_string(), TestDir::new_entry(2, "old"));
    dir.as_dir()
        .unwrap()
        .rename("old", dir.as_dir().unwrap(), "new")
        .unwrap();
}
