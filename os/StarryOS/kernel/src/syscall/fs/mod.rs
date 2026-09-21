mod aio;
mod ctl;
mod event;
mod fd_ops;
mod inotify;
mod io;
mod io_uring;
mod lock;
mod memfd;
mod mount;
mod pidfd;
mod pipe;
mod signalfd;
mod stat;
mod timerfd;
mod xattr;

use axfs_ng_vfs::MutationCredentials;

use crate::task::Cred;

/// Converts a StarryOS task credential into the VFS access snapshot.
pub(crate) fn mutation_credentials(cred: &Cred) -> MutationCredentials<'_> {
    MutationCredentials {
        fsuid: cred.fsuid,
        fsgid: cred.fsgid,
        supplementary_gids: &cred.groups,
        cap_dac_override: cred.has_cap_dac_override(),
        cap_dac_read_search: cred.has_cap_dac_read_search(),
        cap_fowner: cred.has_cap_fowner(),
    }
}

pub use self::{
    aio::*,
    ctl::*,
    event::*,
    fd_ops::*,
    inotify::*,
    io::*,
    io_uring::*,
    lock::{
        release_flock_lock, release_inode_posix_locks, release_pid_flock_locks, release_pid_locks,
        wake_flock_waiters, wake_lock_waiters,
    },
    memfd::*,
    mount::*,
    pidfd::*,
    pipe::*,
    signalfd::*,
    stat::*,
    timerfd::*,
    xattr::*,
};
