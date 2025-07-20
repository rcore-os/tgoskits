use alloc::{borrow::ToOwned, string::String, sync::Arc};

use axfs_ng::FS_CONTEXT;
use axhal::context::UspaceContext;
use axprocess::{Pid, Process};
use axsync::Mutex;
use axtask::{TaskExtProxy, spawn_task};
use starry_api::task::new_user_task;
use starry_core::{
    mm::{copy_from_kernel, load_user_app, map_trampoline, new_user_aspace_empty},
    task::{ProcessData, Thread, add_task_to_table},
};

pub fn run_initproc(args: &[String], envs: &[String]) -> i32 {
    let mut uspace = new_user_aspace_empty()
        .and_then(|mut it| {
            copy_from_kernel(&mut it)?;
            map_trampoline(&mut it)?;
            Ok(it)
        })
        .expect("Failed to create user address space");

    let exe_path = &args[0];
    let name = FS_CONTEXT
        .lock()
        .resolve(exe_path)
        .expect("Failed to resolve executable path")
        .name()
        .to_owned();

    let (entry_vaddr, ustack_top) = load_user_app(&mut uspace, None, args, envs)
        .unwrap_or_else(|e| panic!("Failed to load user app: {}", e));

    let uctx = UspaceContext::new(entry_vaddr.into(), ustack_top, 2333);

    let mut task = new_user_task(&name, uctx, None);
    task.ctx_mut().set_page_table_root(uspace.page_table_root());

    let pid = task.id().as_u64() as Pid;
    let proc = Process::new_init(pid);
    proc.add_thread(pid);

    let proc_data = ProcessData::new(
        proc,
        exe_path.clone(),
        Arc::new(Mutex::new(uspace)),
        Arc::default(),
        None,
    );
    let thr = Thread::new(proc_data);

    *task.task_ext_mut() = Some(unsafe { TaskExtProxy::from_impl(thr) });

    let task = spawn_task(task);
    add_task_to_table(&task);

    // TODO: wait for all processes to finish
    task.join()
}
