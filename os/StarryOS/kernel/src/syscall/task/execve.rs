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
use ax_fs_ng::vfs::{FileLocation, current_fs_context};
use ax_runtime::hal::cpu::uspace::UserContext;
use ax_sync::PiMutex;
use kernel_elf_parser::AuxType;
use linux_raw_sys::general::{AT_EMPTY_PATH, AT_SYMLINK_NOFOLLOW};
use starry_process::Pid;
use starry_vm::vm_load_until_nul;

use crate::{
    config::USER_HEAP_BASE,
    file::{FD_TABLE, ResolveAtResult, current_fd_table, memfd::Memfd, resolve_at},
    mm::{copy_from_kernel, load_user_app, new_user_aspace_empty, vm_load_string},
    task::{current_user_task, future::block_on, rebind_task_tid, yield_now, zap_thread},
};

pub fn sys_execve(
    uctx: &mut UserContext,
    path: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> AxResult<isize> {
    let path = vm_load_string(path)?;
    let loc = current_fs_context().lock().resolve_file_location(&path)?;
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

    // Resolve dirfd + path to the `FileLocation` capability the loader reads from. A regular
    // file yields its filesystem path as the display name; an anonymous memfd
    // has no path but wraps a tmpfs-backed `FileLocation` we can still load — this
    // is systemd's `execveat(memfd, "", AT_EMPTY_PATH)` path. Other anonymous
    // fds (sockets, eventfd, …) are not executable.
    let (loc, disp_path) = match resolve_at(dirfd, Some(path.as_str()), flags)? {
        ResolveAtResult::File(loc) => {
            let disp = loc.with_operation(|view| {
                Ok(view
                    .absolute_path()
                    .map(|path| path.to_string())
                    .unwrap_or_else(|_| path.clone()))
            })?;
            (loc, disp)
        }
        ResolveAtResult::Directory(_) => return Err(AxError::PermissionDenied),
        ResolveAtResult::Other(f) => {
            let memfd = f.downcast_ref::<Memfd>().ok_or_else(|| {
                warn!("sys_execveat: exec from non-memfd anonymous fd is not supported");
                AxError::PermissionDenied
            })?;
            let loc = memfd.inner().inner().file_location();
            let disp = format!("/memfd:{} (deleted)", memfd.name());
            (loc, disp)
        }
    };

    do_execve(uctx, loc, disp_path, argv, envp)
}

/// Shared execve core (Linux's `do_execveat_common` equivalent): both
/// `sys_execve` and `sys_execveat` resolve the program to a `FileLocation`, then
/// funnel it plus the raw `argv` / `envp` user pointers here to be loaded once.
/// `path` is the display name (used for argv0-independent `comm`/`exe_path` and
/// the loader's `.sh`/shebang handling), not re-resolved against the FS.
fn do_execve(
    uctx: &mut UserContext,
    loc: FileLocation,
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

    let curr = current_user_task();
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
    // We can't use `ax_sync::PiMutex::lock` directly: its PI wait is not
    // cancelled by zap's `task.interrupt()`, and (worse) on release the loser would acquire
    // the mutex and proceed with execve on top of the holder's already-
    // committed new image. Busy-yield with an `exit_request` probe gives
    // us:
    //   - fall-through to acquisition if the holder fails before commit,
    //   - cooperative exit (EINTR → user-return → `do_exit(0, false)`) if
    //     the holder zaps us during its sibling-teardown loop,
    // without consuming any flag the user-return `process_one_signal` needs.
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
    let (mut new_name, mut new_exe_path) = loc.with_operation(|view| {
        Ok((
            view.name(),
            view.absolute_path()
                .map(|path| path.to_string())
                .unwrap_or_else(|_| path.clone()),
        ))
    })?;

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
                let shell_loc = current_fs_context()
                    .lock()
                    .resolve_file_location(shell_path)?;
                (new_name, new_exe_path) = shell_loc
                    .with_operation(|view| Ok((view.name(), view.absolute_path()?.to_string())))?;
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

        debug!(
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

    // ----------------------------------------------------------------
    // Phase 2: point of no return — commit all changes.
    // Nothing below may fail; errors here would leave the process broken.
    // ----------------------------------------------------------------

    // Linux unshares `files_struct` before applying close-on-exec. A distinct
    // process may still share this table through CLONE_FILES even though every
    // sibling in our thread group has exited, so mutating the inherited table
    // would incorrectly close its descriptors too. Clone the final table only
    // after sibling teardown, then publish the private owner in one short
    // active-scope transaction. Dropping the inherited Arc happens after the
    // scope/preemption guard has been released.
    let private_fd_table = {
        let inherited = current_fd_table();
        let snapshot = inherited.read().clone();
        Arc::new(ax_kspin::SpinRwLock::new(snapshot))
    };
    let old_fd_table = proc_data.with_current_scope_mut(|scope| {
        let mut slot = FD_TABLE.scope_cell_mut(scope);
        core::mem::replace(&mut *slot, private_fd_table.clone())
    });
    drop(old_fd_table);

    // Remove CLOEXEC entries in one short spin-protected transaction, then run
    // POSIX-lock release, wakes, and descriptor destructors only after the raw
    // table guard is gone. In particular, no FD_TABLE guard may cross into
    // address-space process-slot accounting, which uses a sleepable PI mutex.
    let fd_table_owner = private_fd_table;
    let mut fd_table = fd_table_owner.write();
    let cloexec_fds: Vec<_> = fd_table
        .ids()
        .filter(|it| fd_table.get(*it).unwrap().cloexec)
        .collect();
    let mut closing = Vec::with_capacity(cloexec_fds.len());
    for fd in cloexec_fds {
        if let Some(file) = fd_table.remove(fd) {
            closing.push(file);
        }
    }
    drop(fd_table);
    for file in closing {
        crate::file::release_locks_on_close(file);
    }

    // Replace the aspace Arc so the parent's shared Arc<PiMutex<AddrSpace>>
    // (from CLONE_VM) is never touched. The parent's page table register
    // keeps pointing at the original still-live AddrSpace.
    let new_pt_root = new_aspace.page_table_root();
    let newaspace_arc = Arc::new(PiMutex::new(new_aspace));
    proc_data.replace_aspace(newaspace_arc);
    proc_data.mark_vm_aspace_private_after_exec();

    // Switch the hardware page table now that the new aspace is installed.
    curr.switch_page_table(new_pt_root);

    curr.set_name(&new_name);
    *proc_data.exe_path.write() = new_exe_path;
    *proc_data.cmdline.write() = Arc::new(args);
    let auxv_len = auxv.len();
    let has_ldso = auxv.iter().any(|e| e.get_type() == AuxType::BASE);
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

    debug!(
        "execve: path={} entry={:#x} sp={:#x} tp={} auxv_count={} auxv_has_ldso={}",
        new_name,
        entry_point.as_usize(),
        user_stack_base,
        uctx.tls(),
        auxv_len,
        has_ldso,
    );

    // All ptrace tracees (both TRACEME and ATTACH) unconditionally
    // stop with SIGTRAP on execve (Linux ptrace(2)). PTRACE_O_TRACEEXEC
    // only controls whether the stop carries PTRACE_EVENT_EXEC data,
    // not whether the stop itself occurs.
    if proc_data.is_ptrace_traceme() || proc_data.is_ptrace_attached() {
        proc_data.set_ptrace_exec_stop_pending();
    }

    // Per-task perf: flip any `enable_on_exec` counter attached to this thread
    // to enabled and program it onto HW now (this thread is the running task).
    // `perf stat -- cmd` relies on this to start counting at the child's exec.
    #[cfg(target_arch = "aarch64")]
    crate::perf::task::on_exec(thr);
    // Emit COMM + MMAP2 side-band records for the new image so `perf report` can
    // symbolize this task's samples (the new aspace + name are committed above).
    #[cfg(target_arch = "aarch64")]
    crate::perf::task::on_exec_sideband(thr);

    // Unblock a vfork parent waiting for this child to exec.
    // Must be last: by now CLOEXEC fds are closed so the parent's pipe
    // read will see EOF correctly.
    proc_data.notify_vfork_done();

    Ok(0)
}
