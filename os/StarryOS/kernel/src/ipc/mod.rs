//! In-kernel IPC objects that are not tied to a single syscall module.
//!
//! POSIX message queues (`mq_*`) live here because the queue object is a
//! [`FileLike`](crate::file::FileLike) fd target shared by the syscall layer
//! (`syscall::ipc`) and the `/dev/mqueue` pseudo filesystem
//! (`pseudofs::mqueue`). Keeping the object, its global name registry and its
//! limits in one place avoids a cyclic dependency between those two consumers.

pub mod mqueue;
