//! Linux READ_FSCREDS authorization for sensitive proc object links.

use alloc::sync::Arc;

use linux_raw_sys::general::CAP_SYS_PTRACE;

use super::{Cred, Thread, UserTaskRef};
use crate::{
    StarryError, StarryResult,
    mm::CloneUserRefError,
    namespace::{ROOT_USER_NS, UserNamespace},
    sync::RawSpinLock,
};

type UserNamespaceRef = Arc<RawSpinLock<UserNamespace>>;

/// One thread's credentials and namespace from the common publication gate.
struct ProcReadCredentials {
    cred: Arc<Cred>,
    user_ns: UserNamespaceRef,
}

struct ProcReadTarget {
    credentials: ProcReadCredentials,
    dumpable: i32,
    mm_user_ns: Option<UserNamespaceRef>,
}

/// Checks READ_FSCREDS against the exact, strongly retained target task.
///
/// This may sleep. Callers must release raw locks before entering and retain
/// `target` while acquiring the link object; this result is not a reusable
/// authorization token. Denials are procfs EACCES, not ptrace attach EPERM.
pub(crate) fn authorize_proc_read(current: &UserTaskRef, target: &UserTaskRef) -> StarryResult<()> {
    authorize_thread_read(current.as_thread(), target.as_thread())
}

fn authorize_thread_read(current: &Thread, target: &Thread) -> StarryResult<()> {
    if Arc::ptr_eq(&current.proc_data, &target.proc_data) {
        return Ok(());
    }

    // Never hold two process writer gates: opposite-direction proc reads must
    // not deadlock. Each view is coherent with its own namespace/cred writers.
    let caller = ProcReadCredentials::snapshot(current);
    let target = ProcReadTarget::snapshot(target)?;
    authorize_views(&caller, &target)
}

impl ProcReadCredentials {
    fn snapshot(thread: &Thread) -> Self {
        let update = thread.proc_data.namespace_update();
        Self {
            cred: thread.cred(),
            user_ns: update.snapshot().user_ns.clone(),
        }
    }

    fn has_ptrace_cap(&self, target_ns: &UserNamespaceRef) -> bool {
        // Linux commoncap::cap_capable_helper: identity is the Arc, never the
        // display ID or the mutable mapping-completeness flag.
        let caller_level = self.user_ns.lock_irqsave().lineage().level;
        let mut ns = target_ns.clone();
        loop {
            if Arc::ptr_eq(&ns, &self.user_ns) {
                return self.cred.has_cap(CAP_SYS_PTRACE);
            }
            let lineage = ns.lock_irqsave().lineage();
            if lineage.level <= caller_level {
                return false;
            }
            let Some(parent) = lineage.parent else {
                return false;
            };
            if Arc::ptr_eq(&parent, &self.user_ns) && lineage.owner_uid == self.cred.euid {
                return true;
            }
            ns = parent;
        }
    }
}

impl ProcReadTarget {
    fn snapshot(thread: &Thread) -> StarryResult<Self> {
        let update = thread.proc_data.namespace_update();
        let credentials = ProcReadCredentials {
            cred: thread.cred(),
            user_ns: update.snapshot().user_ns.clone(),
        };
        let mm_user_ns = match thread.proc_data.clone_aspace_user_ref() {
            Ok(owner) => {
                // A kernel pin alone can also pin an MM retained by a different
                // CLONE_VM process after this process detached it. Acquiring an
                // owned reference first validates this exact process owner.
                let mm = owner.pin().map_err(|_| StarryError::PermissionDenied)?;
                let namespace = mm.lock().user_namespace();
                Some(namespace)
            }
            // An exited task can outlive its MM. ProcessData retains the last
            // user-dumpable value, like task->user_dumpable in Linux exit_mm.
            Err(CloneUserRefError::Retired) => None,
            Err(CloneUserRefError::Overflow) => return Err(StarryError::PermissionDenied),
        };
        // The gate and credential mutex acquire pair with credential commit:
        // dumpable is cleared before the new credential becomes observable.
        let dumpable = thread.proc_data.dumpable();
        Ok(Self {
            credentials,
            dumpable,
            mm_user_ns,
        })
    }
}

