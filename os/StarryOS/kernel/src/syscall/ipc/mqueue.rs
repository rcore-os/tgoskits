//! POSIX message queue syscalls (`mq_open`, `mq_unlink`, `mq_timedsend`,
//! `mq_timedreceive`, `mq_notify`, `mq_getsetattr`).
//!
//! Mirrors Linux `ipc/mqueue.c`. The queue object, registry and limits live
//! in [`crate::ipc::mqueue`]; this module is the user-facing ABI glue:
//! argument marshalling, name lookup/creation, fd-table integration and the
//! `mq_attr`/`sigevent` wire structures.

use alloc::sync::Arc;

use ax_errno::{AxError, AxResult, LinuxError};
use linux_raw_sys::general::{
    __kernel_mode_t, __kernel_timespec, O_ACCMODE, O_CREAT, O_EXCL, O_NONBLOCK, O_RDONLY, O_RDWR,
    O_WRONLY, RLIMIT_MSGQUEUE, SIGEV_NONE, SIGEV_SIGNAL, SIGEV_THREAD, sigevent,
};
use starry_vm::{VmMutPtr, VmPtr, vm_load, vm_write_slice};

use crate::{
    file::{add_file_like, get_file_like, netlink::NetlinkSocket},
    ipc::mqueue::{
        MQ_NSIG, MQ_REGISTRY, MessageQueue, MqAttr, MqDescriptor, NOTIFY_COOKIE_LEN, NotifyRequest,
        charge_open_bytes, msg_default, msg_max, msgsize_default, msgsize_max, queues_count,
        queues_max, validate_name,
    },
    mm::vm_load_string,
    task::AsThread,
    time::TimeValueLike,
};

/// Resolve an optional absolute `CLOCK_REALTIME` timeout pointer into a
/// wall-clock deadline. A null pointer means "wait forever"; a supplied
/// timespec is validated the way Linux does (`EINVAL` on out-of-range nsec).
fn load_deadline(abs_timeout: *const __kernel_timespec) -> AxResult<Option<core::time::Duration>> {
    if abs_timeout.is_null() {
        return Ok(None);
    }
    let ts: __kernel_timespec = unsafe { abs_timeout.vm_read_uninit()?.assume_init() };
    Ok(Some(ts.try_into_time_value()?))
}

