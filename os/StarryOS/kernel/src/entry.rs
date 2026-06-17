use alloc::{
    string::{String, ToString},
    sync::Arc,
};

use ax_fs::FS_CONTEXT;
use ax_runtime::hal::cpu::uspace::UserContext;
use ax_sync::Mutex;
use ax_task::{AxTaskExt, spawn_task};
use starry_process::{Pid, Process};

use crate::{
    file::FD_TABLE,
    mm::{copy_from_kernel, load_user_app, new_user_aspace_empty},
    pseudofs::{self, dev::tty::N_TTY},
    task::{ProcessData, ProcessImage, Thread, add_task_to_table, new_user_task, spawn_alarm_task},
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

    ax_alloc::register_page_reclaim_fn(ax_fs::page_cache_reclaim);

    let loc = FS_CONTEXT
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
    let mut task = new_user_task(&name, uctx, 0);
    task.ctx_mut().set_page_table_root(uspace.page_table_root());

    let pid = task.id().as_u64() as Pid;
    let proc = Process::new_init(pid);
    proc.add_thread(pid);

    N_TTY.bind_to(&proc).expect("Failed to bind ntty");

    let proc = ProcessData::new(
        proc,
        ProcessImage::new(path.to_string(), Arc::new(args.to_vec()), auxv),
        Arc::new(Mutex::new(uspace)),
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
    *task.task_ext_mut() = Some(AxTaskExt::from_impl(thr));

    let task = spawn_task(task);
    add_task_to_table(&task);

    // TODO: wait for all processes to finish
    let exit_code = task.join();
    info!("Init process exited with code: {exit_code:?}");

    let cx = FS_CONTEXT.lock();
    cx.root_dir()
        .unmount_all()
        .expect("Failed to unmount all filesystems");
    cx.root_dir()
        .filesystem()
        .flush()
        .expect("Failed to flush rootfs");
}
