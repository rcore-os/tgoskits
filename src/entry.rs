use alloc::{borrow::ToOwned, string::String, sync::Arc};
use axfs_ng::FS_CONTEXT;
use axhal::arch::UspaceContext;
use axprocess::{Pid, init_proc};
use axsignal::Signo;
use axsync::Mutex;
use axtask::TaskExtProxy;
use starry_core::{
    mm::{copy_from_kernel, load_user_app, map_trampoline, new_user_aspace_empty},
    task::{ProcessData, StarryTaskExt, ThreadData, add_thread_to_table, new_user_task},
};

pub fn run_user_app(args: &[String], envs: &[String]) -> Option<i32> {
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

    let process_data = ProcessData::new(
        exe_path.clone(),
        Arc::new(Mutex::new(uspace)),
        Arc::default(),
        Some(Signo::SIGCHLD),
    );

    let tid = task.id().as_u64() as Pid;
    let process = init_proc().fork(tid).data(process_data).build();

    let thread = process
        .new_thread(tid)
        .data(ThreadData::new(process.data().unwrap()))
        .build();
    add_thread_to_table(&thread);

    *task.task_ext_mut() = Some(unsafe { TaskExtProxy::from_impl(StarryTaskExt::new(thread)) });

    let task = axtask::spawn_task(task);

    // TODO: we need a way to wait on the process but not only the main task
    task.join()
}