/// `mq_open(name, oflag, mode, attr)`.
///
/// Creates or opens a named queue and returns a message-queue descriptor.
/// `O_CREAT`/`O_EXCL`/`O_NONBLOCK` and the access mode are honored; when
/// `O_CREAT` supplies an `attr`, its `mq_maxmsg`/`mq_msgsize` seed the queue
/// (bounded by the system limits), otherwise the Linux defaults apply.
pub fn sys_mq_open(
    name: *const core::ffi::c_char,
    oflag: i32,
    mode: __kernel_mode_t,
    attr: *const MqAttr,
) -> AxResult<isize> {
    let raw = vm_load_string(name)?;
    let short = validate_name(&raw)?;
    let key = {
        let mut k = alloc::string::String::with_capacity(short.len() + 1);
        k.push('/');
        k.push_str(short);
        k
    };

    let oflag = oflag as u32;

    // Snapshot the caller's identity once: fsuid/fsgid stamp a newly created
    // queue and drive the access check on an existing one (Linux uses
    // current_fsuid()/current_fsgid()); the resource capability lifts the
    // unprivileged attribute ceilings and DAC-override bypasses the open check.
    let curr = ax_task::current();
    let thr = curr.as_thread();
    let cred = thr.cred();
    let (fsuid, fsgid, can_sys_resource, can_dac_override) = (
        cred.fsuid,
        cred.fsgid,
        cred.has_cap_sys_resource(),
        cred.has_cap_dac_override(),
    );
    let umask = thr.proc_data.umask();
    // The creator's `RLIMIT_MSGQUEUE` soft limit bounds the total bytes across
    // all their queues (Linux charges `mq_bytes` against the ucounts rlimit).
    let msgqueue_rlimit = thr.proc_data.rlim.read()[RLIMIT_MSGQUEUE].current;

    let mut registry = MQ_REGISTRY.lock();
    // Whether this call created the queue (so an fd-allocation failure below
    // knows to unwind the registry insert; an *existing* queue must be left in
    // place for its other openers).
    let mut created = false;
    let queue = match registry.get(&key) {
        Some(existing) => {
            if oflag & O_CREAT != 0 && oflag & O_EXCL != 0 {
                return Err(LinuxError::EEXIST.into());
            }
            // Linux `prepare_open`: the invalid access mode `O_RDWR|O_WRONLY`
            // (the 0b11 `O_ACCMODE` value) is rejected before the permission
            // check when opening an existing queue.
            if oflag & O_ACCMODE == O_RDWR | O_WRONLY {
                return Err(LinuxError::EINVAL.into());
            }
            // Opening an existing queue is a permission-checked open, mirroring
            // Linux `do_open` -> `inode_permission`; `CAP_DAC_OVERRIDE` bypasses.
            // The group tier consults the caller's fsgid *and* supplementary
            // groups (`in_group_p`), so hand the check `cred.in_group`.
            if !can_dac_override {
                existing.check_open_access(oflag & O_ACCMODE, fsuid, |gid| cred.in_group(gid))?;
            }
            existing.clone()
        }
        None => {
            if oflag & O_CREAT == 0 {
                return Err(LinuxError::ENOENT.into());
            }
            // Linux `mqueue_create_attr`: a `CAP_SYS_RESOURCE` caller may
            // exceed `mq_queues_max`; only unprivileged callers hit the cap.
            // The count is per live inode (`mq_queues_count`), not per name, so
            // a queue unlinked while still open keeps counting - use the live
            // queue count rather than `registry.len()`.
            if queues_count() >= queues_max() && !can_sys_resource {
                return Err(LinuxError::ENOSPC.into());
            }
            let (max_msg, msg_size) = if attr.is_null() {
                // Linux seeds an attr-less queue with min(mq_msg_max,
                // mq_msg_default) / min(mq_msgsize_max, mq_msgsize_default)
                // (ipc/mqueue.c:325), honoring the current sysctl tunables.
                (msg_default(), msgsize_default())
            } else {
                let a: MqAttr = attr.vm_read()?;
                // The unprivileged ceilings come from the (sysctl-tunable)
                // msg_max/msgsize_max; a `CAP_SYS_RESOURCE` caller gets the
                // hard limits instead.
                let (msg_cap, size_cap) =
                    (msg_max(can_sys_resource), msgsize_max(can_sys_resource));
                // Linux rejects non-positive or over-limit attributes with
                // EINVAL before the queue is created.
                if a.mq_maxmsg <= 0
                    || a.mq_msgsize <= 0
                    || a.mq_maxmsg as usize > msg_cap
                    || a.mq_msgsize as usize > size_cap
                {
                    return Err(LinuxError::EINVAL.into());
                }
                let (max_msg, msg_size) = (a.mq_maxmsg as usize, a.mq_msgsize as usize);
                // Linux checks `mq_msgsize > ULONG_MAX / mq_maxmsg` and returns
                // EOVERFLOW: the per-field bounds above pass independently but
                // their product (total queue bytes) must not wrap `usize`.
                if msg_size > usize::MAX / max_msg {
                    return Err(LinuxError::EOVERFLOW.into());
                }
                (max_msg, msg_size)
            };
            // Charge the queue's `mq_bytes` against the creator's
            // `RLIMIT_MSGQUEUE` *before* creating it; too-large a queue (or a
            // user already at their ceiling) fails with EMFILE and nothing is
            // registered (ipc/mqueue.c:367-381).
            let charged = charge_open_bytes(fsuid, msgqueue_rlimit, max_msg, msg_size)?;
            // Linux stamps the new mqueue inode with mode & ~umask (masked to
            // the permission bits) and the creator's fsuid/fsgid.
            let perm = ((mode as u16) & !(umask as u16)) & 0o777;
            let q = MessageQueue::new(max_msg, msg_size, perm, fsuid, fsgid, charged);
            registry.insert(key.clone(), q.clone());
            created = true;
            q
        }
    };
    drop(registry);

    // Keep an identity handle to a freshly created queue so the fd-failure
    // unwind removes exactly the binding this call added (and not a same-named
    // queue a racing unlink+create may have installed in the meantime).
    let created_queue = created.then(|| queue.clone());

    // `O_NONBLOCK` lives on the descriptor (the open file description), so it is
    // carried by `oflag`; nothing is stored on the shared queue.
    let cloexec = true; // mq descriptors are FD_CLOEXEC by default on Linux.
    // Linux `do_mq_open` reserves the descriptor slot (`get_unused_fd_flags`)
    // and only then commits the queue, unwinding everything on any later error.
    // Here fd allocation happens last, so an exhausted fd table must not leak
    // the freshly created queue: drop the registry binding we just added, which
    // releases the last strong ref and runs `MessageQueue::Drop` to refund the
    // `RLIMIT_MSGQUEUE` charge and the live-queue count. An *existing* queue is
    // untouched (`created` is false), matching Linux leaving it for its openers.
    let fd = match add_file_like(Arc::new(MqDescriptor::new(queue, oflag)), cloexec) {
        Ok(fd) => fd,
        Err(e) => {
            if let Some(created_queue) = created_queue {
                let mut registry = MQ_REGISTRY.lock();
                if registry
                    .get(&key)
                    .is_some_and(|q| Arc::ptr_eq(q, &created_queue))
                {
                    registry.remove(&key);
                }
            }
            return Err(e);
        }
    };
    Ok(fd as isize)
}

