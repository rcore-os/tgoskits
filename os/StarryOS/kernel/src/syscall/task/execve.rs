use alloc::{
    format,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::{
    ffi::{c_char, c_int},
    future::poll_fn,
    iter,
    task::Poll,
};

use ax_errno::{AxError, AxResult};
use ax_fs_ng::vfs::FS_CONTEXT;
use ax_runtime::hal::cpu::uspace::UserContext;
use ax_sync::Mutex;
use ax_task::{current, future::block_on, yield_now};
use axfs_ng_vfs::Location;
use linux_raw_sys::general::{AT_EMPTY_PATH, AT_SYMLINK_NOFOLLOW};
use starry_process::Pid;
use starry_vm::vm_load_until_nul;

use crate::{
    config::USER_HEAP_BASE,
    file::{FD_TABLE, ResolveAtResult, memfd::Memfd, resolve_at},
    mm::{copy_from_kernel, load_user_app, new_user_aspace_empty, vm_load_string},
    task::{AsThread, rebind_task_tid, zap_thread},
};

pub fn sys_execve(
    uctx: &mut UserContext,
    path: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> AxResult<isize> {
    let path = vm_load_string(path)?;
    let loc = FS_CONTEXT.lock().resolve(&path)?;
    do_execve(uctx, loc, path, argv, envp)
}

/// execveat(2) — like execve, but the program is identified by `dirfd` plus
/// `path` (resolved relative to `dirfd`), or by `dirfd` alone when
/// `AT_EMPTY_PATH` is set and `path` is empty.
pub fn sys_execveat(
    uctx: &mut UserContext,
    dirfd: c_int,
    path: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
    flags: u32,
) -> AxResult<isize> {
    if flags & !(AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW) != 0 {
        return Err(AxError::InvalidInput);
    }

    let path = vm_load_string(path)?;

    // Resolve dirfd + path to the `Location` the loader reads from. A regular
    // file yields its filesystem path as the display name; an anonymous memfd
    // has no path but wraps a tmpfs-backed `Location` we can still load — this
    // is systemd's `execveat(memfd, "", AT_EMPTY_PATH)` path. Other anonymous
    // fds (sockets, eventfd, …) are not executable.
    let (loc, disp_path) = match resolve_at(dirfd, Some(path.as_str()), flags)? {
        ResolveAtResult::File(loc) => {
            let disp = loc.absolute_path().map(|p| p.to_string()).unwrap_or(path);
            (loc, disp)
        }
        ResolveAtResult::Other(f) => {
            let memfd = f.downcast_ref::<Memfd>().ok_or_else(|| {
                warn!("sys_execveat: exec from non-memfd anonymous fd is not supported");
                AxError::PermissionDenied
            })?;
            let loc = memfd.inner().inner().location().clone();
            let disp = format!("/memfd:{} (deleted)", memfd.name());
            (loc, disp)
        }
    };

    do_execve(uctx, loc, disp_path, argv, envp)
}

/// Shared execve core (Linux's `do_execveat_common` equivalent): both
/// `sys_execve` and `sys_execveat` resolve the program to a `Location`, then
/// funnel it plus the raw `argv` / `envp` user pointers here to be loaded once.
/// `path` is the display name (used for argv0-independent `comm`/`exe_path` and
/// the loader's `.sh`/shebang handling), not re-resolved against the FS.
fn do_execve(
    uctx: &mut UserContext,
    loc: Location,
    path: String,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> AxResult<isize> {
    // ----------------------------------------------------------------
    // Phase 1: all fallible work — nothing is committed yet.
    // If any of these fail we return an error and the process is intact.
    // ----------------------------------------------------------------

    // A NULL vector pointer is accepted as an empty list: glibc's
    // `execl(path, NULL)` passes NULL to mean "no arguments", and Linux's
    // `count_strings_kernel` short-circuits NULL to an empty list rather
    // than returning EFAULT.
    let load_vec = |ptr: *const *const c_char| -> AxResult<Vec<String>> {
        if ptr.is_null() {
            Ok(Vec::new())
        } else {
            vm_load_until_nul(ptr)?
                .into_iter()
                .map(vm_load_string)
                .collect::<Result<Vec<_>, _>>()
        }
    };
    let mut args = load_vec(argv)?;
    let envs = load_vec(envp)?;

    // Linux still supplies an empty string as argv[0] to the new image, so
    // normalize an empty argv here.
    if args.is_empty() {
        args.push(String::new());
    }

    debug!("do_execve <= path: {path:?}, args: {args:?}, envs: {envs:?}");

    let curr = current();
    let thr = curr.as_thread();
    let proc_data = &thr.proc_data;
    let my_tid = thr.tid();
    let tgid = proc_data.proc.pid();

    // Serialize concurrent execve from sibling threads.
    //
    // `try_lock` alone would let a loser fail with EINTR even while the
    // holder is still in the *fallible* phase (path resolve / ELF load):
    // if the holder then errored out and released the lock, the loser
    // would have wrongly given up on an execve that could have succeeded
    // on its own image. We wait for the lock instead, and only bail when
    // the holder has crossed into irreversible teardown — which we observe
    // by `zap_thread` setting our `exit_request`.
    //
    // We can't use `ax_sync::Mutex::lock` directly: it sleeps on
    // `WaitQueue::wait_until`, which is not awakened by zap's
    // `task.interrupt()`, and (worse) on release the loser would acquire
    // the mutex and proceed with execve on top of the holder's already-
    // committed new image. Busy-yield with an `exit_request` probe gives
    // us:
    //   - fall-through to acquisition if the holder fails before commit,
    //   - cooperative exit (EINTR → user-return → `do_exit(0, false)`) if
    //     the holder zaps us during its sibling-teardown loop,
    // without consuming any flag the user-return `check_signals` needs.
    //
    // Note: we deliberately do *not* abort on generic `task.interrupt()`
    // (signal wakeups). Linux's execve is killable but not arbitrarily
    // signal-interruptible while it serializes through `cred_guard_mutex`.
    let _exec_guard = loop {
        if let Some(g) = proc_data.exec_lock.try_lock() {
            break g;
        }
        if thr.has_exit_request() {
            return Err(AxError::Interrupted);
        }
        yield_now();
    };

    // Collect metadata from the already-resolved location before touching
    // anything. An anonymous memfd has no filesystem path, so fall back to the
    // caller-supplied display name (e.g. `/memfd:<name> (deleted)`).
    let mut new_name = loc.name().to_string();
    let mut new_exe_path = loc
        .absolute_path()
        .map(|p| p.to_string())
        .unwrap_or_else(|_| path.clone());

    // Build the new address space entirely before committing.
    // Loading into a fresh aspace (rather than clearing the existing one)
    // ensures a CLONE_VM parent's mappings are never disturbed —
    // posix_spawn uses CLONE_VM|CLONE_VFORK and runs the child on a stack
    // slice inside the parent's address space. The fully-loaded aspace
    // also acts as the bprm-equivalent: the executable contents are
    // pinned now, so the post-teardown commit phase doesn't re-resolve
    // the pathname (the FS could change while siblings are being reaped).
    let mut new_aspace = new_user_aspace_empty()?;
    copy_from_kernel(&mut new_aspace)?;
    let (entry_point, user_stack_base, auxv) =
        match load_user_app(&mut new_aspace, loc, &path, &args, &envs) {
            Ok(result) => result,
            Err(AxError::InvalidExecutable) => {
                // ENOEXEC fallback: retry via /bin/sh.
                // In Linux this retry is done by user-space (execvp / busybox),
                // not by the kernel. This is a pragmatic workaround until
                // musl's execvp or busybox's ENOEXEC handling is available.
                let shell_path = "/bin/sh";
                let shell_loc = FS_CONTEXT.lock().resolve(shell_path)?;
                new_name = shell_loc.name().to_string();
                new_exe_path = shell_loc.absolute_path()?.to_string();
                args = iter::once(String::from(shell_path))
                    .chain(args.iter().cloned())
                    .collect();
                load_user_app(&mut new_aspace, shell_loc, shell_path, &args, &envs)?
            }
            Err(e) => return Err(e),
        };

    // ----------------------------------------------------------------
    // Sibling teardown (multi-thread only).
    // Zap each sibling so it does a thread-only `do_exit(0, false)` —
    // not a process-fatal SIGKILL — and wait until the thread group
    // contains only the caller before committing.
    //
    // The wait is *not* interruptible: once siblings are zapped the
    // teardown is irreversible, and EINTR here would leave the process
    // partially de-threaded but still running on the old aspace. Any
    // self-fatal signal targeting the caller will be delivered after
    // the commit phase via the user-space return path.
    //
    // Re-snapshot every iteration: a sibling may have spawned yet
    // another thread between our zap broadcast and its own exit, and
    // that new thread's tid wasn't visible last time around.
    // ----------------------------------------------------------------
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
            "sys_execve: zapping {} sibling thread(s) before exec",
            siblings.len()
        );
        for tid in &siblings {
            // Best-effort: target may already be reaped.
            let _ = zap_thread(*tid);
        }

        block_on(poll_fn(|cx| {
            let remaining = proc_data
                .proc
                .threads()
                .into_iter()
                .filter(|tid| *tid != my_tid)
                .count();
            if remaining == 0 {
                return Poll::Ready(());
            }
            // Registration happens from execve task context.
            unsafe {
                proc_data
                    .thread_exit_event
                    .register(cx.waker(), axpoll::IoEvents::IN)
            };
            // Re-check after registering: a sibling could have exited
            // between the first check and the register, and the wake
            // that fired then would have found an empty waker set.
            let remaining = proc_data
                .proc
                .threads()
                .into_iter()
                .filter(|tid| *tid != my_tid)
                .count();
            if remaining == 0 {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }));
    }

    // Collect CLOEXEC fds to close *after* sibling teardown. Snapshotting
    // before teardown would miss any fd a sibling promoted to CLOEXEC (via
    // `open(... O_CLOEXEC)`, `fcntl(F_SETFD)`, or `close_range(..., CLOEXEC)`)
    // between our snapshot and its own exit, leaking those fds into the new
    // image. Once all siblings are reaped, the snapshot reflects the final
    // post-quiescence table. The close pass below runs under the same
    // `FD_TABLE.write()` guard so no new fds appear between scan and close.
    let mut fd_table = FD_TABLE.write();
    let cloexec_fds: Vec<_> = fd_table
        .ids()
        .filter(|it| fd_table.get(*it).unwrap().cloexec)
        .collect();

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
    proc_data.mark_vm_aspace_private_after_exec();

    // Switch the hardware page table now that the new aspace is installed.
    curr.switch_page_table(new_pt_root);

    curr.set_name(&new_name);
    *proc_data.exe_path.write() = new_exe_path;
    *proc_data.cmdline.write() = Arc::new(args);
    *proc_data.auxv.write() = auxv;

    proc_data.set_heap_top(USER_HEAP_BASE);

    // Reset signal state for the new image, per POSIX/Linux semantics
    // (see `flush_signal_handlers` + `do_execveat_common` in Linux):
    //
    //   - Custom user handlers go back to SIG_DFL with cleared flags/mask.
    //   - Explicit `SIG_IGN` is preserved across exec (POSIX); default
    //     dispositions stay `SIG_DFL` even when the signal's default
    //     action is Ignore.
    //   - Pending signals at both process and thread level are *kept*:
    //     POSIX requires that signals already queued (including blocked
    //     ones) survive `execve` and be delivered against the new image's
    //     handlers. The blocked-signals mask itself is also preserved.
    //   - The alternate signal stack registered via `sigaltstack` is
    //     reset, since its `ss_sp` pointed into the old aspace which is
    //     no longer mapped.
    proc_data.signal.reset_actions_for_exec();
    thr.signal.reset_stack();
    proc_data.posix_timers.clear();

    // Pointers cached in the thread that referenced user memory in the
    // OLD aspace are now dangling. Clear them so subsequent syscalls and
    // the thread-exit path don't dereference freed user pages.
    thr.set_clear_child_tid(0);
    thr.set_robust_list_head(0);
    thr.clear_rseq_state();

    // Remove CLOEXEC fds from the table under the write guard we took
    // for the post-teardown snapshot — no fd can be added or have its
    // CLOEXEC bit flipped between scan and close — but defer the actual
    // `release_locks_on_close` (POSIX-lock release, OFD waker wakes,
    // FileDescriptor drop) until after we've dropped the table write
    // lock. The wakers fire on the global advisory-lock waiter queues
    // and may immediately drive woken tasks back through `FD_TABLE`;
    // running them under the write guard would risk lock re-entry and
    // also expand the critical section across arbitrary destructor work.
    // Linux's `do_close_on_exec` drops `files->file_lock` around each
    // `filp_close` call for the same reason. We close the entire batch
    // after the lock is released, which is equivalent: no new fd can
    // appear in the slots we just emptied because nothing else in this
    // process is running yet (siblings reaped, new image not started).
    let mut closing = Vec::with_capacity(cloexec_fds.len());
    for fd in cloexec_fds {
        if let Some(f) = fd_table.remove(fd) {
            closing.push(f);
        }
    }
    drop(fd_table);
    for f in closing {
        crate::file::release_locks_on_close(f);
    }

    // de_thread leader transfer (non-leader caller only).
    //
    // After the sibling-teardown loop above, the only remaining task in
    // this thread group is `curr`. If `curr` is not the original leader,
    // Linux's `de_thread()` transfers the leader's TID/TGID identity to
    // the calling thread via `exchange_tids` / `transfer_pid` so that
    // `gettid() == getpid()` holds in the new image, and the parent's
    // existing handle on the (still-original) PID continues to refer to
    // this thread for `wait`, `kill`, `tgkill`, `/proc/<pid>` etc.
    //
    // We mirror that here by:
    //   - renaming our `Thread::tid` from the old non-leader value to
    //     the leader's TGID,
    //   - re-keying the global TASK_TABLE entry,
    //   - re-keying the process-level signal child list,
    //   - replacing our entry in `proc.tg.threads`.
    //
    // The original leader was zapped above (it's a sibling from `curr`'s
    // viewpoint), did its `do_exit(0, false)`, and is no longer in the
    // task table or thread group, so the destination TID is free.
    if my_tid != tgid {
        thr.set_tid(tgid);
        rebind_task_tid(&curr, my_tid, tgid);
        proc_data.signal.rename_child(my_tid, tgid);
        proc_data.proc.rename_thread(my_tid, tgid);
    }

    // Reset every user-visible register to a fresh-process state, not
    // just IP/SP. Linux's `start_thread()` clears all GP registers,
    // resets the TLS pointer, and clobbers any FP/SIMD state to the
    // ABI default; leaving the syscall trapframe partially populated
    // would let the new image observe leftover argv/envp pointers,
    // a stale TLS base set by the pre-exec image, etc. Building a new
    // `UserContext` matches what `entry::run_user_app` does for the
    // init process — the only state the new image legitimately
    // inherits is the address space and the kernel/scheduler bits we
    // explicitly preserved above.
    *uctx = UserContext::new(entry_point.as_usize(), user_stack_base, 0);

    if proc_data.is_ptrace_traceme() {
        proc_data.set_ptrace_exec_stop_pending();
    }

    // Unblock a vfork parent waiting for this child to exec.
    // Must be last: by now CLOEXEC fds are closed so the parent's pipe
    // read will see EOF correctly.
    proc_data.notify_vfork_done();

    Ok(0)
}
