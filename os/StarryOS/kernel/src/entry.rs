use alloc::{
    string::{String, ToString},
    sync::Arc,
};

use ax_fs_ng::vfs::current_fs_context;
use ax_runtime::hal::cpu::user::UserContext;

use crate::{
    file::{FD_TABLE, FileTable, new_file_table_scope},
    mm::{MmHandle, load_user_app, new_user_image_builder},
    namespace::NsProxy,
    pseudofs::{self, dev::tty},
    sync::{Mutex, RwLock},
    task::{
        PidReservation, PidReservationKind, Process, ProcessData, ProcessDataInit, ProcessImage,
        ROOT_PID_NS, Tgid, Thread, Tid, TidNumber, UserThreadOptions, new_user_task,
        prepare_user_thread, spawn_alarm_task,
    },
    tracepoint::tracepoint_init,
};

/// Initialize and run initproc.
pub fn init(args: &[String], envs: &[String]) {
    // Install task-context diagnostics and contention backoff before userspace.
    crate::rdrive_osal::init();

    crate::stop_machine::init();
    crate::trap::init_handlers();
    static_keys::global_init();
    crate::cgroup::init();

    tracepoint_init().expect("Failed to initialize tracepoints");

    crate::ebpf::init_ebpf();
    crate::perf::perf_event_init();
    crate::kmod::init_kmod();

    pseudofs::mount_all().expect("Failed to mount pseudofs");
    crate::file::epoll::start_epoll_notify_worker();
    spawn_alarm_task();
    crate::mm::spawn_reclaimer_task();
    pseudofs::usbfs::start_event_pump();

    ax_alloc::register_page_reclaim_fn(ax_fs_ng::vfs::page_cache_reclaim);

    let loc = current_fs_context()
        .lock()
        .resolve(&args[0])
        .expect("Failed to resolve executable path");
    let path = loc
        .absolute_path()
        .expect("Failed to get executable absolute path");
    let name = loc.name().into_owned();

    let mut image_builder =
        new_user_image_builder().expect("Failed to create unpublished user address space");
    let loaded_image = load_user_app(
        &mut image_builder,
        loc,
        &args[0],
        args,
        envs,
        &crate::task::Cred::root(),
    )
    .unwrap_or_else(|error| panic!("Failed to load user app: {error}"));
    let prepared_image = image_builder
        .finish(loaded_image)
        .expect("loaded init image token no longer matches its address space");
    let (uspace, entry_vaddr, ustack_top, auxv) = prepared_image.into_parts();

    let uctx = UserContext::new(entry_vaddr.into(), ustack_top, 0);

    // PID 1 must really be 1: the init process is the root of the process
    // hierarchy and userspace (e.g. systemd's `getpid() == 1` system-manager
    // check) relies on it. The scheduler task id is an internal counter that is
    // already past 1 by the time we spawn the user init (kernel helper tasks
    // took the low ids), so we pin the user-visible pid/tid to 1 and leave the
    // scheduler id untouched. `Thread::tid` is already decoupled from the
    // scheduler id (see its field doc), so this only requires the table keys to
    // follow the thread tid rather than `task.id()`.
    const INIT_PID: u32 = 1;
    let reservation = PidReservation::reserve(&ROOT_PID_NS, PidReservationKind::ProcessLeader)
        .expect("failed to reserve init PID identity");
    let pid = reservation
        .number_in(&ROOT_PID_NS)
        .expect("init PID reservation has no root binding")
        .get();
    assert_eq!(pid, INIT_PID);
    let identity = reservation.identity();
    let tid_lease = identity
        .acquire_role::<Tid>()
        .expect("failed to acquire init TID role");
    let tgid_lease = identity
        .acquire_role::<Tgid>()
        .expect("failed to acquire init TGID role");
    let proc = Process::new_init(identity.clone()).expect("failed to prepare init process");
    proc.add_thread(TidNumber::try_from(pid).expect("init TID must be non-zero"));

    // Initial console descriptors do not assign a controlling terminal to
    // PID 1. A userspace session leader claims it with TIOCSCTTY.

    let proc = ProcessData::new(
        proc,
        identity.clone(),
        tgid_lease,
        ProcessDataInit::new(
            ProcessImage::new(
                path.to_string(),
                Arc::new(args.to_vec()),
                Arc::new(envs.to_vec()),
                auxv,
                "/".to_string(),
                "/".to_string(),
            ),
            MmHandle::from_arc(Arc::new(Mutex::new(uspace)))
                .expect("init MM identity must be unique"),
            Arc::default(),
            NsProxy::new_root(),
            None,
            TidNumber::try_from(pid).expect("init TID must be non-zero"),
        ),
    );
    // SAFE-EXPECT: failing to attach init would violate the kernel's process accounting invariant.
    crate::cgroup::attach_initial_process(&identity)
        .expect("Failed to attach init process to cgroup root");

    let mut scope = scope_local::Scope::new();
    let mut fd_table = FileTable::new();
    crate::file::add_stdio(&mut fd_table).expect("Failed to add stdio");
    *FD_TABLE.scope_mut(&mut scope) = new_file_table_scope(Arc::new(RwLock::new(fd_table)));

    let thr = Thread::new(
        identity.clone(),
        tid_lease,
        proc,
        None,
        starry_signal::SignalSet::default(),
        scope,
    )
    .expect("failed to prepare init thread state");
    let prepared_task = prepare_user_thread(
        new_user_task(
            uctx,
            0,
            TidNumber::try_from(pid).expect("init TID must be non-zero"),
        ),
        thr,
        UserThreadOptions::new(&name).expect("failed to prepare init thread name"),
    )
    .expect("failed to prepare init task");
    let staged_task = prepared_task.stage().expect("failed to stage init task");
    let published_identity = reservation
        .publish()
        .expect("failed to publish init PID identity");
    debug_assert!(Arc::ptr_eq(&published_identity, &identity));
    staged_task.with_task(|task| task.as_thread().attach_pid_task(task));
    tty::arm_console_irq();
    let task = staged_task.activate();

    // Reap the initial scheduler thread, which may exit while init's peers
    // remain alive. Only do_exit's last-thread owner decides that init died.
    // Userspace owns normal shutdown through reboot(2); the bootstrap thread
    // must neither return nor poll a second process-lifecycle state here.
    let _exit_code = task.join();
    match crate::task::future::block_on(core::future::pending::<core::convert::Infallible>()) {}
}