fn authorize_views(caller: &ProcReadCredentials, target: &ProcReadTarget) -> StarryResult<()> {
    let cred = &caller.cred;
    let target_cred = &target.credentials.cred;
    let target_ns = &target.credentials.user_ns;
    let ptrace_cap = caller.has_ptrace_cap(target_ns);
    let ids_match = cred.fsuid == target_cred.uid
        && cred.fsuid == target_cred.euid
        && cred.fsuid == target_cred.suid
        && cred.fsgid == target_cred.gid
        && cred.fsgid == target_cred.egid
        && cred.fsgid == target_cred.sgid;
    if !ids_match && !ptrace_cap {
        return Err(StarryError::PermissionDenied);
    }

    if target.dumpable != 1 {
        // Linux task_still_dumpable explicitly uses init_user_ns without an
        // MM; a capability in the credential namespace is not a substitute.
        let dump_ns = target.mm_user_ns.as_ref().unwrap_or(&ROOT_USER_NS);
        if !caller.has_ptrace_cap(dump_ns) {
            return Err(StarryError::PermissionDenied);
        }
    }

    // commoncap uses the caller's effective, not permitted set for FSCREDS.
    if (Arc::ptr_eq(&caller.user_ns, target_ns)
        && target_cred.cap_permitted & !cred.cap_effective == 0)
        || ptrace_cap
    {
        Ok(())
    } else {
        Err(StarryError::PermissionDenied)
    }
}

/// Linux cred_cap_issubset for a namespace-only credential transition.
pub(super) fn namespace_preserves_permitted(
    old_ns: &UserNamespaceRef,
    new_ns: &UserNamespaceRef,
    old_euid: u32,
) -> bool {
    if Arc::ptr_eq(old_ns, new_ns) {
        return true;
    }
    let mut ns = new_ns.clone();
    loop {
        let lineage = ns.lock_irqsave().lineage();
        let Some(parent) = lineage.parent else {
            return false;
        };
        if Arc::ptr_eq(old_ns, &parent) && lineage.owner_uid == old_euid {
            return true;
        }
        ns = parent;
    }
}

pub(super) fn credential_resets_dumpable(old: &Cred, new: &Cred) -> bool {
    old.euid != new.euid
        || old.egid != new.egid
        || old.fsuid != new.fsuid
        || old.fsgid != new.fsgid
        || new.cap_permitted & !old.cap_permitted != 0
}

#[cfg(test)]
mod tests {
    use linux_raw_sys::general::CAP_CHOWN;

    use super::*;

    fn credentials(ns: UserNamespaceRef, uid: u32, caps: u64) -> ProcReadCredentials {
        let mut cred = Cred::root();
        cred.uid = uid;
        cred.euid = uid;
        cred.suid = uid;
        cred.fsuid = uid;
        cred.gid = uid;
        cred.egid = uid;
        cred.sgid = uid;
        cred.fsgid = uid;
        cred.cap_permitted = caps;
        cred.cap_effective = caps;
        ProcReadCredentials {
            cred: Arc::new(cred),
            user_ns: ns,
        }
    }

    fn target(ns: UserNamespaceRef) -> ProcReadTarget {
        ProcReadTarget {
            credentials: credentials(ns.clone(), 41, 0),
            dumpable: 1,
            mm_user_ns: Some(ns),
        }
    }

