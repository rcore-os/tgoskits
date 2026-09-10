//! In-kernel IPC objects that are not tied to a single syscall module.
//!
//! POSIX message queues (`mq_*`) live here because the queue object is a
//! [`FileLike`](crate::file::FileLike) fd target shared by the syscall layer
//! (`syscall::ipc`) and the `/dev/mqueue` pseudo filesystem
//! (`pseudofs::mqueue`). Keeping the object, its global name registry and its
//! limits in one place avoids a cyclic dependency between those two consumers.

use core::sync::atomic::{AtomicI32, Ordering};

pub mod mqueue;

mod permission;
pub mod shm;

pub use permission::IpcPerm;
pub(crate) use permission::has_ipc_permission;

static IPC_ID: AtomicI32 = AtomicI32::new(0);

pub(crate) fn next_ipc_id() -> i32 {
    IPC_ID.fetch_add(1, Ordering::Relaxed)
}
