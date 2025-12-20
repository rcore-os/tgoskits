use core::sync::atomic::{AtomicI32, Ordering};

use starry_core::shm::IpcPerm;

static IPC_ID: AtomicI32 = AtomicI32::new(0);

fn next_ipc_id() -> i32 {
    IPC_ID.fetch_add(1, Ordering::Relaxed)
}

mod msg;
mod shm;
pub use self::{msg::*, shm::*};

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

// Permission bits
const USER_READ: u32 = 0o400;
const USER_WRITE: u32 = 0o200;
const GROUP_READ: u32 = 0o040;
const GROUP_WRITE: u32 = 0o020;
const OTHER_READ: u32 = 0o004;
const OTHER_WRITE: u32 = 0o002;

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
