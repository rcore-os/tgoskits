use alloc::{string::ToString, sync::Arc, vec::Vec};
use core::ffi::c_char;

use ax_errno::{AxError, AxResult};
use ax_fs::FS_CONTEXT;
use ax_hal::uspace::UserContext;
use ax_sync::Mutex;
use ax_task::current;
use starry_vm::vm_load_until_nul;

use crate::{
    config::USER_HEAP_BASE,
    file::FD_TABLE,
    mm::{copy_from_kernel, load_user_app, new_user_aspace_empty, vm_load_string},
    task::AsThread,
};

pub fn sys_execve(
    uctx: &mut UserContext,
    path: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> AxResult<isize> {
    // ----------------------------------------------------------------
    // Phase 1: all fallible work — nothing is committed yet.
    // If any of these fail we return an error and the process is intact.
    // ----------------------------------------------------------------
    let path = vm_load_string(path)?;

    let args = if argv.is_null() {
        // Handle NULL argv (treat as empty array)
        Vec::new()
    } else {
        vm_load_until_nul(argv)?
            .into_iter()
            .map(vm_load_string)
            .collect::<Result<Vec<_>, _>>()?
    };

    let envs = if envp.is_null() {
        // Handle NULL envp (treat as empty array)
        Vec::new()
    } else {
        vm_load_until_nul(envp)?
            .into_iter()
            .map(vm_load_string)
            .collect::<Result<Vec<_>, _>>()?
    };

    debug!("sys_execve <= path: {path:?}, args: {args:?}, envs: {envs:?}");

    let curr = current();
    let proc_data = &curr.as_thread().proc_data;

    if proc_data.proc.threads().len() > 1 {
        // TODO: handle multi-thread case
        error!("sys_execve: multi-thread not supported");
        return Err(AxError::WouldBlock);
    }

    // Resolve the path and collect metadata before touching anything.
    let loc = FS_CONTEXT.lock().resolve(&path)?;
    let new_name = loc.name();
    let new_exe_path = loc.absolute_path()?.to_string();

    // Build the new address space entirely before committing.
    // Loading into a fresh aspace (rather than clearing the existing one)
    // ensures a CLONE_VM parent's mappings are never disturbed —
    // posix_spawn uses CLONE_VM|CLONE_VFORK and runs the child on a stack
    // slice inside the parent's address space.
    let mut new_aspace = new_user_aspace_empty()?;
    copy_from_kernel(&mut new_aspace)?;
    let (entry_point, user_stack_base) =
        load_user_app(&mut new_aspace, Some(path.as_str()), &args, &envs)?;

    // Collect CLOEXEC fds to close (read-only scan, no mutation yet).
    let cloexec_fds: Vec<_> = {
        let fd_table = FD_TABLE.read();
        fd_table
            .ids()
            .filter(|it| fd_table.get(*it).unwrap().cloexec)
            .collect()
    };

    // ----------------------------------------------------------------
    // Phase 2: point of no return — commit all changes.
    // Nothing below may fail; errors here would leave the process broken.
    // ----------------------------------------------------------------

    // Replace the aspace Arc so the parent's shared Arc<Mutex<AddrSpace>>
    // (from CLONE_VM) is never touched. The parent's page table register
    // keeps pointing at the original still-live AddrSpace.
    let new_pt_root = new_aspace.page_table_root();
    let newaspace_arc = Arc::new(Mutex::new(new_aspace));
    proc_data.replace_aspace(newaspace_arc);

    // Switch the hardware page table now that the new aspace is installed.
    curr.switch_page_table(new_pt_root);

    curr.set_name(new_name);
    *proc_data.exe_path.write() = new_exe_path;
    *proc_data.cmdline.write() = Arc::new(args);

    proc_data.set_heap_top(USER_HEAP_BASE);

    proc_data.signal.reset_actions();

    // Clear set_child_tid after exec since the original address is no longer valid
    curr.as_thread().set_clear_child_tid(0);

    // Close CLOEXEC file descriptors.
    let mut fd_table = FD_TABLE.write();
    for fd in cloexec_fds {
        fd_table.remove(fd);
    }
    drop(fd_table);

    uctx.set_ip(entry_point.as_usize());
    uctx.set_sp(user_stack_base.as_usize());

    // Unblock a vfork parent waiting for this child to exec.
    // Must be last: by now CLOEXEC fds are closed so the parent's pipe
    // read will see EOF correctly.
    proc_data.notify_vfork_done();

    Ok(0)
}
