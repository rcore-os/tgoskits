mod mqueue;
mod msg;
mod shm;

pub use self::{mqueue::*, msg::*, shm::*};

// IPC command constants
const IPC_PRIVATE: i32 = 0;
const IPC_CREAT: i32 = 0o1000;
const IPC_EXCL: i32 = 0o2000;
const IPC_RMID: i32 = 0;
const IPC_SET: i32 = 1;
const IPC_STAT: i32 = 2;
const IPC_INFO: i32 = 3;
const MSG_STAT: i32 = 11;
const MSG_INFO: i32 = 12;
const SHM_STAT: i32 = 13;
const SHM_INFO: i32 = 14;

#[cfg(all(test, not(axtest)))]
fn ipc_permission_and_constants_rules_hold_for_test() -> bool {
    use crate::ipc::{IpcPerm, has_ipc_permission};
    const {
        assert!(IPC_PRIVATE == 0);
        assert!(IPC_CREAT == 0o1000);
        assert!(IPC_EXCL == 0o2000);
    }

    // Test has_ipc_permission logic
    let perm = IpcPerm {
        key: 0,
        uid: 1000,
        gid: 1000,
        cuid: 1000,
        cgid: 1000,
        mode: 0o644, // rw-r--r-- (owner has read+write)
        seq: 0,
        pad: 0,
        alignment_pad: 0,
        unused0: 0,
        unused1: 0,
    };

    // Root user should have all permissions
    assert!(has_ipc_permission(&perm, 0, 0, false));
    assert!(has_ipc_permission(&perm, 0, 0, true));

    // Owner with read permission
    assert!(has_ipc_permission(&perm, 1000, 1000, false));

    // Owner with write permission (mode is 0o644, owner has write)
    assert!(has_ipc_permission(&perm, 1000, 1000, true));

    // Other user with read permission
    assert!(has_ipc_permission(&perm, 2000, 2000, false));

    // Other user without write permission (mode is 0o644, other has only read)
    assert!(!has_ipc_permission(&perm, 2000, 2000, true));

    // Test with read-only mode for owner
    let perm_readonly = IpcPerm {
        key: 0,
        uid: 1000,
        gid: 1000,
        cuid: 1000,
        cgid: 1000,
        mode: 0o444, // r--r--r-- (only read)
        seq: 0,
        pad: 0,
        alignment_pad: 0,
        unused0: 0,
        unused1: 0,
    };

    // Owner without write permission
    assert!(has_ipc_permission(&perm_readonly, 1000, 1000, false));
    assert!(!has_ipc_permission(&perm_readonly, 1000, 1000, true));

    true
}

#[cfg(all(test, not(axtest)))]
mod tests {
    #[test]
    fn ipc_permission_and_constants_rules_hold() {
        assert!(super::ipc_permission_and_constants_rules_hold_for_test());
    }
}
