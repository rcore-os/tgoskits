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
use linux_raw_sys::general::CAP_MKNOD;

use crate::task::FsCredentialsSnapshot;

/// Borrows the paired identity for the current flat, non-idmapped VFS domain.
///
/// Capability exceptions are restricted to the initial user namespace; ordinary
/// owner and group checks still use the saved IDs in every namespace.
pub(crate) fn mutation_credentials(snapshot: &FsCredentialsSnapshot) -> MutationCredentials<'_> {
    let cred = snapshot.credentials();
    let initial_namespace = snapshot.in_initial_user_ns();
    MutationCredentials {
        fsuid: cred.fsuid,
        fsgid: cred.fsgid,
        supplementary_gids: &cred.groups,
        cap_dac_override: initial_namespace && cred.has_cap_dac_override(),
        cap_dac_read_search: initial_namespace && cred.has_cap_dac_read_search(),
        cap_fowner: initial_namespace && cred.has_cap_fowner(),
        cap_mknod: initial_namespace && cred.has_cap(CAP_MKNOD),
    }
}

#[cfg(feature = "qperf-metrics")]
pub(crate) use lock::render_file_lock_metrics;

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
