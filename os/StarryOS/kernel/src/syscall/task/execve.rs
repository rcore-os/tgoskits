use alloc::{
    format,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::{
    ffi::{c_char, c_int},
    mem::size_of,
};

use ax_fs_ng::vfs::current_fs_context;
use ax_runtime::hal::cpu::uspace::UserContext;
use axfs_ng_vfs::Location;
use kernel_elf_parser::AuxType;
use linux_raw_sys::general::{AT_EMPTY_PATH, AT_SYMLINK_NOFOLLOW};
use starry_vm::VmError;

use crate::{
    StarryError, StarryResult,
    file::{ResolveAtResult, memfd::Memfd, resolve_at},
    mm::{
        MAX_EXEC_ARG_BYTES, MmHandle, load_user_app, new_user_image_builder,
        validate_exec_arg_size, vm_load_string, vm_load_until_nul,
    },
    sync::{InterruptibleMutexExt, PiMutex},
    task::{TidNumber, future::block_on, zap_thread},
};

fn commit_address_space_handoff<OldAddressSpace>(
    publish_new: impl FnOnce() -> OldAddressSpace,
    install_new: impl FnOnce(),
    release_old: impl FnOnce(OldAddressSpace),
) {
    let old_address_space = publish_new();
    install_new();
    release_old(old_address_space);
}

fn charge_exec_arg_bytes(total: &mut usize, bytes: usize) -> StarryResult {
    *total = total
        .checked_add(bytes)
        .ok_or(StarryError::ArgumentListTooLong)?;
    if *total > MAX_EXEC_ARG_BYTES {
        return Err(StarryError::ArgumentListTooLong);
    }
    Ok(())
}

fn exec_arg_vm_error(error: VmError) -> StarryError {
    match error {
        VmError::TooLong => StarryError::ArgumentListTooLong,
        error => error.into(),
    }
}

/// Copy one user-provided argv or envp vector while enforcing a shared budget.
fn load_exec_vec(
    current: &crate::task::UserTaskRef,
    ptr: *const *const c_char,
    total: &mut usize,
) -> StarryResult<Vec<String>> {
    if ptr.is_null() {
        return Ok(Vec::new());
    }

    let pointers = vm_load_until_nul(current, ptr).map_err(exec_arg_vm_error)?;
    let pointer_bytes = pointers
        .len()
        .checked_add(1)
        .and_then(|count| count.checked_mul(size_of::<*const c_char>()))
        .ok_or(StarryError::ArgumentListTooLong)?;
    charge_exec_arg_bytes(total, pointer_bytes)?;

    let mut values = Vec::with_capacity(pointers.len());
    for ptr in pointers {
        let value = vm_load_string(current, ptr).map_err(|error| match error {
            StarryError::Vm(error) => exec_arg_vm_error(error),
            error => error,
        })?;
        let string_bytes = value
            .len()
            .checked_add(1)
            .ok_or(StarryError::ArgumentListTooLong)?;
        charge_exec_arg_bytes(total, string_bytes)?;
        values.push(value);
    }
    Ok(values)
}

pub fn sys_execve(
    current: &crate::task::UserTaskRef,
    uctx: &mut UserContext,
    path: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> crate::StarryResult<isize> {
    let path = vm_load_string(current, path)?;
    let loc = if let Some(fd) = self_fd_number(&path) {
        match resolve_at(fd, Some(""), AT_EMPTY_PATH)? {
            ResolveAtResult::File(loc) => loc,
            ResolveAtResult::Other(file) => file
                .downcast_ref::<Memfd>()
                .ok_or(StarryError::PermissionDenied)?
                .inner()
                .inner()
                .location()
                .clone(),
        }
    } else {
        current_fs_context().lock().resolve(&path)?
    };
    do_execve(current, uctx, loc, path, argv, envp)
}

fn self_fd_number(path: &str) -> Option<c_int> {
    ["/proc/self/fd/", "/dev/fd/"]
        .into_iter()
        .find_map(|prefix| path.strip_prefix(prefix))?
        .parse()
        .ok()
}

/// execveat(2) — like execve, but the program is identified by `dirfd` plus
/// `path` (resolved relative to `dirfd`), or by `dirfd` alone when
/// `AT_EMPTY_PATH` is set and `path` is empty.
pub fn sys_execveat(
    current: &crate::task::UserTaskRef,
    uctx: &mut UserContext,
    dirfd: c_int,
    path: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
    flags: u32,
) -> StarryResult<isize> {
    if flags & !(AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW) != 0 {
        return Err(StarryError::InvalidInput);
    }

    let path = vm_load_string(current, path)?;

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
                StarryError::PermissionDenied
            })?;
            let loc = memfd.inner().inner().location().clone();
            let disp = format!("/memfd:{} (deleted)", memfd.name());
            (loc, disp)
        }
    };

    do_execve(current, uctx, loc, disp_path, argv, envp)
}