/// `mq_unlink(name)`.
///
/// Removes the name binding. Open descriptors keep the queue alive (the
/// `Arc` outlives the registry entry) until the last one is closed.
///
/// On Linux `mq_unlink` goes through the VFS: the mqueuefs root is mounted
/// sticky (`S_IFDIR | S_ISVTX | S_IRWXUGO`, ipc/mqueue.c:415), so `vfs_unlink`
/// -> `may_delete_dentry` -> `check_sticky` (fs/namei.c:3645) permits removal
/// only when the caller owns the victim (fsuid == queue uid), owns the sticky
/// dir (the mqueuefs root, created at mount as root, so fsuid == 0), or holds
/// `CAP_FOWNER`; otherwise `-EPERM`. The dir is world-writable so no extra
/// `MAY_WRITE`/`MAY_EXEC` gate applies. We enforce the same before removing.
pub fn sys_mq_unlink(name: *const core::ffi::c_char) -> AxResult<isize> {
    let raw = vm_load_string(name)?;
    let short = validate_name(&raw)?;
    let key = {
        let mut k = alloc::string::String::with_capacity(short.len() + 1);
        k.push('/');
        k.push_str(short);
        k
    };

    let curr = ax_task::current();
    let cred = curr.as_thread().cred();
    let (fsuid, can_fowner) = (cred.fsuid, cred.has_cap_fowner());

    let mut registry = MQ_REGISTRY.lock();
    let Some(queue) = registry.get(&key) else {
        return Err(LinuxError::ENOENT.into());
    };
    // `check_sticky`: owner of victim, owner of the sticky dir (mqueuefs root,
    // uid 0), or CAP_FOWNER.
    if fsuid != queue.uid() && fsuid != 0 && !can_fowner {
        return Err(LinuxError::EPERM.into());
    }
    registry.remove(&key);
    Ok(0)
}

/// Fetch the per-fd descriptor behind an mqd, rejecting non-mqueue fds with
/// `EBADF`.
fn descriptor_from_fd(mqdes: i32) -> AxResult<Arc<MqDescriptor>> {
    get_file_like(mqdes)?
        .downcast_arc::<MqDescriptor>()
        .map_err(|_| AxError::from(LinuxError::EBADF))
}

/// Fetch the shared queue behind an mqd (access mode not checked here).
fn queue_from_fd(mqdes: i32) -> AxResult<Arc<MessageQueue>> {
    Ok(descriptor_from_fd(mqdes)?.queue().clone())
}

/// `mq_timedsend(mqdes, msg, len, prio, abs_timeout)`.
pub fn sys_mq_timedsend(
    mqdes: i32,
    msg_ptr: *const u8,
    msg_len: usize,
    msg_prio: u32,
    abs_timeout: *const __kernel_timespec,
) -> AxResult<isize> {
    let desc = descriptor_from_fd(mqdes)?;
    // A queue opened O_RDONLY may not be sent to (Linux returns EBADF).
    if desc.access() == O_RDONLY {
        return Err(LinuxError::EBADF.into());
    }
    let queue = desc.queue();
    let deadline = load_deadline(abs_timeout)?;
    let data = vm_load(msg_ptr, msg_len)?;
    queue.send(&data, msg_prio, deadline, desc.is_nonblocking())?;
    Ok(0)
}

/// `mq_timedreceive(mqdes, msg, len, &prio, abs_timeout)`.
///
/// Returns the number of bytes copied. `msg_prio`, when non-null, receives the
/// priority the message was sent with.
pub fn sys_mq_timedreceive(
    mqdes: i32,
    msg_ptr: *mut u8,
    msg_len: usize,
    msg_prio: *mut u32,
    abs_timeout: *const __kernel_timespec,
) -> AxResult<isize> {
    let desc = descriptor_from_fd(mqdes)?;
    // A queue opened O_WRONLY may not be received from (Linux returns EBADF).
    if desc.access() == O_WRONLY {
        return Err(LinuxError::EBADF.into());
    }
    let queue = desc.queue();
    let deadline = load_deadline(abs_timeout)?;
    let (data, prio) = queue.receive(msg_len, deadline, desc.is_nonblocking())?;
    vm_write_slice(msg_ptr, &data)?;
    if !msg_prio.is_null() {
        msg_prio.vm_write(prio)?;
    }
    Ok(data.len() as isize)
}

