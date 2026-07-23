use core::sync::atomic::{AtomicI32, Ordering};

mod mqueue;
mod msg;
mod shm;
use bytemuck::AnyBitPattern;
use linux_raw_sys::{
    ctypes::{c_long, c_ushort},
    general::*,
};

pub use self::{mqueue::*, msg::*, shm::*};

static IPC_ID: AtomicI32 = AtomicI32::new(0);

fn next_ipc_id() -> i32 {
    IPC_ID.fetch_add(1, Ordering::Relaxed)
}

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

// Permission bits
const USER_READ: u32 = 0o400;
const USER_WRITE: u32 = 0o200;
const GROUP_READ: u32 = 0o040;
const GROUP_WRITE: u32 = 0o020;
const OTHER_READ: u32 = 0o004;
const OTHER_WRITE: u32 = 0o002;

/// Data structure used to pass permission information to IPC operations.
#[repr(C)]
#[derive(Clone, Copy, AnyBitPattern)]
pub struct IpcPerm {
    /// Key supplied to msgget(2)
    pub key: __kernel_key_t,
    /// Effective UID of owner
    pub uid: __kernel_uid_t,
    /// Effective GID of owner
    pub gid: __kernel_gid_t,
    /// Effective UID of creator
    pub cuid: __kernel_uid_t,
    /// Effective GID of creator
    pub cgid: __kernel_gid_t,
    /// Permissions (least significant 9 bits define access permissions)
    pub mode: __kernel_mode_t,
    /// Sequence number
    pub seq: c_ushort,
    /// Padding
    pub pad: c_ushort,
    /// Unused field
    pub unused0: c_long,
    /// Unused field
    pub unused1: c_long,
}

// add a helper function to check IPC permissions
fn has_ipc_permission(perm: &IpcPerm, current_uid: u32, current_gid: u32, is_write: bool) -> bool {
    // root user has all permissions
    if current_uid == 0 {
        return true;
    }

    if perm.uid == current_uid {
        (perm.mode & if is_write { USER_WRITE } else { USER_READ }) != 0
    } else if perm.gid == current_gid {
        (perm.mode & if is_write { GROUP_WRITE } else { GROUP_READ }) != 0
    } else {
        (perm.mode & if is_write { OTHER_WRITE } else { OTHER_READ }) != 0
    }
}

#[cfg(axtest)]
pub(crate) fn ipc_permission_and_constants_rules_hold_for_test() -> bool {
    // Test IPC constants
    assert!(IPC_PRIVATE == 0);
    assert!(IPC_CREAT == 0o1000);
    assert!(IPC_EXCL == 0o2000);
    
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
        unused0: 0,
        unused1: 0,
    };
    
    // Owner without write permission
    assert!(has_ipc_permission(&perm_readonly, 1000, 1000, false));
    assert!(!has_ipc_permission(&perm_readonly, 1000, 1000, true));
    
    true
}
