use alloc::{
    string::{String, ToString},
    sync::Arc,
};

use axfs::FS_CONTEXT;
use axhal::uspace::UserContext;
use axsync::Mutex;
use axtask::{AxTaskExt, spawn_task};
use starry_api::{file::FD_TABLE, task::new_user_task, vfs::dev::tty::N_TTY};
use starry_core::{
    mm::{copy_from_kernel, load_user_app, new_user_aspace_empty},
    task::{ProcessData, Thread, add_task_to_table},
};
use starry_process::{Pid, Process};

pub fn run_initproc(args: &[String], envs: &[String]) -> i32 {
    let mut uspace = new_user_aspace_empty()
        .and_then(|mut it| {
            copy_from_kernel(&mut it)?;
            Ok(it)
        })
        .expect("Failed to create user address space");

    let loc = FS_CONTEXT
        .lock()
        .resolve(&args[0])
        .expect("Failed to resolve executable path");
    let path = loc
        .absolute_path()
        .expect("Failed to get executable absolute path");
    let name = loc.name();

    let (entry_vaddr, ustack_top) = load_user_app(&mut uspace, None, args, envs)
        .unwrap_or_else(|e| panic!("Failed to load user app: {}", e));

    let uctx = UserContext::new(entry_vaddr.into(), ustack_top, 0);

    let mut task = new_user_task(name, uctx, None);
    task.ctx_mut().set_page_table_root(uspace.page_table_root());

    let pid = task.id().as_u64() as Pid;
    let proc = Process::new_init(pid);
    proc.add_thread(pid);

    N_TTY.bind_to(&proc).expect("Failed to bind ntty");

    let proc_data = ProcessData::new(
        proc,
        path.to_string(),
        Arc::new(args.to_vec()),
        Arc::new(Mutex::new(uspace)),
        Arc::default(),
        None,
    );
    {
        let mut scope = proc_data.scope.write();
        starry_api::file::add_stdio(&mut FD_TABLE.scope_mut(&mut scope).write())
            .expect("Failed to add stdio");
    }
    let thr = Thread::new(pid, proc_data);

    *task.task_ext_mut() = Some(unsafe { AxTaskExt::from_impl(thr) });

    let task = spawn_task(task);
    add_task_to_table(&task);

    // TODO: wait for all processes to finish
    task.join()
}
