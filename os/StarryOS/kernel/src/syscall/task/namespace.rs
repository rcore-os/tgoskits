use alloc::sync::Arc;

use ax_fs_ng::FsSharingRequirement;
use linux_raw_sys::general::{
    CAP_SYS_ADMIN, CLONE_FILES, CLONE_FS, CLONE_NEWCGROUP, CLONE_NEWIPC, CLONE_NEWNET, CLONE_NEWNS,
    CLONE_NEWPID, CLONE_NEWUSER, CLONE_NEWUTS,
};

use crate::{
    StarryError, StarryResult,
    file::{FileTable, NsFd, PidFd, get_file_like},
    namespace::NsProxy,
    sync::RawSpinRwLock,
    task::{NamespaceChange, UserTaskRef, authorize_pidfd_namespace_read},
};

const UNSHARE_NAMESPACE_FLAGS: u32 = CLONE_NEWUTS
    | CLONE_NEWPID
    | CLONE_NEWNS
    | CLONE_NEWNET
    | CLONE_NEWIPC
    | CLONE_NEWUSER
    | CLONE_NEWCGROUP;
const SUPPORTED_NS_FLAGS: u32 = UNSHARE_NAMESPACE_FLAGS | CLONE_FS | CLONE_FILES;
const SUPPORTED_SETNS_FLAGS: u32 = UNSHARE_NAMESPACE_FLAGS;

// Other namespace installers retain their current initial-userns policy.
// Mount and user installation use the actual caller/target lineage below.
fn may_join_other_namespace(change: &NamespaceChange<'_>) -> bool {
    change.caller().has_cap_sys_admin() && change.original_proxy().in_initial_user_ns()
}

/// unshare(2): prepare private resources before changing either scope entry.
pub fn sys_unshare(current: &UserTaskRef, flags: usize) -> StarryResult<isize> {
    if flags & !(SUPPORTED_NS_FLAGS as usize) != 0 {
        return Err(StarryError::InvalidInput);
    }
    let flags = flags as u32;
    let thread = current.as_thread();
    let mut change = NamespaceChange::new(thread)?;
    if flags & CLONE_NEWUSER != 0 {
        if thread.proc_data.proc.threads().len() > 1 {
            return Err(StarryError::InvalidInput);
        }
        let euid = change.caller().euid;
        change.proxy_mut().unshare_user(euid)?;
    }
    if flags & (CLONE_NEWNS | CLONE_NEWCGROUP) != 0 {
        let user = change.proxy_mut().user_ns.clone();
        if !change.has_capability(&user, CAP_SYS_ADMIN) {
            return Err(StarryError::OperationNotPermitted);
        }
    }
    if flags & CLONE_NEWUTS != 0 {
        change.proxy_mut().unshare_uts();
    }
    if flags & CLONE_NEWPID != 0 {
        change
            .proxy_mut()
            .prepare_pid_ns_for_children(thread.active_pid_namespace());
    }
    if flags & CLONE_NEWNET != 0 {
        change.proxy_mut().unshare_net();
    }
    if flags & CLONE_NEWIPC != 0 {
        change.proxy_mut().unshare_ipc();
    }
    if flags & CLONE_NEWCGROUP != 0 {
        change
            .proxy_mut()
            .unshare_cgroup(thread.proc_data.cgroup_node())?;
    }
    if flags & CLONE_FILES != 0 {
        let files: Arc<RawSpinRwLock<FileTable>> = Arc::new(RawSpinRwLock::new(
            crate::file::current_fd_table().read().clone(),
        ));
        change.replace_files(crate::file::new_file_table_scope(files));
    }
    if flags & CLONE_NEWNS != 0 {
        change.prepare_namespace_clone()?;
    } else if flags & (CLONE_FS | CLONE_NEWUSER) != 0 {
        // Linux unshare NEWUSER also requests a private fs_struct.
        change.prepare_fork();
    }
    change.commit()?;
    Ok(0)
}

/// Namespace descriptors select one type; pidfds accept the supported bitmask.
/// Descriptor lookup precedes flag validation, matching Linux setns's fdget.
pub fn sys_setns(current: &UserTaskRef, fd: u32, nstype: u32) -> StarryResult<isize> {
    let file = get_file_like(fd as i32)?;
    if let Some(nsfd) = file.downcast_ref::<NsFd>() {
        if nstype != 0 && nstype != nsfd.ns_type() {
            return Err(StarryError::InvalidInput);
        }
        return setns_via_nsfd(current, nsfd);
    }
    if let Some(pidfd) = file.downcast_ref::<PidFd>() {
        if nstype == 0 || nstype & !SUPPORTED_SETNS_FLAGS != 0 {
            return Err(StarryError::InvalidInput);
        }
        return setns_via_pidfd(current, pidfd, nstype);
    }
    Err(StarryError::InvalidInput)
}

fn prepare_user_join(
    change: &mut NamespaceChange<'_>,
    target: Arc<crate::sync::RawSpinLock<crate::namespace::UserNamespace>>,
    current: &UserTaskRef,
) -> StarryResult<()> {
    // userns_install checks identity, thread group, fs shares, then capability.
    if Arc::ptr_eq(&change.original_proxy().user_ns, &target)
        || current.as_thread().proc_data.proc.threads().len() > 1
    {
        return Err(StarryError::InvalidInput);
    }
    change.require_sole_owner()?;
    if !change.has_capability(&target, CAP_SYS_ADMIN) {
        return Err(StarryError::OperationNotPermitted);
    }
    change.proxy_mut().set_ns_user(target);
    Ok(())
}

