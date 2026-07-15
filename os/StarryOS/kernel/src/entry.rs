use alloc::{
    string::{String, ToString},
    sync::Arc,
};

use ax_fs_ng::vfs::current_fs_context;
use ax_kspin::PreemptIrqGuard;
use ax_runtime::hal::cpu::uspace::UserContext;
use ax_sync::PiMutex;
use starry_process::{Pid, Process};

use crate::{
    file::FD_TABLE,
    mm::{copy_from_kernel, load_user_app, new_user_aspace_empty},
    pseudofs::{self, dev::tty},
    task::{
        ProcessData, ProcessImage, Thread, add_task_to_table, new_user_task, spawn_alarm_task,
        spawn_user_thread,
    },
    tracepoint::tracepoint_init,
};

/// Initialize and run initproc.
pub fn init(args: &[String], envs: &[String]) {
    static_keys::global_init();
    tracepoint_init().expect("Failed to initialize tracepoints");

    crate::ebpf::init_ebpf();
    crate::perf::perf_event_init();
    crate::kmod::init_kmod();

    pseudofs::mount_all().expect("Failed to mount pseudofs");
    spawn_alarm_task();
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

    let mut uspace = new_user_aspace_empty()
        .and_then(|mut it| {
            copy_from_kernel(&mut it)?;
            Ok(it)
        })
        .expect("Failed to create user address space");

    let (entry_vaddr, ustack_top, auxv) = load_user_app(&mut uspace, loc, &args[0], args, envs)
        .unwrap_or_else(|e| panic!("Failed to load user app: {}", e));

    let uctx = UserContext::new(entry_vaddr.into(), ustack_top, 0);
    let page_table_root = uspace.page_table_root().as_usize();

    // PID 1 must really be 1: the init process is the root of the process
    // hierarchy and userspace (e.g. systemd's `getpid() == 1` system-manager
    // check) relies on it. The scheduler task id is an internal counter that is
    // already past 1 by the time we spawn the user init (kernel helper tasks
    // took the low ids), so we pin the user-visible pid/tid to 1 and leave the
    // scheduler id untouched. `Thread::tid` is already decoupled from the
    // scheduler id (see its field doc), so this only requires the table keys to
    // follow the thread tid rather than `task.id()`.
    const INIT_PID: Pid = 1;
    let pid = INIT_PID;
    let proc = Process::new_init(pid);
    proc.add_thread(pid);

    if let Err(err) = tty::bind_console_to(&proc) {
        warn!("Failed to bind console tty: {err:?}");
    }

    let proc = ProcessData::new(
        proc,
        ProcessImage::new(path.to_string(), Arc::new(args.to_vec()), auxv),
        Arc::new(PiMutex::new(uspace)),
        Arc::default(),
        None,
        pid,
        false,
    );

    {
        let mut scope = proc.scope.write();
        crate::file::add_stdio(&mut FD_TABLE.scope_mut(&mut scope).write())
            .expect("Failed to add stdio");
    }

    let thr = Thread::new(pid, proc, None, starry_signal::SignalSet::default());

    let task = {
        let _guard = PreemptIrqGuard::new();
        let task = spawn_user_thread(
            new_user_task(uctx, 0),
            name,
            crate::config::KERNEL_STACK_SIZE,
            page_table_root,
            thr,
        )
        .unwrap_or_else(|error| panic!("failed to spawn init task: {error}"));
        add_task_to_table(&task);
        tty::arm_console_irq();
        task
    };

    // TODO: wait for all processes to finish
    let exit_code = task.join();
    info!("Init process exited with code: {exit_code:?}");

    let fs_context = current_fs_context();
    let cx = fs_context.lock();
    cx.root_dir()
        .unmount_all()
        .expect("Failed to unmount all filesystems");
    cx.root_dir()
        .filesystem()
        .flush()
        .expect("Failed to flush rootfs");
}
