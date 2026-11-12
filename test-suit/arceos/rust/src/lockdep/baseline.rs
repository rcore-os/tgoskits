use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    vec::Vec,
};

use ax_std::os::arceos::sync::{Mutex, RawSpinLock};
use axfs_ng_vfs::{Mountpoint, MutationCredentials, NodePermission, NodeType, VfsError};

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
    static GLOBAL: RawSpinLock<usize> = RawSpinLock::new(0);
    *GLOBAL.lock_irqsave() = 0;

    let shared = Arc::new(RawSpinLock::new(0usize));
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let shared = shared.clone();
        tasks.push(thread::spawn(move || {
            for _ in 0..32 {
                *shared.lock_irqsave() += 1;
                *GLOBAL.lock_irqsave() += 1;
                thread::yield_now();
            }
        }));
    }

    for task in tasks {
        task.join().unwrap();
    }

    assert_eq!(*shared.lock_irqsave(), 4 * 32);
    assert_eq!(*GLOBAL.lock_irqsave(), 4 * 32);
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
    {
        let _guard_b = lock_b.lock();
        assert!(
            lock_a.try_lock().is_some(),
            "try_lock(A) unexpectedly failed without lockdep"
        );
    }
    handle.join().unwrap();
}

fn spin_single_task_abba() {
    let lock_a = RawSpinLock::new(0usize);
    let lock_b = RawSpinLock::new(0usize);

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
    let lock_a = Arc::new(RawSpinLock::new(0usize));
    let lock_b = Arc::new(RawSpinLock::new(0usize));
    let stage = Arc::new(AtomicUsize::new(0));

    let thread_lock_a = lock_a.clone();
    let thread_lock_b = lock_b.clone();
    let thread_stage = stage.clone();

    let handle = thread::spawn(move || {
        {
            let _guard_a = thread_lock_a.lock();
            let _guard_b = thread_lock_b.lock();
        }
        // The peer cannot touch either lock until stage 1 is published. Check
        // this precondition here so early publication fails deterministically.
        assert!(
            thread_lock_a.try_lock().is_some(),
            "stage 1 requires spin lock A to be released"
        );
        assert!(
            thread_lock_b.try_lock().is_some(),
            "stage 1 requires spin lock B to be released"
        );
        thread_stage.store(1, Ordering::Release);
    });

    wait_until(&stage, 1);
    {
        let _guard_b = lock_b.lock();
        assert!(
            lock_a.try_lock().is_some(),
            "try_lock(A) unexpectedly failed without lockdep"
        );
    }
    handle.join().unwrap();
}

fn mixed_single_task_abba() {
    let lock_a = RawSpinLock::new(0usize);
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
    let lock_a = Arc::new(RawSpinLock::new(0usize));
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
    {
        let _guard_b = lock_b.lock();
        assert!(
            lock_a.try_lock().is_some(),
            "try_lock(A) unexpectedly failed without lockdep"
        );
    }
    handle.join().unwrap();
}

fn mixed_ms_single_task_abba() {
    let lock_a = Mutex::new(0usize);
    let lock_b = RawSpinLock::new(0usize);

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
    let lock_b = Arc::new(RawSpinLock::new(0usize));
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
    {
        let _guard_b = lock_b.lock();
        assert!(
            lock_a.try_lock().is_some(),
            "try_lock(A) unexpectedly failed without lockdep"
        );
    }
    handle.join().unwrap();
}

fn vfs_cache_single_task_abba() {
    let filesystem = ax_fs_ng::MemoryFs::new();
    let mount = Mountpoint::new_root(&filesystem);
    let dir = mount.root_location();
    dir.create(
        "child",
        NodeType::Directory,
        NodePermission::default(),
        0,
        0,
    )
    .unwrap();
    let old = dir
        .create("old", NodeType::Directory, NodePermission::default(), 0, 0)
        .unwrap();

    {
        let _guard = dir.entry().user_data();
        let _child = dir.lookup_no_follow("child").unwrap();
    }

    dir.rename("old", &dir, "new", &MutationCredentials::root())
        .unwrap();
    let renamed = dir.lookup_no_follow("new").unwrap();
    assert!(renamed.entry().ptr_eq(old.entry()));
    assert!(matches!(
        dir.lookup_no_follow("old"),
        Err(VfsError::NotFound)
    ));
}