fn setns_via_nsfd(current: &UserTaskRef, nsfd: &NsFd) -> StarryResult<isize> {
    let thread = current.as_thread();
    let mut change = NamespaceChange::new(thread)?;
    if !matches!(
        nsfd.object(),
        crate::file::NamespaceObject::Mnt(_) | crate::file::NamespaceObject::User(_)
    ) && !may_join_other_namespace(&change)
    {
        return Err(StarryError::OperationNotPermitted);
    }
    match nsfd.object() {
        crate::file::NamespaceObject::User(ns) => {
            prepare_user_join(&mut change, ns.clone(), current)?
        }
        crate::file::NamespaceObject::Mnt(ns) => {
            change.prepare_mount_join(ns.clone(), FsSharingRequirement::SoleOwner)?;
        }
        crate::file::NamespaceObject::Uts(ns) => change.proxy_mut().set_ns_uts(ns.clone()),
        crate::file::NamespaceObject::Ipc(ns) => change.proxy_mut().set_ns_ipc(ns.clone()),
        crate::file::NamespaceObject::Pid(ns) => {
            if thread.proc_data.proc.threads().len() > 1 {
                return Err(StarryError::InvalidInput);
            }
            change.proxy_mut().set_ns_pid(ns.clone());
        }
        crate::file::NamespaceObject::Net(ns) => change.proxy_mut().set_ns_net(ns.clone()),
        crate::file::NamespaceObject::Cgroup(ns) => change.proxy_mut().set_ns_cgroup(ns.clone()),
    }
    change.commit()?;
    Ok(0)
}

struct TargetNamespaces {
    proxy: Option<Arc<NsProxy>>,
    binding: Option<crate::task::FsBindingSnapshot>,
    pid: crate::namespace::PidNamespaceRef,
}

impl TargetNamespaces {
    fn snapshot(target: &UserTaskRef, flags: u32) -> StarryResult<Self> {
        let thread = target.as_thread();
        // Construct the owning carrier before the writer so failure drops the
        // writer first; the same gate covers proxy and paired FS selection.
        let mut snapshot = Self {
            proxy: None,
            binding: None,
            pid: thread.active_pid_namespace(),
        };
        let update = thread.proc_data.namespace_update();
        snapshot.proxy = Some(update.snapshot());
        if flags & CLONE_NEWNS != 0 {
            snapshot.binding = thread.snapshot_fs_binding_under(&update)?;
            if snapshot.binding.is_none() {
                return Err(StarryError::NoSuchProcess);
            }
        }
        drop(update);
        Ok(snapshot)
    }
}

fn setns_via_pidfd(current: &UserTaskRef, pidfd: &PidFd, flags: u32) -> StarryResult<isize> {
    let target_task = pidfd
        .process_identity()
        .live_task()
        .ok_or(StarryError::NoSuchProcess)?;
    authorize_pidfd_namespace_read(current, &target_task)?;
    let target = TargetNamespaces::snapshot(&target_task, flags)?;
    let proxy = target.proxy.as_ref().expect("selected target proxy");
    let thread = current.as_thread();
    // The target's gates have ended before entering this caller's writer.
    let mut change = NamespaceChange::new(thread)?;

    // validate_nsset installs USER, MNT, UTS, IPC, PID, CGROUP, NET in order.
    if flags & CLONE_NEWUSER != 0 {
        prepare_user_join(&mut change, proxy.user_ns.clone(), current)?;
    }
    if flags & CLONE_NEWNS != 0 {
        let sharing = if flags == CLONE_NEWNS {
            FsSharingRequirement::SoleOwner
        } else {
            FsSharingRequirement::AnyOwner
        };
        change.prepare_mount_join(
            target
                .binding
                .as_ref()
                .expect("selected mount binding")
                .mnt
                .clone(),
            sharing,
        )?;
    }
    let other_flags = flags & !(CLONE_NEWUSER | CLONE_NEWNS);
    if other_flags != 0 && !may_join_other_namespace(&change) {
        return Err(StarryError::OperationNotPermitted);
    }
    if flags & CLONE_NEWUTS != 0 {
        change.proxy_mut().set_ns_uts(proxy.uts_ns.clone());
    }
    if flags & CLONE_NEWIPC != 0 {
        change.proxy_mut().set_ns_ipc(proxy.ipc_ns.clone());
    }
    if flags & CLONE_NEWPID != 0 {
        if thread.proc_data.proc.threads().len() > 1 {
            return Err(StarryError::InvalidInput);
        }
        change.proxy_mut().set_ns_pid(target.pid.clone());
    }
    if flags & CLONE_NEWCGROUP != 0 {
        change.proxy_mut().set_ns_cgroup(proxy.cgroup_ns.clone());
    }
    if flags & CLONE_NEWNET != 0 {
        change.proxy_mut().set_ns_net(proxy.net_ns.clone());
    }
    change.commit()?;
    Ok(0)
}
