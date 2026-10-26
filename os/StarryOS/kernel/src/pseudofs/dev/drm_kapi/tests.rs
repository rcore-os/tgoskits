//! Shared DRM waits exercised with real Starry threads, signals and epoll.

use core::{sync::atomic::AtomicUsize, task::Poll};

use ax_std::os::arceos::task::thread::ThreadState;
use kdrm_core::{
    drm_drv_types::DrmDriver,
    drm_drv_upstream::{drm_dev_alloc, drm_dev_get, drm_dev_put, drm_dev_register, drm_dev_unplug, drm_dev_unregister},
    drm_file_upstream::{drm_open_minor_index, drm_poll, drm_release},
    drm_syncobj_upstream::{drm_syncobj_fence_create, drm_syncobj_fence_put, drm_syncobj_fence_signal},
};
use starry_signal::{SignalDisposition, SignalInfo, Signo};

use super::*;
use crate::{
    file::{epoll::{Epoll, EpollEvent, EpollFlags}, close_file_like},
    task::{PidReservation, PidReservationKind, ROOT_PID_NS, Tgid, Thread, Tid, UserThreadOptions, prepare_user_thread},
};

fn init_services() {
    static INIT: ax_lazyinit::OnceLock<()> = ax_lazyinit::OnceLock::new();
    INIT.call_once(|| {
        ax_drm::runtime::init().unwrap();
        super::init();
    });
}

fn spawn_user(entry: impl FnOnce() + Send + 'static) -> (UserTaskRef, PidReservation) {
    let reservation = PidReservation::reserve(&ROOT_PID_NS, PidReservationKind::ProcessLeader).unwrap();
    let identity = reservation.identity();
    let tid = identity.acquire_role::<Tid>().unwrap();
    let process = crate::task::new_test_process_data(identity.clone(), identity.acquire_role::<Tgid>().unwrap());
    crate::mm::copy_from_kernel(&mut process.pin_aspace().unwrap().lock()).unwrap();
    let thread = Thread::new(identity, tid, process, None, Default::default(), scope_local::Scope::new()).unwrap();
    let task = prepare_user_thread(entry, thread, UserThreadOptions::new("drm-wait-test").unwrap())
        .unwrap().stage().unwrap().activate();
    (task, reservation)
}

fn await_blocked(task: &UserTaskRef, boundary: &str, ready: impl Fn() -> bool) {
    let deadline = ax_runtime::hal::time::monotonic_time_nanos() + 2_000_000_000;
    loop {
        if ready() && task.state() == ThreadState::Blocked { return; }
        assert!(ax_runtime::hal::time::monotonic_time_nanos() < deadline, "DRM waiter did not park at {boundary}: state={:?} ready={}", task.state(), ready());
        crate::task::yield_now();
    }
}

#[axtest::axtest]
fn pending_input_fence_is_interrupted_by_a_real_starry_signal() {
    init_services();
    let fence = drm_syncobj_fence_create();
    assert!(!fence.is_null());
    let fence_address = fence as usize;
    let phase = Arc::new(AtomicUsize::new(0));
    let progress = phase.clone();
    let (task, reservation) = spawn_user(move || {
        // A fixture may receive PID 1, whose default signal dispositions are
        // protected. A caught signal remains deliverable in either case; this
        // kernel entry never returns to userspace to execute the handler.
        unsafe extern "C" fn handler(_: i32) {}
        let current = crate::task::current_user_task();
        current.as_thread().signal().process().actions().lock_irqsave()[Signo::SIGUSR1].disposition = SignalDisposition::Handler(handler);
        let fence = fence_address as *mut drm_kapi::types::DmaFence;
        progress.store(1, Ordering::Release);
        assert!(drm_kapi::task_runtime::can_block_current(), "input fence requires a sleepable user task");
        // SAFETY: The parent retains this software fence through join. Both
        // waits run in a real sleepable Starry user-thread context.
        unsafe {
            assert_eq!(drm_kapi::dma_fence::dma_fence_wait(fence, true), -drm_kapi::config::ERESTARTSYS);
            assert_eq!(drm_kapi::dma_fence::dma_fence_wait_timeout(fence, true, 100), -(drm_kapi::config::ERESTARTSYS as c_long));
            assert!(!drm_kapi::dma_fence::dma_fence_is_signaled(fence));
            progress.store(2, Ordering::Release);
            let infinite = drm_kapi::config::MAX_SCHEDULE_TIMEOUT as c_long;
            assert_eq!(drm_kapi::dma_fence::dma_fence_wait_timeout(fence, false, infinite), infinite);
        }
        progress.store(3, Ordering::Release);
    });
    await_blocked(&task, "interruptible input fence", || phase.load(Ordering::Acquire) == 1);
    crate::task::send_signal_to_task(&task, None, Some(SignalInfo::new_kernel(Signo::SIGUSR1))).unwrap();
    // The same pending signal cannot complete the uninterruptible wait.
    await_blocked(&task, "uninterruptible input fence", || phase.load(Ordering::Acquire) == 2);
    // Consume whole jiffies while the native waiter is parked. An infinite
    // budget must remain the sentinel, not become a very large finite wait
    // when dma_resv passes it on to the next fence.
    let earliest_signal = ax_runtime::hal::time::monotonic_time_nanos()
        + 2_000_000_000 / drm_kapi::config::CONFIG_HZ as u64;
    while ax_runtime::hal::time::monotonic_time_nanos() < earliest_signal {
        crate::task::yield_now();
    }
    // SAFETY: The parent still owns the fence, and signaling wakes native waits.
    unsafe { drm_syncobj_fence_signal(fence, 0) };
    assert_eq!(task.join(), 0);
    assert_eq!(phase.load(Ordering::Acquire), 3);
    // SAFETY: The worker has joined; release the parent's sole fence reference.
    unsafe { drm_syncobj_fence_put(fence) };
    drop(reservation);
}

