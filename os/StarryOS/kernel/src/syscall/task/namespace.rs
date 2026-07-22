use alloc::sync::Arc;
use core::ops::DerefMut;

use ax_errno::{AxError, AxResult};
use ax_fs_ng::{FS_CONTEXT, FsContext};
use ax_kspin::SpinRwLock;
use ax_sync::Mutex;
use ax_task::current;
use axnsproxy::NsProxy;
use flatten_objects::FlattenObjects;
use linux_raw_sys::general::{
    CLONE_FILES, CLONE_FS, CLONE_NEWCGROUP, CLONE_NEWIPC, CLONE_NEWNET, CLONE_NEWNS, CLONE_NEWPID,
    CLONE_NEWUSER, CLONE_NEWUTS,
};

use crate::{
    file::{FD_TABLE, FileDescriptor, NsFd, PidFd, get_file_like},
    task::{AX_FILE_LIMIT, AsThread, Thread, get_task},
};

const UNSHARE_NAMESPACE_FLAGS: u32 = CLONE_NEWUTS
    | CLONE_NEWPID
    | CLONE_NEWNS
    | CLONE_NEWNET
    | CLONE_NEWIPC
    | CLONE_NEWUSER
    | CLONE_NEWCGROUP;

const SUPPORTED_NS_FLAGS: u32 = UNSHARE_NAMESPACE_FLAGS | CLONE_FS | CLONE_FILES;

const SUPPORTED_SETNS_FLAGS: u32 = SUPPORTED_NS_FLAGS & !CLONE_FILES;

type SharedFileTable = Arc<SpinRwLock<FlattenObjects<FileDescriptor, AX_FILE_LIMIT>>>;

struct PreparedUnshare {
    file_table: Option<SharedFileTable>,
    fs_context: Option<Arc<Mutex<FsContext>>>,
    nsproxy: Option<NsProxy>,
}

impl PreparedUnshare {
    fn prepare(flags: u32, thread: &Thread) -> AxResult<Self> {
        let file_table = (flags & CLONE_FILES != 0).then(|| {
            Arc::new(SpinRwLock::new(
                crate::file::current_fd_table().read().clone(),
            ))
        });

        let mut nsproxy = (flags & UNSHARE_NAMESPACE_FLAGS != 0)
            .then(|| thread.proc_data.nsproxy.lock().clone_for_unshare());
        if let Some(nsproxy) = &mut nsproxy {
            if flags & CLONE_NEWUTS != 0 {
                nsproxy.unshare_uts();
            }
            if flags & CLONE_NEWPID != 0 {
                nsproxy.prepare_child_pid_ns();
            }
            if flags & CLONE_NEWNET != 0 {
                nsproxy.unshare_net();
            }
            if flags & CLONE_NEWIPC != 0 {
                nsproxy.unshare_ipc();
            }
            if flags & CLONE_NEWUSER != 0 {
                nsproxy.unshare_user();
            }
            if flags & CLONE_NEWCGROUP != 0 {
                nsproxy.unshare_cgroup();
            }
        }

        let want_mount_namespace = flags & CLONE_NEWNS != 0;
        let fs_context = if want_mount_namespace || flags & CLONE_FS != 0 {
            let mut fs_context = ax_fs_ng::vfs::current_fs_context().lock().clone();
            if want_mount_namespace {
                fs_context.unshare_mount_namespace()?;
                if let Some(nsproxy) = &mut nsproxy {
                    nsproxy.unshare_mnt();
                }
            }
            Some(Arc::new(Mutex::new(fs_context)))
        } else {
            None
        };

        Ok(Self {
            file_table,
            fs_context,
            nsproxy,
        })
    }

    fn commit(self, thread: &Thread) {
        let Self {
            file_table,
            fs_context,
            nsproxy,
        } = self;

        if file_table.is_some() || fs_context.is_some() {
            thread.with_current_scope_mut(|scope| {
                if let Some(file_table) = file_table {
                    *FD_TABLE.scope_mut(scope).deref_mut() = file_table;
                }
                if let Some(fs_context) = fs_context {
                    *FS_CONTEXT.scope_mut(scope) = fs_context;
                }
            });
        }
        if let Some(nsproxy) = nsproxy {
            *thread.proc_data.nsproxy.lock() = nsproxy;
        }
    }
}