/// Shared execve core (Linux's `do_execveat_common` equivalent): both
/// `sys_execve` and `sys_execveat` resolve the program to a `Location`, then
/// funnel it plus the raw `argv` / `envp` user pointers here to be loaded once.
/// `path` is the display name (used for argv0-independent `comm`/`exe_path` and
/// the loader's shebang handling), not re-resolved against the FS.
fn do_execve(
    current: &crate::task::UserTaskRef,
    uctx: &mut UserContext,
    loc: Location,
    path: String,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> StarryResult<isize> {
    // ----------------------------------------------------------------
    // Phase 1: all fallible work — nothing is committed yet.
    // If any of these fail we return an error and the process is intact.
    // ----------------------------------------------------------------

    // A NULL vector pointer is accepted as an empty list: glibc's
    // `execl(path, NULL)` passes NULL to mean "no arguments", and Linux's
    // `count_strings_kernel` short-circuits NULL to an empty list rather
    // than returning EFAULT.
    let mut arg_bytes = 0;
    let mut args = load_exec_vec(current, argv, &mut arg_bytes)?;
    let envs = load_exec_vec(current, envp, &mut arg_bytes)?;

    // Linux still supplies an empty string as argv[0] to the new image, so
    // normalize an empty argv here.
    if args.is_empty() {
        charge_exec_arg_bytes(&mut arg_bytes, 1)?;
        args.push(String::new());
    }
    validate_exec_arg_size(&args, &envs)?;

    debug!("do_execve <= path: {path:?}, args: {args:?}, envs: {envs:?}");

    let curr = current;
    let thr = curr.as_thread();
    let proc_data = &thr.proc_data;
    let my_tid = thr.tid_number();
    let former_tid = thr.pid_identity().snapshot();
    let leader_tid = TidNumber::from(proc_data.proc.pid().pid_number());

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
    // PREEMPT_RT turns this mutex into an rtmutex. Its wait loop first tries
    // to take a published ownerless handoff, then checks the kill condition,
    // and removes a cancelled waiter together with its PI donation. The
    // Starry kill condition is the persistent sibling `exit_request`: generic
    // signal wakeups must not abort this serialization boundary.
    //
    // Note: we deliberately do *not* abort on generic `task.interrupt()`
    // (signal wakeups). Linux's execve is killable but not arbitrarily
    // signal-interruptible while it serializes through `cred_guard_mutex`.
    let _exec_guard = proc_data
        .exec_lock()
        .lock_interruptible(|| thr.has_exit_request())
        .map_err(|_| crate::StarryError::Interrupted)?;

    // Collect metadata from the already-resolved location before touching
    // anything. An anonymous memfd has no filesystem path, so fall back to the
    // caller-supplied display name (e.g. `/memfd:<name> (deleted)`).
    let new_name = loc.name().to_string();
    let new_exe_path = loc
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
    let mut image_builder = new_user_image_builder()?;
    let loaded_image = load_user_app(&mut image_builder, loc, &path, &args, &envs, &thr.cred())?;
    let prepared_image = image_builder.finish(loaded_image)?;
    let (new_aspace, entry_point, user_stack_base, auxv) = prepared_image.into_parts();

    // Registration, runtime ownership and process metadata must all be ready
    // before the first sibling is killed, which is already irreversible.
    let inherited_thp_mode = proc_data.transparent_huge_page_mode();
    let newaspace_arc = Arc::new(PiMutex::new(new_aspace));
    let new_mm = MmHandle::from_arc(newaspace_arc).map_err(|_| StarryError::BadState)?;
    new_mm.set_transparent_huge_page_mode(inherited_thp_mode);

    let scheduler_address_space =
        crate::task::scheduler_address_space(&new_mm).map_err(|_| StarryError::BadState)?;
    let prepared_memory = crate::task::PreparedProcessMemory::new(new_mm);
    let new_cmdline = Arc::new(args);
    let new_envp = Arc::new(envs);

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
        let siblings: Vec<TidNumber> = proc_data
            .proc
            .threads()
            .into_iter()
            .filter(|tid| *tid != my_tid)
            .collect();
        let leader_exit_complete =
            my_tid == leader_tid || proc_data.retired_leader_transfer_ready();
        if siblings.is_empty() && leader_exit_complete {
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

        block_on(crate::task::wait_on_pollset(
            proc_data.thread_exit_event(),
            || {
                let remaining = proc_data
                    .proc
                    .threads()
                    .into_iter()
                    .filter(|tid| *tid != my_tid)
                    .count();
                let leader_exit_complete =
                    my_tid == leader_tid || proc_data.retired_leader_transfer_ready();
                (remaining == 0 && leader_exit_complete).then_some(())
            },
        ));
    }

    // ----------------------------------------------------------------
    // Phase 2: point of no return — commit all changes.
    // Nothing below may fail; errors here would leave the process broken.
    // ----------------------------------------------------------------

    // Publish only this process's new MM. CLONE_VM peers keep their owner.
    commit_address_space_handoff(
        || proc_data.stage_memory_replacement(prepared_memory),
        || curr.switch_address_space(scheduler_address_space),
        |old_memory| old_memory.retire(),
    );

    // PR_SET_KEEPCAPS is deliberately not inherited by a new executable
    // image. Do this only after crossing the point of no return so a failed
    // exec leaves the caller's credential state untouched.
    let old_cred = thr.cred();
    if old_cred.keep_capabilities() {
        let mut new_cred = (*old_cred).clone();
        new_cred.set_keep_capabilities(false);
        thr.set_cred(new_cred);
    }

    curr.set_name(&new_name);
    proc_data.set_exe_path(new_exe_path);
    proc_data.set_cmdline(new_cmdline);
    proc_data.set_envp(new_envp);
    let auxv_len = auxv.len();
    let has_ldso = auxv.iter().any(|e| e.get_type() == AuxType::BASE);
    proc_data.set_auxv(auxv);

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
    thr.signal().reset_stack();
    proc_data.posix_timers().clear();

    // Pointers cached in the thread that referenced user memory in the
    // OLD aspace are now dangling. Clear them so subsequent syscalls and
    // the thread-exit path don't dereference freed user pages.
    thr.set_clear_child_tid(0);
    thr.set_robust_list_head(0);
    thr.clear_rseq_state();

    // Scan after sibling teardown so their final CLOEXEC changes are visible.
    // As in Linux do_close_on_exec, detach one owner under the table lock and
    // perform filp_close-equivalent callbacks after unlocking. No temporary
    // Vec allocation or last descriptor drop occurs under the table guard.
    let fd_table_owner = crate::file::current_fd_table();
    let mut cursor = 0;
    loop {
        let closing = {
            let mut table = fd_table_owner.write();
            let next = table
                .ids()
                .find(|fd| *fd >= cursor && table.get(*fd).is_some_and(|entry| entry.cloexec));
            next.and_then(|fd| table.remove(fd).map(|descriptor| (fd, descriptor)))
        };
        let Some((fd, descriptor)) = closing else {
            break;
        };
        cursor = fd + 1;
        crate::file::release_locks_on_close(descriptor);
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
    // We mirror that here by transferring the stable leader identity to the
    // caller, then updating signal and thread-group indexes that use TIDs.
    //
    // The original leader was zapped above (it's a sibling from `curr`'s
    // viewpoint), did its `do_exit(0, false)`, and transferred its TID role to
    // the process. Taking that exact lease preserves the generation instead of
    // releasing and reacquiring a numeric slot.
    if my_tid != leader_tid {
        let old_task_identity = thr.pid_identity();
        let leader_identity = proc_data.identity();
        crate::cgroup::rename_task(proc_data, &old_task_identity, &leader_identity)
            .expect("de-threaded task must own the process's sole cgroup charge");
        let (_, leader_tid_lease) = proc_data.take_retired_leader_for_exec();
        thr.transfer_pid_identity(curr, leader_identity, leader_tid_lease);
        proc_data
            .signal
            .rename_child(my_tid.get(), leader_tid.get());
        proc_data.proc.rename_thread(my_tid, leader_tid);
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
    ax_runtime::task::reset_current_user_fp_state()
        .unwrap_or_else(|error| panic!("exec committed without a resettable FPU owner: {error}"));
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
        proc_data.set_ptrace_exec_stop_pending(former_tid);
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

#[cfg(all(test, not(axtest)))]
mod tests {
    use alloc::vec;
    use core::cell::RefCell;

    use super::commit_address_space_handoff;

    #[test]
    fn address_space_handoff_installs_before_releasing_old() {
        let events = RefCell::new(vec![]);

        commit_address_space_handoff(
            || {
                events.borrow_mut().push("publish");
                "old"
            },
            || events.borrow_mut().push("install"),
            |old| {
                assert_eq!(old, "old");
                events.borrow_mut().push("release");
            },
        );

        assert_eq!(*events.borrow(), ["publish", "install", "release"]);
    }
}