// The callbacks below only adapt the Linux file_operations prototypes. The
// actual readiness, queue registration, unplug and release remain in DRM core.
unsafe extern "C" fn poll_drm(file: *mut LinuxKpiFile, table: *mut PollTable) -> u32 {
    // SAFETY: The anon inode owns a full-sized core File allocation as its
    // opaque payload; it is distinct from the smaller shmem File wrapper.
    unsafe {
        let core = (*file)._private_data.cast::<drm_kapi::types::File>();
        let private = (*core).private_data.cast::<kdrm_core::drm_file_types::DrmFile>();
        register_poll_wait(file.cast(), ptr::addr_of_mut!((*private).event_wait), table);
        drm_poll(core, ptr::null_mut())
    }
}

unsafe extern "C" fn release_drm(inode: *mut shmem::Inode, file: *mut LinuxKpiFile) -> c_int {
    // SAFETY: Final fput owns the separately allocated DRM core file payload.
    unsafe {
        let core = (*file)._private_data.cast::<drm_kapi::types::File>();
        let result = drm_release(inode.cast(), core);
        drop(Box::from_raw(core));
        result
    }
}

#[axtest::axtest]
fn unplug_wakes_an_already_sleeping_epoll_error_only_observer() {
    init_services();
    for callback in [wake_poll_waiters, super::super::card0::wake_poll_waiters] {
        let device = Arc::new(AtomicUsize::new(0));
        let published = device.clone();
        let result = Arc::new(AtomicU32::new(0));
        let observed = result.clone();
        let (task, reservation) = spawn_user(move || {
            // SAFETY: Zeroed driver storage has only nullable pointers and
            // scalar fields. It stays pinned until after device destruction.
            let mut driver = Box::new(unsafe { mem::zeroed::<DrmDriver>() });
            driver.name = c"drm-unplug-test".as_ptr().cast_mut().cast();
            let fops = FileOperations { poll: Some(poll_drm), release: Some(release_drm), ..FileOperations::empty() };
            // SAFETY: The driver and fops remain live through all fd owners.
            unsafe {
                let dev = drm_dev_alloc(&*driver, ptr::null_mut());
                assert!(!dev.is_null());
                assert_eq!(drm_dev_register(dev, 0), 0);
                let core = Box::into_raw(Box::new(mem::zeroed::<drm_kapi::types::File>()));
                assert_eq!(drm_open_minor_index((*(*dev).primary).index as u32, core), 0);
                let raw = drm_kapi::kernel_services::anon_inode_getfile(c"drm-test".as_ptr(), (&fops as *const FileOperations).cast(), core.cast(), 0);
                assert!(!raw.is_null() && !drm_kapi::config::is_err(raw));
                let fd = drm_kapi::kernel_services::get_unused_fd_flags(0);
                assert!(fd >= 0);
                drm_kapi::kernel_services::fd_install(fd, raw);
                let epoll = Epoll::new();
                epoll.add(fd, EpollEvent { events: IoEvents::empty(), user_data: 7 }, EpollFlags::empty()).unwrap();
                let wrapper = KAPI_FILES.lock().get(&(raw as usize)).and_then(Weak::upgrade).unwrap();
                // This isolated device cannot notify before the parent unplugs
                // it. Exercise each production adapter's actual wake callback.
                for entry in wrapper.wait_entries.lock().iter() { (*entry.entry.get()).func = Some(callback); }
                published.store(dev as usize, Ordering::Release);
                let events = crate::task::future::block_on(crate::task::future::poll_shared(
                    || {
                        let mut mask = 0;
                        match epoll.poll_events_with(1, |_, event| { mask = event.events; Ok(()) }) {
                            Ok(_) => Poll::Ready(mask),
                            Err(StarryError::WouldBlock) => Poll::Pending,
                            Err(error) => panic!("DRM epoll query failed: {error:?}"),
                        }
                    },
                    |sink| epoll.register_shared(sink, IoEvents::IN),
                ));
                observed.store(events, Ordering::Release);
                epoll.delete(fd).unwrap();
                close_file_like(fd).unwrap();
                drop(wrapper);
                drm_dev_unregister(dev);
                drm_dev_put(dev);
            }
        });
        await_blocked(&task, "epoll terminal observer", || device.load(Ordering::Acquire) != 0);
        // SAFETY: The blocked worker owns the registered device. Take another
        // reference so it cannot disappear while this unplug call returns.
        unsafe {
            let dev = device.load(Ordering::Acquire) as *mut kdrm_core::drm_device_types::DrmDevice;
            drm_dev_get(dev);
            drm_dev_unplug(dev);
            drm_dev_put(dev);
        }
        assert_eq!(task.join(), 0);
        assert_eq!(result.load(Ordering::Acquire), EPOLLERR | EPOLLHUP);
        drop(reservation);
    }
}