/// unshare(2) — disassociate parts of the process execution context.
pub fn sys_unshare(flags: u32) -> AxResult<isize> {
    if flags & !SUPPORTED_NS_FLAGS != 0 {
        warn!("sys_unshare: unsupported flags {:#x}", flags);
        return Err(AxError::InvalidInput);
    }

    let curr = current();
    let thread = curr.as_thread();
    let want_ns = flags & CLONE_NEWNS != 0;

    if want_ns && !thread.cred().has_cap_sys_admin() {
        return Err(AxError::OperationNotPermitted);
    }

    let prepared = PreparedUnshare::prepare(flags, thread)?;
    prepared.commit(thread);

    Ok(0)
}

/// setns(2) — reassociate the calling thread with an existing namespace.
///
/// `fd` is a file descriptor that references a namespace (obtained by
/// opening `/proc/<pid>/ns/<type>`).  `nstype` is the expected namespace
/// type (`CLONE_NEW*`); `0` means allow any type.
///
/// # Errors
///
/// * `EBADF` — `fd` is not a valid file descriptor or not a namespace fd
/// * `EINVAL` — `nstype` does not match the namespace type, or multi-threaded
///   process attempts to change PID namespace
/// * `EPERM` — insufficient privileges (e.g. user namespace restrictions)
pub fn sys_setns(fd: u32, nstype: u32) -> AxResult<isize> {
    if nstype != 0 && nstype & !SUPPORTED_SETNS_FLAGS != 0 {
        warn!("sys_setns: unsupported nstype {:#x}", nstype);
        return Err(AxError::InvalidInput);
    }

    let file_like = get_file_like(fd as i32)?;

    // ── 方式一: NsFd (from /proc/<pid>/ns/<type>) ────────────────────
    if let Some(nsfd) = file_like.downcast_ref::<NsFd>() {
        return setns_via_nsfd(nsfd, nstype);
    }

    // ── 方式二: PidFd (from pidfd_open) ─────────────────────────────
    if let Some(pidfd) = file_like.downcast_ref::<PidFd>() {
        return setns_via_pidfd(pidfd, nstype);
    }

    Err(AxError::BadFileDescriptor)
}

/// setns via an NsFd (from `/proc/<pid>/ns/<type>`).
///
/// An NsFd always references exactly one namespace type, so `nstype`
/// must either be `0` or match the fd's type.
fn setns_via_nsfd(nsfd: &NsFd, nstype: u32) -> AxResult<isize> {
    let fd_type = nsfd.ns_type();

    if nstype != 0 && nstype != fd_type {
        warn!(
            "sys_setns: nstype {:#x} does not match fd type {:#x}",
            nstype, fd_type
        );
        return Err(AxError::InvalidInput);
    }

    let curr = current();
    let thread = curr.as_thread();
    let proc_data = &thread.proc_data;

    // PID namespace: calling process stays in its current PID ns;
    // the target ns is staged to child_pid_ns and consumed by the
    // next fork/clone. Must be single-threaded (Linux check).
    if fd_type == CLONE_NEWPID {
        let thread_count = proc_data.proc.threads().len();
        if thread_count > 1 {
            warn!(
                "sys_setns: cannot change PID namespace in multi-threaded process ({} threads)",
                thread_count
            );
            return Err(AxError::InvalidInput);
        }
    }

    let mut nsproxy = proc_data.nsproxy.lock();

    match nsfd {
        NsFd::Uts(ns) => nsproxy.set_ns_uts(ns.clone()),
        NsFd::Ipc(ns) => nsproxy.set_ns_ipc(ns.clone()),
        NsFd::Mnt { ns, fs_ns } => {
            drop(nsproxy);
            ax_fs_ng::vfs::current_fs_context()
                .lock()
                .set_mount_namespace(fs_ns.clone())?;
            proc_data.nsproxy.lock().set_ns_mnt(ns.clone());
        }
        NsFd::Pid(ns) => nsproxy.set_ns_pid(ns.clone()),
        NsFd::Net(ns) => nsproxy.set_ns_net(ns.clone()),
        NsFd::Cgroup(ns) => nsproxy.set_ns_cgroup(ns.clone()),
        NsFd::User(ns) => {
            // Multi-threaded process cannot change user namespace.
            let thread_count = proc_data.proc.threads().len();
            if thread_count > 1 {
                warn!(
                    "sys_setns: cannot change user namespace in multi-threaded process ({} \
                     threads)",
                    thread_count
                );
                return Err(AxError::OperationNotPermitted);
            }
            nsproxy.set_ns_user(ns.clone());
        }
    }

    debug!(
        "sys_setns: successfully joined namespace type {:#x}",
        fd_type
    );
    Ok(0)
}

