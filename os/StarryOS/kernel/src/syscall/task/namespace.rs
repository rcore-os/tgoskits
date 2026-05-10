use ax_errno::{AxError, AxResult};
use ax_task::current;

use crate::task::{AsThread, Thread};

/// unshare(2) — disassociate parts of the process execution context
///
/// Currently supports:
/// - `CLONE_NEWUSER` — create a new user namespace (credentials become nobody:65534)
/// - `0` — no-op, always succeeds
///
/// Returns EINVAL for unsupported flags.
pub fn sys_unshare(flags: i32) -> AxResult<isize> {
    if flags == 0 {
        return Ok(0);
    }

    const CLONE_NEWUSER: i32 = 0x1000_0000u32 as i32;

    if flags == CLONE_NEWUSER {
        let curr = current();
        let thr = curr.as_thread();

        // In a new user namespace, uid/gid start as 65534 (nobody)
        let mut cred = (*thr.cred()).clone();
        cred.uid = 65534;
        cred.gid = 65534;
        cred.euid = 65534;
        cred.egid = 65534;
        cred.suid = 65534;
        cred.sgid = 65534;
        cred.fsuid = 65534;
        cred.fsgid = 65534;

        Thread::set_cred(thr, cred);

        Ok(0)
    } else {
        // Any other flags (or combinations) are not yet supported
        Err(AxError::InvalidInput)
    }
}
