use alloc::{string::String, sync::Arc};
use arceos_posix_api::FilePath;
use axhal::arch::UspaceContext;
use axsync::Mutex;

use crate::{
    mm::{copy_from_kernel, load_user_app, new_user_aspace_empty},
    task::spawn_user_task,
};

pub fn run_user_app(args: &[String], envs: &[String]) -> Option<i32> {
    let mut uspace = new_user_aspace_empty()
        .and_then(|mut it| {
            copy_from_kernel(&mut it)?;
            Ok(it)
        })
        .expect("Failed to create user address space");

    let path = FilePath::new(&args[0]).expect("Invalid file path");
    axfs::api::set_current_dir(path.parent().unwrap()).expect("Failed to set current dir");

    let (entry_vaddr, ustack_top) = load_user_app(&mut uspace, args, envs)
        .unwrap_or_else(|e| panic!("Failed to load user app: {}", e));
    let user_task = spawn_user_task(
        Arc::new(Mutex::new(uspace)),
        UspaceContext::new(entry_vaddr.into(), ustack_top, 2333),
        axconfig::plat::USER_HEAP_BASE as _,
    );
    user_task.join()
}