/// setns via a PidFd (from `pidfd_open(2)`).
///
/// `nstype` is a bitmask of `CLONE_NEW*` flags specifying which
/// namespaces to join from the target process.  Unlike the NsFd path,
/// this can join multiple namespaces in a single call.
fn setns_via_pidfd(pidfd: &PidFd, nstype: u32) -> AxResult<isize> {
    if nstype == 0 {
        warn!("sys_setns: nstype must be non-zero for pidfd");
        return Err(AxError::InvalidInput);
    }
    if nstype & !SUPPORTED_SETNS_FLAGS != 0 {
        warn!("sys_setns: unsupported nstype flags {:#x}", nstype);
        return Err(AxError::InvalidInput);
    }

    let target_proc = pidfd.process_data()?;
    let target_mnt_fs_ns = if nstype & CLONE_NEWNS != 0 {
        let task = get_task(target_proc.proc.pid())?;
        let scope = task.as_thread().scope.read();
        let fs_context = FS_CONTEXT.scope(&scope).clone();
        drop(scope);
        Some(fs_context.lock().mount_namespace().clone())
    } else {
        None
    };
    let target_nsproxy = target_proc.nsproxy.lock().clone_all();

    let curr = current();
    let thread = curr.as_thread();
    let proc_data = &thread.proc_data;

    // Check multi-threaded restrictions before making any changes.
    let thread_count = proc_data.proc.threads().len();
    if nstype & CLONE_NEWPID != 0 && thread_count > 1 {
        warn!(
            "sys_setns: cannot change PID namespace in multi-threaded process ({} threads)",
            thread_count
        );
        return Err(AxError::InvalidInput);
    }
    if nstype & CLONE_NEWUSER != 0 && thread_count > 1 {
        warn!(
            "sys_setns: cannot change user namespace in multi-threaded process ({} threads)",
            thread_count
        );
        return Err(AxError::OperationNotPermitted);
    }

    let mut nsproxy = proc_data.nsproxy.lock();

    if nstype & CLONE_NEWUTS != 0 {
        nsproxy.set_ns_uts(target_nsproxy.uts_ns);
    }
    if nstype & CLONE_NEWIPC != 0 {
        nsproxy.set_ns_ipc(target_nsproxy.ipc_ns);
    }
    if nstype & CLONE_NEWNS != 0 {
        drop(nsproxy);
        ax_fs_ng::vfs::current_fs_context()
            .lock()
            .set_mount_namespace(target_mnt_fs_ns.expect("target mount namespace captured"))?;
        nsproxy = proc_data.nsproxy.lock();
        nsproxy.set_ns_mnt(target_nsproxy.mnt_ns);
    }
    if nstype & CLONE_NEWPID != 0 {
        nsproxy.set_ns_pid(target_nsproxy.pid_ns);
    }
    if nstype & CLONE_NEWNET != 0 {
        nsproxy.set_ns_net(target_nsproxy.net_ns);
    }
    if nstype & CLONE_NEWUSER != 0 {
        nsproxy.set_ns_user(target_nsproxy.user_ns);
    }
    if nstype & CLONE_NEWCGROUP != 0 {
        nsproxy.set_ns_cgroup(target_nsproxy.cgroup_ns);
    }

    debug!(
        "sys_setns: successfully joined namespaces {:#x} via pidfd",
        nstype
    );
    Ok(0)
}