    fn denied(result: StarryResult<()>) {
        assert_eq!(result.unwrap_err().linux_errno(), syscalls::Errno::EACCES);
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn read_fscreds_requires_all_ids_and_effective_capability_subset() {
        let ns = Arc::new(RawSpinLock::new(UserNamespace::new_root()));
        let caller = credentials(ns.clone(), 41, 0);
        assert!(authorize_views(&caller, &target(ns.clone())).is_ok());
        let mismatches: [fn(&mut Cred); 6] = [
            |c| c.uid += 1,
            |c| c.euid += 1,
            |c| c.suid += 1,
            |c| c.gid += 1,
            |c| c.egid += 1,
            |c| c.sgid += 1,
        ];
        for mismatch in mismatches {
            let mut target = target(ns.clone());
            mismatch(Arc::make_mut(&mut target.credentials.cred));
            denied(authorize_views(&caller, &target));
        }
        let mut fs_caller = credentials(ns.clone(), 99, 0);
        let cred = Arc::make_mut(&mut fs_caller.cred);
        cred.fsuid = 41;
        cred.fsgid = 41;
        assert!(authorize_views(&fs_caller, &target(ns.clone())).is_ok());
        let cred = Arc::make_mut(&mut fs_caller.cred);
        cred.fsuid = 99;
        denied(authorize_views(&fs_caller, &target(ns.clone())));
        let cred = Arc::make_mut(&mut fs_caller.cred);
        cred.fsuid = 41;
        cred.fsgid = 99;
        denied(authorize_views(&fs_caller, &target(ns.clone())));
        denied(authorize_views(
            &credentials(ns.clone(), 0, 0),
            &target(ns.clone()),
        ));

        let cap = 1 << CAP_CHOWN;
        let mut target = target(ns.clone());
        Arc::make_mut(&mut target.credentials.cred).cap_permitted = cap;
        denied(authorize_views(&caller, &target));
        let mut caller = credentials(ns, 41, cap);
        Arc::make_mut(&mut caller.cred).cap_effective = 0;
        denied(authorize_views(&caller, &target));
        Arc::make_mut(&mut caller.cred).cap_effective = cap;
        assert!(authorize_views(&caller, &target).is_ok());
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn ptrace_capability_uses_namespace_identity_ancestry_and_parent_owner() {
        let root = Arc::new(RawSpinLock::new(UserNamespace::new_root()));
        let owned = Arc::new(RawSpinLock::new(
            UserNamespace::new_child(root.clone(), 41).unwrap(),
        ));
        let descendant = Arc::new(RawSpinLock::new(
            UserNamespace::new_child(owned.clone(), 99).unwrap(),
        ));
        let sibling = Arc::new(RawSpinLock::new(
            UserNamespace::new_child(root.clone(), 99).unwrap(),
        ));
        let owner = credentials(root.clone(), 41, 0);
        assert!(owner.has_ptrace_cap(&owned));
        assert!(owner.has_ptrace_cap(&descendant));
        assert!(!owner.has_ptrace_cap(&sibling));
        assert!(!owner.has_ptrace_cap(&root));
        let capable = credentials(root.clone(), 99, 1 << CAP_SYS_PTRACE);
        assert!(capable.has_ptrace_cap(&descendant));
        let child_capable = credentials(owned.clone(), 41, 1 << CAP_SYS_PTRACE);
        assert!(!child_capable.has_ptrace_cap(&root));
        assert!(!child_capable.has_ptrace_cap(&sibling));
        // Neither mapped-root status nor a colliding display ID confers caps.
        sibling.lock_irqsave().is_root = true;
        let owned_id = owned.lock_irqsave().id;
        sibling.lock_irqsave().id = owned_id;
        assert!(!child_capable.has_ptrace_cap(&sibling));
        let different_ns = credentials(sibling.clone(), 41, 0);
        denied(authorize_views(&different_ns, &target(owned.clone())));
        assert!(namespace_preserves_permitted(&root, &descendant, 41));
        assert!(!namespace_preserves_permitted(&root, &sibling, 41));
        assert!(!namespace_preserves_permitted(&owned, &root, 41));
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn dumpability_uses_mm_creation_namespace_and_init_namespace_without_mm() {
        let root = ROOT_USER_NS.clone();
        let child = Arc::new(RawSpinLock::new(
            UserNamespace::new_child(root.clone(), 41).unwrap(),
        ));
        let owner = credentials(root.clone(), 41, 0);
        let mut target = target(child.clone());
        target.mm_user_ns = Some(root.clone());
        assert!(authorize_views(&owner, &target).is_ok());
        for dumpable in [0, 2] {
            target.dumpable = dumpable;
            denied(authorize_views(&owner, &target));
            target.mm_user_ns = Some(child.clone());
            assert!(authorize_views(&owner, &target).is_ok());
            target.mm_user_ns = None;
            denied(authorize_views(&owner, &target));
            let capable = credentials(root.clone(), 99, 1 << CAP_SYS_PTRACE);
            assert!(authorize_views(&capable, &target).is_ok());
            target.mm_user_ns = Some(root.clone());
        }
        let child_capable = credentials(child, 41, 1 << CAP_SYS_PTRACE);
        denied(authorize_views(&child_capable, &target));
    }

    #[cfg(axtest)]
    #[axtest::axtest]
    fn credential_publication_and_mm_detach_govern_proc_authorization() {
        use crate::task::{
            PidReservation, PidReservationKind, ROOT_PID_NS, Tgid, Tid, new_test_process_data,
        };

        let make_thread = || {
            let reservation =
                PidReservation::reserve(&ROOT_PID_NS, PidReservationKind::ProcessLeader).unwrap();
            let identity = reservation.identity();
            let tid = identity.acquire_role::<Tid>().unwrap();
            let tgid = identity.acquire_role::<Tgid>().unwrap();
            let process = new_test_process_data(identity.clone(), tgid);
            let cred = credentials(ROOT_USER_NS.clone(), 41, 0).cred;
            let thread = Thread::new(
                identity,
                tid,
                process,
                Some(cred),
                Default::default(),
                scope_local::Scope::new(),
            )
            .unwrap();
            (reservation, thread)
        };
        let (_caller_pid, caller) = make_thread();
        let (_target_pid, target) = make_thread();
        assert!(authorize_thread_read(&caller, &target).is_ok());

        // The caller still matches all six target IDs after only its fsuid
        // changes. Publication itself must make the target nondumpable; relying
        // on a syscall's later cleanup would incorrectly authorize this read.
        let mut changed = (*target.cred()).clone();
        changed.fsuid += 1;
        target.set_thread_cred(changed);
        assert_eq!(target.proc_data.dumpable(), 0);
        denied(authorize_thread_read(&caller, &target));

        let mut changed = (*target.cred()).clone();
        changed.cap_permitted = 1 << CAP_CHOWN;
        target.proc_data.set_dumpable(1);
        target.set_cred(changed);
        assert_eq!(target.proc_data.dumpable(), 0);
        let mut changed = (*target.cred()).clone();
        changed.cap_permitted = 0;
        target.proc_data.set_dumpable(1);
        target.set_thread_cred(changed);
        assert_eq!(target.proc_data.dumpable(), 1);

        let update = target.proc_data.namespace_update();
        let mut changed = (*target.cred()).clone();
        changed.fsgid += 1;
        // This guarded route must not recursively lock the publication gate.
        update.set_process_cred(&target, changed);
        assert_eq!(target.proc_data.dumpable(), 0);
        drop(update);
        assert!(authorize_thread_read(&target, &target).is_ok());
        let sibling_pid =
            PidReservation::reserve(&ROOT_PID_NS, PidReservationKind::Thread).unwrap();
        let sibling_identity = sibling_pid.identity();
        let sibling = Thread::new(
            sibling_identity.clone(),
            sibling_identity.acquire_role::<Tid>().unwrap(),
            target.proc_data.clone(),
            Some(credentials(ROOT_USER_NS.clone(), 99, 0).cred),
            Default::default(),
            scope_local::Scope::new(),
        )
        .unwrap();
        assert!(authorize_thread_read(&sibling, &target).is_ok());

        // A CLONE_VM peer can retain a live MM after this task detached it.
        // A mere successful kernel pin must not select the MM-userns branch.
        let peer_mm = target.proc_data.clone_aspace_user_ref().unwrap();
        target.proc_data.retire_mm_owner();
        assert!(target.proc_data.pin_aspace().is_ok());
        assert!(
            ProcReadTarget::snapshot(&target)
                .unwrap()
                .mm_user_ns
                .is_none()
        );
        denied(authorize_thread_read(&caller, &target));
        target.proc_data.set_dumpable(1);
        assert!(authorize_thread_read(&caller, &target).is_ok());
        drop(peer_mm);
    }
}
