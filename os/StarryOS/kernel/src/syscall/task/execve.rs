use alloc::{string::ToString, sync::Arc, vec::Vec};
use core::{ffi::c_char, future::poll_fn, task::Poll};

use ax_errno::{AxError, AxResult};
use ax_fs::FS_CONTEXT;
use ax_hal::uspace::UserContext;
use ax_task::{
    current,
    future::{block_on, interruptible},
};
use starry_process::Pid;
use starry_signal::{SignalAction, SignalDisposition, SignalInfo, Signo};
use starry_vm::vm_load_until_nul;

use crate::{
    config::USER_HEAP_BASE,
    file::FD_TABLE,
    mm::{copy_from_kernel, load_user_app, new_user_aspace_empty, vm_load_string},
    task::{AsThread, send_signal_to_thread},
};

pub fn sys_execve(
    uctx: &mut UserContext,
    path: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> AxResult<isize> {
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
    let my_tid = curr.id().as_u64() as Pid;

    // Serialize execve across the process. A concurrent execve from a sibling
    // would race on aspace teardown and signal-action reset.
    let _exec_guard = proc_data.exec_lock.try_lock().ok_or(AxError::Interrupted)?;

    // POSIX requires execve failures to leave the process unchanged, but the
    // sibling-thread teardown below is irreversible. So run every fallible
    // step first against a throwaway address space: a bad path, malformed
    // ELF, missing interpreter, or up-front OOM surfaces here while the
    // siblings are still alive and our own aspace is untouched, and the
    // caller sees a clean execve failure.
    let loc = FS_CONTEXT.lock().resolve(&path)?;
    let exe_path = loc.absolute_path()?.to_string();
    let exe_name = loc.name();
    {
        let mut probe = new_user_aspace_empty()?;
        copy_from_kernel(&mut probe)?;
        load_user_app(&mut probe, Some(path.as_str()), &args, &envs)?;
    }

    // Kill every sibling thread and wait until they are reaped so we are the
    // sole owner of the address space before reloading the ELF.
    //
    // The loop re-reads `proc.threads()` every iteration: if a sibling spawns
    // yet another thread via clone() between our SIGKILL broadcast and its own
    // termination, we will pick the new tid up on the next pass.
    let sigkill = SignalInfo::new_kernel(Signo::SIGKILL);
    loop {
        let siblings: Vec<Pid> = proc_data
            .proc
            .threads()
            .into_iter()
            .filter(|tid| *tid != my_tid)
            .collect();
        if siblings.is_empty() {
            break;
        }

        info!(
            "sys_execve: killing {} sibling thread(s) before exec",
            siblings.len()
        );
        for tid in &siblings {
            // Failure just means the thread is already gone — the next
            // iteration's `threads()` snapshot will confirm that.
            let _ = send_signal_to_thread(None, *tid, Some(sigkill.clone()));
        }

        // Park until at least one thread exits, then re-check the whole set.
        // `interruptible` lets a SIGKILL targeted at *us* unblock this wait
        // instead of hanging forever.
        block_on(interruptible(poll_fn(|cx| {
            let remaining = proc_data
                .proc
                .threads()
                .into_iter()
                .filter(|tid| *tid != my_tid)
                .count();
            if remaining == 0 {
                Poll::Ready(Ok::<_, AxError>(()))
            } else {
                proc_data.thread_exit_event.register(cx.waker());
                // Re-check after registering: a sibling may have exited
                // between the first check and the register, and the wake that
                // fired would have found an empty waker set.
                let remaining = proc_data
                    .proc
                    .threads()
                    .into_iter()
                    .filter(|tid| *tid != my_tid)
                    .count();
                if remaining == 0 {
                    Poll::Ready(Ok(()))
                } else {
                    Poll::Pending
                }
            }
        })))??;
    }

    // The probe load above proved every fallible step works; if the second
    // mapping pass still hits a fresh OOM, the siblings are already gone and
    // the process can no longer return to userspace meaningfully — the
    // page-fault path will tear it down, matching Linux's post-de_thread
    // behavior.
    let mut aspace = proc_data.aspace.lock();
    let (entry_point, user_stack_base) =
        load_user_app(&mut aspace, Some(path.as_str()), &args, &envs)?;
    drop(aspace);

    curr.set_name(exe_name);
    *proc_data.exe_path.write() = exe_path;
    *proc_data.cmdline.write() = Arc::new(args);

    proc_data.set_heap_top(USER_HEAP_BASE);

    // POSIX: reset signal handlers to SIG_DFL, but preserve SIG_IGN across
    // exec. Flags, masks, and restorers are always reset.
    {
        let mut actions = proc_data.signal.actions.lock();
        for i in 1u8..=64 {
            let Some(signo) = Signo::from_repr(i) else {
                continue;
            };
            let keep_ignore = matches!(actions[signo].disposition, SignalDisposition::Ignore);
            actions[signo] = if keep_ignore {
                SignalAction {
                    disposition: SignalDisposition::Ignore,
                    ..SignalAction::default()
                }
            } else {
                SignalAction::default()
            };
        }
    }

    // Clear set_child_tid after exec since the original address is no longer valid
    curr.as_thread().set_clear_child_tid(0);
    // Same for robust_list: the user-space pointer is stale after a new ELF
    // replaces the address space.
    curr.as_thread().set_robust_list_head(0);

    // Close CLOEXEC file descriptors
    let mut fd_table = FD_TABLE.write();
    let cloexec_fds = fd_table
        .ids()
        .filter(|it| fd_table.get(*it).unwrap().cloexec)
        .collect::<Vec<_>>();
    for fd in cloexec_fds {
        fd_table.remove(fd);
    }
    drop(fd_table);

    uctx.set_ip(entry_point.as_usize());
    uctx.set_sp(user_stack_base.as_usize());
    Ok(0)
}
