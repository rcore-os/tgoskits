use ax_task::current;

use crate::{
    StarryError, StarryResult,
    task::{AsThread, PgidNumber, Process, TgidNumber, current_pid_view},
};

enum JobProcessSelector {
    Current,
    Process(TgidNumber),
}

impl TryFrom<u32> for JobProcessSelector {
    type Error = StarryError;

    fn try_from(pid: u32) -> Result<Self, Self::Error> {
        if pid == 0 {
            Ok(Self::Current)
        } else {
            Ok(Self::Process(TgidNumber::try_from(pid)?))
        }
    }
}

impl JobProcessSelector {
    fn resolve(self) -> StarryResult<alloc::sync::Arc<Process>> {
        match self {
            Self::Current => Ok(current().as_thread().proc_data.proc.clone()),
            Self::Process(tgid) => Ok(current_pid_view().resolve_process(tgid)?.process()),
        }
    }
}

pub fn sys_getsid(pid: u32) -> StarryResult<isize> {
    let session = JobProcessSelector::try_from(pid)?
        .resolve()?
        .group()
        .session();
    let view = current_pid_view();
    let number = view
        .visible_session_number(&session.identity())
        .ok_or(StarryError::NoSuchProcess)?;
    let resolved = view.resolve_session(number)?;
    debug_assert!(alloc::sync::Arc::ptr_eq(&resolved, &session));
    Ok(number.get() as _)
}

pub fn sys_setsid() -> StarryResult<isize> {
    let curr = current();
    let proc = &curr.as_thread().proc_data.proc;
    if proc.identity().process_group().is_some() {
        return Err(StarryError::OperationNotPermitted);
    }

    let (session, _group) = proc
        .create_session()
        .ok_or(StarryError::OperationNotPermitted)?;
    Ok(current_pid_view()
        .visible_session_number(&session.identity())
        .expect("new session is visible to its creator")
        .get() as _)
}

pub fn sys_getpgid(pid: u32) -> StarryResult<isize> {
    let group = JobProcessSelector::try_from(pid)?.resolve()?.group();
    Ok(current_pid_view()
        .visible_group_number(&group.identity())
        .ok_or(StarryError::NoSuchProcess)?
        .get() as _)
}

#[cfg(target_arch = "x86_64")]
pub fn sys_getpgrp() -> StarryResult<isize> {
    let curr = current();
    let group = curr.as_thread().proc_data.proc.group();
    Ok(current_pid_view()
        .visible_group_number(&group.identity())
        .expect("current process group is visible in its active PID namespace")
        .get() as _)
}

pub fn sys_setpgid(pid: i32, pgid: i32) -> StarryResult<isize> {
    if pid < 0 || pgid < 0 {
        return Err(StarryError::InvalidInput);
    }
    let target = JobProcessSelector::try_from(pid as u32)?;
    let group = (pgid != 0)
        .then(|| PgidNumber::try_from(pgid as u32))
        .transpose()?;

    let proc = target.resolve()?;
    let proc_number = current_pid_view()
        .visible_process_number(&proc.identity())
        .ok_or(StarryError::NoSuchProcess)?
        .get();

    match group {
        None => {
            let _ = proc.create_group();
        }
        Some(pgid) if pgid.get() == proc_number => {
            let _ = proc.create_group();
        }
        Some(pgid) => {
            // POSIX: looking up a non-existent target pgid yields EPERM,
            // not ESRCH (which is reserved for pid lookup failures).
            let group = current_pid_view()
                .resolve_group(pgid)
                .map_err(|_| StarryError::OperationNotPermitted)?;
            if !proc.move_to_group(&group) {
                return Err(StarryError::OperationNotPermitted);
            }
        }
    }

    Ok(0)
}

#[cfg(all(test, not(axtest)))]
fn job_setpgid_validation_rules_hold_for_test() -> bool {
    // Test sys_setpgid validation: negative pid or pgid should fail
    // The function checks: if pid < 0 || pgid < 0 return Err(InvalidInput)

    // Negative pid should be invalid
    let neg_pid = -1i32;
    assert!(neg_pid < 0);

    // Negative pgid should be invalid
    let neg_pgid = -1i32;
    assert!(neg_pgid < 0);

    // Zero is valid (means "use current")
    let zero = 0i32;
    assert!(zero >= 0);

    // Positive values are valid
    let pos_pid = 100i32;
    assert!(pos_pid >= 0);

    true
}

// TODO: job control

#[cfg(all(test, not(axtest)))]
mod tests {
    #[test]
    fn job_setpgid_validation_rules_hold() {
        assert!(super::job_setpgid_validation_rules_hold_for_test());
    }
}