/// `mq_notify(mqdes, sevp)`.
///
/// A null `sevp` unregisters the calling process. A non-null `sevp` registers
/// `SIGEV_SIGNAL`/`SIGEV_NONE`/`SIGEV_THREAD`. glibc and musl implement the
/// POSIX `SIGEV_THREAD` wrapper by opening a `PF_NETLINK` socket, spawning a
/// helper thread that reads it, and issuing this syscall with
/// `sigev_notify = SIGEV_THREAD`, `sigev_signo = <netlink fd>` and
/// `sigev_value.sival_ptr = <cookie buffer>`; the kernel pushes the cookie over
/// that socket on message arrival (ipc/mqueue.c:1287-1351, `netlink_sendskb`).
pub fn sys_mq_notify(mqdes: i32, sevp: *const sigevent) -> AxResult<isize> {
    let queue = queue_from_fd(mqdes)?;
    let pid = ax_task::current().as_thread().proc_data.proc.pid();

    let req = if sevp.is_null() {
        NotifyRequest::Unregister
    } else {
        let sev: sigevent = unsafe { sevp.vm_read_uninit()?.assume_init() };
        let kind = sev.sigev_notify as u32;
        match kind {
            SIGEV_SIGNAL => {
                // Linux `do_mq_notify` rejects an invalid signal at
                // registration time via `valid_signal(sigev_signo)`
                // (`sig <= _NSIG`, i.e. 64). `sigev_signo == 0` is accepted:
                // it registers and consumes the slot but never delivers.
                let signo = sev.sigev_signo as u32;
                if signo > MQ_NSIG {
                    return Err(LinuxError::EINVAL.into());
                }
                // `sigev_value` is a union; Linux stores the whole word in
                // `info->notify.sigev_value` and returns it as `si_value` when
                // the notification fires. Read the pointer-sized member so all
                // 64 bits survive, matching `SignalInfo::new_mqueue`.
                let value = unsafe { sev.sigev_value.sival_ptr } as i64;
                NotifyRequest::Signal {
                    signo,
                    sigev_value: value,
                }
            }
            SIGEV_NONE => NotifyRequest::None,
            SIGEV_THREAD => {
                // Resolve the netlink socket from `sigev_signo` (the fd libc
                // passed), rejecting a non-netlink fd with EBADF/EINVAL as
                // `netlink_getsockbyfd` does. Then copy the NOTIFY_COOKIE_LEN
                // cookie in from `sigev_value.sival_ptr` (EFAULT on a bad
                // pointer), mirroring do_mq_notify's `copy_from_user`.
                let fd = sev.sigev_signo;
                let sock = get_file_like(fd)?
                    .downcast_arc::<NetlinkSocket>()
                    .map_err(|_| AxError::from(LinuxError::EINVAL))?;
                let cookie_ptr = unsafe { sev.sigev_value.sival_ptr } as *const u8;
                let bytes = vm_load(cookie_ptr, NOTIFY_COOKIE_LEN)?;
                let mut cookie = [0u8; NOTIFY_COOKIE_LEN];
                cookie.copy_from_slice(&bytes);
                NotifyRequest::Thread { sock, cookie }
            }
            _ => return Err(LinuxError::EINVAL.into()),
        }
    };
    queue.register_notify(req, pid)?;
    Ok(0)
}

/// `mq_getsetattr(mqdes, newattr, oldattr)` — the shared backend for the libc
/// `mq_getattr`/`mq_setattr` wrappers.
///
/// When `newattr` is non-null, only the `O_NONBLOCK` bit of `mq_flags` is
/// applied (sizes and count are read-only). When `oldattr` is non-null, the
/// attributes *before* any change are written back.
pub fn sys_mq_getsetattr(
    mqdes: i32,
    newattr: *const MqAttr,
    oldattr: *mut MqAttr,
) -> AxResult<isize> {
    let desc = descriptor_from_fd(mqdes)?;
    let queue = desc.queue();

    // Snapshot the attributes *before* any change: the queue-wide sizes/count
    // plus this descriptor's own `O_NONBLOCK` (Linux keeps mq_flags in the
    // per-fd f_flags, so `mq_getattr` reports the descriptor's bit, not a
    // queue-shared one).
    let mut previous = queue.attr();
    previous.mq_flags = (desc.flags() & O_NONBLOCK) as i64;

    if !newattr.is_null() {
        let new: MqAttr = newattr.vm_read()?;
        // Linux `do_mq_getsetattr` rejects any bit other than `O_NONBLOCK` in
        // `mq_flags` with `EINVAL` before applying the change.
        if new.mq_flags & !(O_NONBLOCK as i64) != 0 {
            return Err(LinuxError::EINVAL.into());
        }
        desc.set_nonblocking_flag(new.mq_flags & O_NONBLOCK as i64 != 0);
        // Applying a new attr bumps the inode's atime+ctime (ipc/mqueue.c:1420).
        queue.touch_attr();
    }
    if !oldattr.is_null() {
        oldattr.vm_write(previous)?;
    }
    Ok(0)
}
