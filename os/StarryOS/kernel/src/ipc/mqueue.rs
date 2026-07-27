//! POSIX message queue object and its global name registry.
//!
//! Mirrors Linux `ipc/mqueue.c`: a named queue holds messages ordered by
//! descending priority (FIFO within a priority), enforces `mq_maxmsg` /
//! `mq_msgsize`, supports a single asynchronous notification registration and
//! is reference counted so it survives `mq_unlink` while descriptors remain
//! open. The queue itself is exposed to userspace through a message-queue
//! descriptor (`mqd_t`), which on Linux is an fd; here it is a
//! [`FileLike`] entry in the fd table.

use alloc::{
    borrow::Cow,
    collections::{BTreeMap, VecDeque},
    string::String,
    sync::Arc,
    vec::Vec,
};
use core::{
    sync::atomic::{AtomicU32, AtomicUsize, Ordering},
    task::Context,
    time::Duration,
};

use ax_errno::{AxError, AxResult, LinuxError};
use ax_kspin::SpinNoIrq;
use ax_runtime::hal::time::wall_time;
use ax_sync::Mutex;
use ax_task::future::{block_on, poll_io, timeout_at_wall};
use axpoll::{IoEvents, PollSet, Pollable};
use linux_raw_sys::general::{
    O_ACCMODE, O_NONBLOCK, O_RDONLY, O_RDWR, O_WRONLY, S_IFREG, SIGEV_NONE, SIGEV_SIGNAL,
    SIGEV_THREAD,
};
use starry_process::Pid;
use starry_signal::{SignalInfo, Signo};

use crate::{
    file::{FileLike, IoDst, IoSrc, Kstat},
    task::{AsThread, send_signal_to_process},
};

/// Hard ceiling for `mq_maxmsg` a privileged (`CAP_SYS_RESOURCE`) caller may
/// request, matching Linux `HARD_MSGMAX` (65536).
pub const MQ_HARD_MSG_MAX: usize = 65536;
/// Hard ceiling for `mq_msgsize` a privileged caller may request, matching
/// Linux `HARD_MSGSIZEMAX` (16 MiB).
pub const MQ_HARD_MSGSIZE_MAX: usize = 16 * 1024 * 1024;
/// Lower bound Linux enforces on the writable `msg_max` sysctl
/// (`MIN_MSGMAX`, `include/linux/ipc_namespace.h`).
pub const MQ_MIN_MSG_MAX: usize = 1;
/// Lower bound Linux enforces on the writable `msgsize_max` sysctl
/// (`MIN_MSGSIZEMAX`, 128).
pub const MQ_MIN_MSGSIZE_MAX: usize = 128;

/// The five `/proc/sys/fs/mqueue/*` tunables, held as live atomics so they can
/// be read back through procfs and, when written, take effect on the next
/// `mq_open`. Linux keeps these per-`ipc_namespace` (`mq_msg_max`, ...); with a
/// single ipc namespace they collapse to one global set seeded from
/// `mq_init_ns` (ipc/mqueue.c:1625) with `DFLT_*` (ipc_namespace.h).
///
/// `queues_max` (`DFLT_QUEUESMAX`), the system-wide queue count ceiling.
pub static MQ_QUEUES_MAX: AtomicUsize = AtomicUsize::new(256);
/// `msg_max` (`DFLT_MSGMAX`), unprivileged `mq_maxmsg` ceiling.
pub static MQ_MSG_MAX: AtomicUsize = AtomicUsize::new(10);
/// `msgsize_max` (`DFLT_MSGSIZEMAX`), unprivileged `mq_msgsize` ceiling.
pub static MQ_MSGSIZE_MAX: AtomicUsize = AtomicUsize::new(8192);
/// `msg_default` (`DFLT_MSG`), `mq_maxmsg` for an attr-less create.
pub static MQ_MSG_DEFAULT: AtomicUsize = AtomicUsize::new(10);
/// `msgsize_default` (`DFLT_MSGSIZE`), `mq_msgsize` for an attr-less create.
pub static MQ_MSGSIZE_DEFAULT: AtomicUsize = AtomicUsize::new(8192);

/// Size of Linux `struct msg_msg` on a 64-bit kernel (two-pointer `m_list` +
/// `long m_type` + `size_t m_ts` + two pointers), used verbatim in the
/// `mq_treesize` accounting (ipc/mqueue.c:364). All StarryOS targets are 64-bit.
const SIZEOF_MSG_MSG: u64 = 48;
/// Size of Linux `struct posix_msg_tree_node` on a 64-bit kernel
/// (`rb_node` 24 + `list_head` 16 + `int priority` padded to 8), used in
/// `mq_treesize` (ipc/mqueue.c:60,365).
const SIZEOF_POSIX_MSG_TREE_NODE: u64 = 48;

/// `mq_maxmsg` ceiling honored at create time: the (possibly sysctl-tuned)
/// unprivileged limit, or the hard limit for a `CAP_SYS_RESOURCE` caller.
pub fn msg_max(privileged: bool) -> usize {
    if privileged {
        MQ_HARD_MSG_MAX
    } else {
        MQ_MSG_MAX.load(Ordering::Relaxed)
    }
}

/// `mq_msgsize` ceiling honored at create time (see [`msg_max`]).
pub fn msgsize_max(privileged: bool) -> usize {
    if privileged {
        MQ_HARD_MSGSIZE_MAX
    } else {
        MQ_MSGSIZE_MAX.load(Ordering::Relaxed)
    }
}

/// Default `mq_maxmsg` for an attr-less create. Linux uses
/// `min(mq_msg_max, mq_msg_default)` (ipc/mqueue.c:325) so a lowered `msg_max`
/// clamps the default too.
pub fn msg_default() -> usize {
    MQ_MSG_MAX
        .load(Ordering::Relaxed)
        .min(MQ_MSG_DEFAULT.load(Ordering::Relaxed))
}

/// Default `mq_msgsize` for an attr-less create (see [`msg_default`]).
pub fn msgsize_default() -> usize {
    MQ_MSGSIZE_MAX
        .load(Ordering::Relaxed)
        .min(MQ_MSGSIZE_DEFAULT.load(Ordering::Relaxed))
}

/// System-wide queue-count ceiling (`queues_max`).
pub fn queues_max() -> usize {
    MQ_QUEUES_MAX.load(Ordering::Relaxed)
}

/// Live queue count, mirroring Linux `ipc_ns->mq_queues_count`. Linux bumps it
/// per *inode* in `mqueue_create_attr` and drops it in `mqueue_evict_inode`
/// (ipc/mqueue.c:586,557) - i.e. tied to the queue object's lifetime, not to
/// its name. A queue that is `mq_unlink`ed while a descriptor is still open
/// keeps its inode alive, so it must keep counting against `queues_max` until
/// the last reference drops. Tracking it here (bumped in [`MessageQueue::new`],
/// dropped in `Drop`) rather than by `registry.len()` reproduces that.
static MQ_QUEUES_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Current live queue count (`ipc_ns->mq_queues_count`).
pub fn queues_count() -> usize {
    MQ_QUEUES_COUNT.load(Ordering::Relaxed)
}

/// Total bytes a queue with these attributes charges against the creating
/// user's `RLIMIT_MSGQUEUE`, computed exactly as Linux does in
/// `mqueue_get_inode` (ipc/mqueue.c:364-371):
///
/// ```text
/// mq_treesize = maxmsg * sizeof(msg_msg)
///             + min(maxmsg, MQ_PRIO_MAX) * sizeof(posix_msg_tree_node)
/// mq_bytes    = mq_treesize + maxmsg * msgsize
/// ```
///
/// Returns `None` on `u64` overflow, mirroring Linux's `mq_bytes + mq_treesize
/// < mq_bytes` wrap check.
fn mq_bytes(max_msg: usize, msg_size: usize) -> Option<u64> {
    let max_msg = max_msg as u64;
    let msg_size = msg_size as u64;
    let tree_nodes = max_msg.min(MQ_PRIO_MAX as u64);
    let tree = max_msg
        .checked_mul(SIZEOF_MSG_MSG)?
        .checked_add(tree_nodes.checked_mul(SIZEOF_POSIX_MSG_TREE_NODE)?)?;
    max_msg.checked_mul(msg_size)?.checked_add(tree)
}

/// Per-user `RLIMIT_MSGQUEUE` accounting: `fsuid -> bytes` currently charged
/// across all of that user's live queues. Linux keeps this in the per-user
/// `ucounts` (`inc_rlimit_ucounts(UCOUNT_RLIMIT_MSGQUEUE, ...)`); with no
/// ucounts abstraction here, a global uid-keyed map is the faithful
/// equivalent. Entries are removed when a user's charge returns to zero.
static MQ_USER_BYTES: Mutex<BTreeMap<u32, u64>> = Mutex::new(BTreeMap::new());

/// Charge `bytes` for `uid` against `limit` (the caller's `RLIMIT_MSGQUEUE`
/// soft limit). Returns `EMFILE` without mutating state when the charge would
/// exceed the limit or overflow, mirroring `mqueue_get_inode`
/// (ipc/mqueue.c:373-381): the increment is rolled back and `-EMFILE` returned.
fn charge_user_bytes(uid: u32, bytes: u64, limit: u64) -> AxResult<()> {
    let mut map = MQ_USER_BYTES.lock();
    let cur = map.get(&uid).copied().unwrap_or(0);
    let next = cur.checked_add(bytes).ok_or(LinuxError::EMFILE)?;
    // Linux fails when the post-increment total hits LONG_MAX or exceeds the
    // rlimit; here `next > limit` covers the rlimit test and the checked_add
    // above covers the wrap.
    if next > limit {
        return Err(LinuxError::EMFILE.into());
    }
    map.insert(uid, next);
    Ok(())
}

/// Refund `bytes` for `uid` when a queue is destroyed (ipc/mqueue.c:549,
/// `dec_rlimit_ucounts`).
fn refund_user_bytes(uid: u32, bytes: u64) {
    let mut map = MQ_USER_BYTES.lock();
    if let Some(cur) = map.get_mut(&uid) {
        *cur = cur.saturating_sub(bytes);
        if *cur == 0 {
            map.remove(&uid);
        }
    }
}

/// Reserve the `RLIMIT_MSGQUEUE` charge for a queue of `(max_msg, msg_size)`
/// created by `uid`, whose soft `RLIMIT_MSGQUEUE` is `limit`. On success
/// returns the `mq_bytes` reserved (to be handed to [`MessageQueue::new`] and
/// refunded on drop). Returns `EMFILE` when the charge would exceed the limit
/// or overflow, exactly as Linux `mqueue_get_inode` does before allocating the
/// inode (ipc/mqueue.c:367-381).
pub fn charge_open_bytes(uid: u32, limit: u64, max_msg: usize, msg_size: usize) -> AxResult<u64> {
    let bytes = mq_bytes(max_msg, msg_size).ok_or(LinuxError::EMFILE)?;
    charge_user_bytes(uid, bytes, limit)?;
    Ok(bytes)
}

/// Number of distinct priority levels: valid priorities are `0..MQ_PRIO_MAX`,
/// matching the Linux/`sysconf(_SC_MQ_PRIO_MAX)` value (32768).
pub const MQ_PRIO_MAX: u32 = 32768;
/// Highest signal number `mq_notify(SIGEV_SIGNAL)` accepts, matching Linux
/// `valid_signal()` (`sig <= _NSIG`, and `_NSIG` is 64). Signal `0` is also
/// accepted (registers, never delivers).
pub const MQ_NSIG: u32 = 64;
/// Length of the `mq_notify(SIGEV_THREAD)` cookie exchanged over the netlink
/// socket, matching Linux `NOTIFY_COOKIE_LEN` (`include/uapi/linux/mqueue.h`).
pub const NOTIFY_COOKIE_LEN: usize = 32;
/// Cookie status byte written into the last cookie octet when a queued message
/// triggers the notification (`NOTIFY_WOKENUP`).
const NOTIFY_WOKENUP: u8 = 1;
/// Cookie status byte written when the registration is torn down before firing
/// (`NOTIFY_REMOVED`), so the helper thread stops.
const NOTIFY_REMOVED: u8 = 2;
/// Longest permitted queue name including the leading `/`, matching Linux
/// (`NAME_MAX` + 1 for the slash → 255 usable chars after the slash).
pub const MQ_NAME_MAX: usize = 255;
/// The mqueuefs inode `i_size`, matching Linux `FILENT_SIZE` (ipc/mqueue.c:52):
/// the fixed 80-byte width of the `QSIZE:...NOTIFY_PID:...` status line, which
/// `mqueue_get_inode` stores as `inode->i_size`.
const FILENT_SIZE: u64 = 80;

/// `struct mq_attr` as seen by userspace. Every field is a `long`, so the
/// layout is identical on all supported 64-bit architectures.
#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::AnyBitPattern, bytemuck::NoUninit)]
pub struct MqAttr {
    /// Queue flags. Only `O_NONBLOCK` is meaningful.
    pub mq_flags: i64,
    /// Maximum number of messages the queue may hold.
    pub mq_maxmsg: i64,
    /// Maximum size in bytes of a single message.
    pub mq_msgsize: i64,
    /// Number of messages currently queued.
    pub mq_curmsgs: i64,
    /// Reserved, always zero.
    pub __reserved: [i64; 4],
}

/// A single queued message: its payload and the priority it was sent with.
struct Message {
    priority: u32,
    data: Vec<u8>,
}

/// A receiver parked in `mq_timedreceive`, mirroring Linux `struct
/// ext_wait_queue` (ipc/mqueue.c:126). Linux keeps one per blocked task on the
/// `info->e_wait_q[RECV]` list; a sender hands a message straight into the
/// chosen waiter's `msg` slot and flips its state to `STATE_READY`
/// (`pipelined_send` -> `__pipelined_op`, ipc/mqueue.c:993,1010), so the queue
/// itself stays empty and no message is enqueued.
///
/// The handoff slot is a `SpinNoIrq<Option<Message>>` because the shared `Arc`
/// is touched by two parties - the sender that fills it and the receiver that
/// drains it - but *always* while the queue's [`Inner`] lock is held, so it is
/// an uncontended leaf lock (acquired and released without ever sleeping),
/// standing in for Linux's `smp_store_release(&this->state, STATE_READY)`
/// publish of `this->msg`. `Some` here is exactly `STATE_READY`; `None` is
/// `STATE_NONE`.
struct RecvWaiter {
    /// The directly-handed message (Linux `ext_wait_queue::msg`). `Some` means
    /// the waiter has been served and must consume this instead of the queue.
    msg: SpinNoIrq<Option<Message>>,
}

impl RecvWaiter {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            msg: SpinNoIrq::new(None),
        })
    }

    /// Take a directly-handed message if one was published (STATE_READY).
    fn take_handed(&self) -> Option<Message> {
        self.msg.lock().take()
    }
}

/// A registered `mq_notify` target.
///
/// Linux delivers exactly one notification when the queue transitions from
/// empty to non-empty, then clears the registration. `SIGEV_SIGNAL` raises a
/// signal, `SIGEV_THREAD` pushes a cookie over a netlink socket to wake the
/// libc helper thread, and `SIGEV_NONE` registers ownership without any
/// delivery (still consuming the single slot, still cleared on delivery).
struct Notification {
    /// `SIGEV_SIGNAL`, `SIGEV_THREAD` or `SIGEV_NONE`.
    notify: u32,
    /// Signal to deliver for `SIGEV_SIGNAL`.
    signo: u32,
    /// Process that owns the registration and receives the signal.
    pid: Pid,
    /// The `sigev_value` the registrant passed to `mq_notify`, delivered as
    /// `si_value` in the `SI_MESGQ` signal. Linux stores this in
    /// `info->notify.sigev_value` (ipc/mqueue.c) and copies it to `sig_i.si_value`
    /// in `__do_notify`; a `SIGEV_SIGNAL` receiver reads it via `SA_SIGINFO`.
    sigev_value: i64,
    /// For `SIGEV_THREAD`: the netlink socket the cookie is delivered on
    /// (`info->notify_sock`) and the cookie bytes (`info->notify_cookie`).
    /// Linux resolves the socket from `sigev_signo` (the netlink fd) at
    /// registration and copies `NOTIFY_COOKIE_LEN` bytes from
    /// `sigev_value.sival_ptr` (ipc/mqueue.c:1287-1351).
    thread: Option<ThreadNotify>,
}

/// The `SIGEV_THREAD` delivery state: the registrant's netlink socket and the
/// cookie buffer copied in at registration time.
struct ThreadNotify {
    sock: Arc<crate::file::netlink::NetlinkSocket>,
    cookie: [u8; NOTIFY_COOKIE_LEN],
}

/// A decoded `mq_notify` request handed to [`MessageQueue::register_notify`],
/// after the syscall layer has validated the `sigevent` and (for
/// `SIGEV_THREAD`) resolved the netlink socket and copied the cookie in.
pub enum NotifyRequest {
    /// `mq_notify(NULL)`: clear the caller's registration.
    Unregister,
    /// `SIGEV_SIGNAL`: deliver `signo` with `sigev_value` as `si_value`.
    Signal { signo: u32, sigev_value: i64 },
    /// `SIGEV_NONE`: register ownership, deliver nothing.
    None,
    /// `SIGEV_THREAD`: push `cookie` over `sock` on message arrival.
    Thread {
        sock: Arc<crate::file::netlink::NetlinkSocket>,
        cookie: [u8; NOTIFY_COOKIE_LEN],
    },
}

/// Mutable state of a queue, guarded by a single lock.
struct Inner {
    /// Priority buckets. `BTreeMap` keeps priorities ordered; iterating in
    /// reverse yields the highest priority first, and each `VecDeque`
    /// preserves send order (FIFO) within a priority.
    buckets: BTreeMap<u32, VecDeque<Message>>,
    /// Number of messages currently queued (sum of bucket lengths).
    len: usize,
    /// Maximum messages (`mq_maxmsg`), fixed at creation.
    max_msg: usize,
    /// Maximum single-message size (`mq_msgsize`), fixed at creation.
    msg_size: usize,
    /// Pending single-shot notification registration, if any.
    notify: Option<Notification>,
    /// Receivers currently parked in `mq_timedreceive` on an empty queue, in
    /// the order they should be served, mirroring Linux's `info->e_wait_q[RECV]`
    /// list (ipc/mqueue.c:152). A sender with a free slot hands its message
    /// straight to the front waiter (`pipelined_send`) instead of enqueuing it,
    /// so the queue stays empty and the empty -> non-empty notification does not
    /// fire - exactly Linux's rule that `__do_notify` runs only when there is no
    /// waiting receiver (`do_mq_timedsend`, ipc/mqueue.c:1121-1130).
    ///
    /// This *is* the wakeup handoff (not a mere count): the message is moved
    /// into the chosen waiter's private slot and that waiter is removed from the
    /// list under this lock, so a second send that lands before the receiver is
    /// rescheduled finds an empty queue with no waiter and correctly fires the
    /// notification - the case a bare "receiver blocked" count silently lost by
    /// treating the still-queued first message as suppressing the edge.
    ///
    /// Linux orders the list by task priority (`wq_add`, ipc/mqueue.c:689);
    /// StarryOS has no thread-priority model for its default schedulers, so
    /// insertion order (FIFO) is the faithful degenerate case. Correctness here
    /// is the notification/handoff semantics, not priority ordering.
    recv_waiters: VecDeque<Arc<RecvWaiter>>,
    /// Last-access / last-status-change / last-modification wall-clock times of
    /// the mqueuefs inode. Linux stamps all three at creation
    /// (`simple_inode_init_ts`) and bumps atime+ctime on the mqueuefs file
    /// operations (`inode_set_atime_to_ts(inode, inode_set_ctime_current())`
    /// at ipc/mqueue.c:654,1340,1367,1420).
    atime: Duration,
    ctime: Duration,
    mtime: Duration,
}

/// A POSIX message queue.
///
/// Shared behind an `Arc` by every descriptor that opened the same name and by
/// the name registry. `PollSet`s drive the blocking `mq_timedsend` /
/// `mq_timedreceive` paths the same way `EventFd` does.
pub struct MessageQueue {
    inner: Mutex<Inner>,
    /// Owner uid, captured from `current_fsuid` at creation and checked against
    /// the caller on a later `mq_open`. Linux keeps it in the mqueue inode.
    uid: u32,
    /// Owner gid, captured from `current_fsgid` at creation.
    gid: u32,
    /// Permission bits (`mode & ~umask & 0777`), checked on open the way Linux
    /// checks the mqueue inode `i_mode` via `inode_permission`.
    mode: u16,
    /// Bytes this queue charged against `uid`'s `RLIMIT_MSGQUEUE` at creation,
    /// refunded on destroy (`Drop`). Fixed for the queue's lifetime, mirroring
    /// Linux's `mq_bytes` recomputed identically in `mqueue_evict_inode`.
    charged_bytes: u64,
    /// Woken when a slot frees up (a message was received): unblocks senders.
    poll_send: PollSet,
    /// Woken when a message arrives: unblocks receivers.
    poll_recv: PollSet,
}

impl MessageQueue {
    /// Create a queue with the given size attributes and owner/permission
    /// metadata (`mode` already masked with the caller's umask). `charged_bytes`
    /// is the `mq_bytes` amount already reserved against the creator's
    /// `RLIMIT_MSGQUEUE`; it is refunded when the queue is dropped.
    pub fn new(
        max_msg: usize,
        msg_size: usize,
        mode: u16,
        uid: u32,
        gid: u32,
        charged_bytes: u64,
    ) -> Arc<Self> {
        // Linux bumps `mq_queues_count` when the inode is created
        // (ipc/mqueue.c:586); the matching decrement is in `Drop`.
        MQ_QUEUES_COUNT.fetch_add(1, Ordering::Relaxed);
        let now = wall_time();
        Arc::new(Self {
            inner: Mutex::new(Inner {
                buckets: BTreeMap::new(),
                len: 0,
                max_msg,
                msg_size,
                notify: None,
                recv_waiters: VecDeque::new(),
                atime: now,
                ctime: now,
                mtime: now,
            }),
            uid,
            gid,
            mode: mode & 0o777,
            charged_bytes,
            poll_send: PollSet::new(),
            poll_recv: PollSet::new(),
        })
    }

    /// Permission check for `mq_open` on an existing queue, mirroring Linux
    /// `inode_permission` -> `generic_permission` -> `acl_permission_check`
    /// (fs/namei.c): `O_RDONLY`/`O_RDWR` need read, `O_WRONLY`/`O_RDWR` need
    /// write. `CAP_DAC_OVERRIDE` is applied by the caller; here we resolve the
    /// owner/group/other class from the caller's fsuid/fsgid and test the
    /// requested bits. Returns `EACCES` when the class lacks a required bit.
    ///
    /// `is_group_member(gid)` reports whether the caller's fsgid *or any of its
    /// supplementary groups* matches `gid`, matching Linux's group tier which
    /// gates on `vfsgid_in_group_p` -> `in_group_p` (kernel/groups.c): the
    /// membership test consults `cred->group_info`, not just `fsgid`.
    pub fn check_open_access(
        &self,
        access_mode: u32,
        fsuid: u32,
        is_group_member: impl Fn(u32) -> bool,
    ) -> AxResult<()> {
        // Select the permission triad: owner (fsuid == queue uid), then group
        // (fsgid or a supplementary group == queue gid), else other. Linux
        // walks the same order in `acl_permission_check`.
        let shift = if fsuid == self.uid {
            6
        } else if is_group_member(self.gid) {
            3
        } else {
            0
        };
        let granted = ((self.mode >> shift) & 0o7) as u32;
        // Read = 0o4, write = 0o2 in the granted triad.
        let need_read = access_mode == O_RDONLY || access_mode == O_RDWR;
        let need_write = access_mode == O_WRONLY || access_mode == O_RDWR;
        if (need_read && granted & 0o4 == 0) || (need_write && granted & 0o2 == 0) {
            return Err(LinuxError::EACCES.into());
        }
        Ok(())
    }

    /// Current attributes for `mq_getattr`. `mq_flags` is per-descriptor on
    /// Linux, so it is left zero here and filled in by the syscall layer from
    /// the descriptor; only the queue-wide size/count fields are reported.
    pub fn attr(&self) -> MqAttr {
        let inner = self.inner.lock();
        MqAttr {
            mq_flags: 0,
            mq_maxmsg: inner.max_msg as i64,
            mq_msgsize: inner.msg_size as i64,
            mq_curmsgs: inner.len as i64,
            __reserved: [0; 4],
        }
    }

    /// Send a message. Blocks while full unless `O_NONBLOCK`, honoring the
    /// optional absolute `CLOCK_REALTIME` deadline.
    ///
    /// `priority >= MQ_PRIO_MAX` and oversize payloads are rejected up front,
    /// matching `mq_timedsend` `EINVAL`/`EMSGSIZE`.
    pub fn send(
        &self,
        data: &[u8],
        priority: u32,
        deadline: Option<core::time::Duration>,
        non_blocking: bool,
    ) -> AxResult<()> {
        if priority >= MQ_PRIO_MAX {
            return Err(LinuxError::EINVAL.into());
        }
        {
            let inner = self.inner.lock();
            if data.len() > inner.msg_size {
                return Err(LinuxError::EMSGSIZE.into());
            }
        }

        let op = || {
            let mut inner = self.inner.lock();
            if inner.len >= inner.max_msg {
                return Err(AxError::WouldBlock);
            }
            let msg = Message {
                priority,
                data: data.to_vec(),
            };
            // Linux `do_mq_timedsend` (ipc/mqueue.c:1121-1130): with a free slot,
            // if a receiver is parked, hand the message straight to it
            // (`pipelined_send`) - the queue stays empty and `__do_notify` does
            // NOT run. Only with no waiting receiver is the message enqueued and,
            // on the empty -> non-empty edge, the notification fired.
            let fired = if let Some(waiter) = inner.recv_waiters.pop_front() {
                // Direct handoff: publish the message into the chosen waiter's
                // slot (STATE_READY) and remove it from the wait list, mirroring
                // `pipelined_send` -> `__pipelined_op`'s `receiver->msg = message`
                // + `list_del`. No enqueue, no notification: a receiver took it.
                *waiter.msg.lock() = Some(msg);
                None
            } else {
                // No waiter: enqueue. Linux fires the notification only when this
                // send made the queue non-empty (`__do_notify` gates on
                // `mq_curmsgs == 1`, ipc/mqueue.c:786), i.e. the empty -> non-empty
                // edge, and clears the single-shot registration.
                let was_empty = inner.len == 0;
                inner.buckets.entry(priority).or_default().push_back(msg);
                inner.len += 1;
                was_empty.then(|| inner.notify.take()).flatten()
            };
            drop(inner);
            if let Some(n) = fired {
                deliver_notification(&n);
            }
            // The message (queued, or published into a waiter's slot) is visible
            // before receivers are woken. Wake-all is safe: the served waiter
            // takes its handed message; every other receiver re-checks, finds
            // nothing for it, and re-parks (see `receive`). This coarse `PollSet`
            // wake plus the per-waiter slot achieve Linux's targeted `wake_q`.
            unsafe { self.poll_recv.wake(IoEvents::IN) };
            Ok(())
        };

        block_on(timeout_at_wall(
            deadline,
            poll_io(self, IoEvents::OUT, non_blocking, op),
        ))
        .map_err(|_| AxError::from(LinuxError::ETIMEDOUT))?
    }

    /// Receive the highest-priority, earliest message. Blocks while empty
    /// unless `O_NONBLOCK`, honoring the optional absolute deadline. Returns
    /// the payload and the priority it was sent with.
    ///
    /// `max_len` is the caller buffer size; a queue whose `mq_msgsize` exceeds
    /// it yields `EMSGSIZE` (checked before blocking, as Linux does).
    pub fn receive(
        &self,
        max_len: usize,
        deadline: Option<core::time::Duration>,
        non_blocking: bool,
    ) -> AxResult<(Vec<u8>, u32)> {
        {
            let inner = self.inner.lock();
            if max_len < inner.msg_size {
                return Err(LinuxError::EMSGSIZE.into());
            }
        }

        // This receiver's private handoff slot (Linux's on-stack `wait.msg`).
        // A sender may publish a message straight into it (`pipelined_send`)
        // instead of enqueuing; the receiver then consumes that, skipping the
        // queue. Shared with `Inner::recv_waiters` while parked.
        let waiter = RecvWaiter::new();

        let op = || {
            let mut inner = self.inner.lock();
            // A sender may have handed this receiver a message directly while it
            // was parked (STATE_READY): consume that first, ahead of the queue,
            // matching `do_mq_timedreceive`'s `msg_ptr = wait.msg` after
            // `wq_sleep` returns (ipc/mqueue.c:1205-1206). No slot frees up (the
            // message was never enqueued), so no sender is woken.
            if let Some(msg) = waiter.take_handed() {
                return Ok((msg.data, msg.priority));
            }
            match inner.pop_highest() {
                Some(msg) => {
                    drop(inner);
                    // A slot freed up: wake a blocked sender (Linux
                    // `pipelined_receive`, ipc/mqueue.c:1216).
                    unsafe { self.poll_send.wake(IoEvents::OUT) };
                    Ok((msg.data, msg.priority))
                }
                None => {
                    // Empty queue: enroll as a waiter so a concurrent send hands
                    // its message straight here and suppresses the notification
                    // (`wq_add`, ipc/mqueue.c:715). Idempotent - `op` re-runs on
                    // every wake - so enroll only if not already listed.
                    if !inner.recv_waiters.iter().any(|w| Arc::ptr_eq(w, &waiter)) {
                        inner.recv_waiters.push_back(waiter.clone());
                    }
                    Err(AxError::WouldBlock)
                }
            }
        };

        // Flatten the outer timeout error (`Elapsed` -> ETIMEDOUT) and the inner
        // `poll_io` result (Ok, or Err(Interrupted)=EINTR, or Err(WouldBlock)=
        // EAGAIN for O_NONBLOCK) into one result.
        let result = match block_on(timeout_at_wall(
            deadline,
            poll_io(self, IoEvents::IN, non_blocking, op),
        )) {
            Ok(inner_result) => inner_result,
            Err(_) => Err(AxError::from(LinuxError::ETIMEDOUT)),
        };

        match result {
            Ok(msg) => Ok(msg),
            Err(err) => {
                // Unwinding via timeout, signal (EINTR) or O_NONBLOCK (EAGAIN).
                // Linux's `wq_sleep` re-checks `state == STATE_READY` under the
                // lock before giving up (ipc/mqueue.c:733), so a message handed to
                // this waiter in the race window between "decided to leave" and
                // "removed from the list" is not lost. Do the same: claim a handed
                // message if one arrived, otherwise `list_del` ourselves. This
                // runs on the EINTR path too, where the sender may have served
                // the waiter concurrently with the interrupt.
                let mut inner = self.inner.lock();
                if let Some(msg) = waiter.take_handed() {
                    drop(inner);
                    return Ok((msg.data, msg.priority));
                }
                inner.recv_waiters.retain(|w| !Arc::ptr_eq(w, &waiter));
                Err(err)
            }
        }
    }

    /// Register (or clear) the single-shot `mq_notify` target.
    ///
    /// [`NotifyRequest::Unregister`] unregisters; the `SIGEV_*` variants
    /// register and return `EBUSY` if the slot is already taken. Registering on
    /// an already non-empty queue is allowed; Linux only fires on the empty →
    /// non-empty edge, so it simply waits for the next such transition.
    pub fn register_notify(&self, req: NotifyRequest, pid: Pid) -> AxResult<()> {
        let mut inner = self.inner.lock();
        match req {
            NotifyRequest::Unregister => {
                // Linux `do_mq_notify`: `mq_notify(NULL)` only removes the
                // registration when the caller owns it (`notify_owner ==
                // task_tgid(current)`); otherwise it is a silent no-op. A real
                // removal bumps atime+ctime (ipc/mqueue.c:1339) and, for a
                // `SIGEV_THREAD` registration, sends NOTIFY_REMOVED so the libc
                // helper thread exits (`remove_notification`, ipc/mqueue.c:850).
                if inner.notify.as_ref().is_some_and(|n| n.pid == pid) {
                    let removed = inner.notify.take();
                    let now = wall_time();
                    inner.atime = now;
                    inner.ctime = now;
                    drop(inner);
                    if let Some(n) = removed {
                        notify_thread_teardown(&n);
                    }
                }
                Ok(())
            }
            NotifyRequest::Signal { signo, sigev_value } => {
                if inner.notify.is_some() {
                    return Err(LinuxError::EBUSY.into());
                }
                inner.notify = Some(Notification {
                    notify: SIGEV_SIGNAL,
                    signo,
                    pid,
                    sigev_value,
                    thread: None,
                });
                Self::stamp_register(&mut inner);
                Ok(())
            }
            NotifyRequest::None => {
                if inner.notify.is_some() {
                    return Err(LinuxError::EBUSY.into());
                }
                inner.notify = Some(Notification {
                    notify: SIGEV_NONE,
                    signo: 0,
                    pid,
                    sigev_value: 0,
                    thread: None,
                });
                Self::stamp_register(&mut inner);
                Ok(())
            }
            NotifyRequest::Thread { sock, cookie } => {
                if inner.notify.is_some() {
                    return Err(LinuxError::EBUSY.into());
                }
                inner.notify = Some(Notification {
                    notify: SIGEV_THREAD,
                    signo: 0,
                    pid,
                    sigev_value: 0,
                    thread: Some(ThreadNotify { sock, cookie }),
                });
                Self::stamp_register(&mut inner);
                Ok(())
            }
        }
    }

    /// Stamp atime+ctime on a successful registration (ipc/mqueue.c:1367).
    fn stamp_register(inner: &mut Inner) {
        let now = wall_time();
        inner.atime = now;
        inner.ctime = now;
    }

    /// Drop the `mq_notify` registration if it is owned by `pid`. Linux clears
    /// it in `mqueue_flush_file` -> `remove_notification` (ipc/mqueue.c:658),
    /// which `filp_flush` runs on *every* fd-closing path: explicit `close`,
    /// `close_range`, `dup2`/`dup3` replacement, exec CLOEXEC and process exit.
    /// StarryOS routes all of those through `release_locks_on_close`, which
    /// calls this via the `FileLike::on_close` hook, so the coverage matches.
    /// A `SIGEV_THREAD` registration also gets a `NOTIFY_REMOVED` cookie so its
    /// helper thread exits.
    pub fn clear_notify_owner(&self, pid: Pid) {
        let mut inner = self.inner.lock();
        if inner.notify.as_ref().is_some_and(|n| n.pid == pid) {
            let removed = inner.notify.take();
            drop(inner);
            if let Some(n) = removed {
                notify_thread_teardown(&n);
            }
        }
    }

    /// Snapshot for `/dev/mqueue/<name>` reporting: `(qsize, notify, signo,
    /// notify_pid)` in Linux column order. Reading the mqueuefs file also bumps
    /// atime+ctime, matching `mqueue_read_file` (ipc/mqueue.c:654).
    pub fn report(&self) -> (usize, u32, u32, u32) {
        let mut inner = self.inner.lock();
        let now = wall_time();
        inner.atime = now;
        inner.ctime = now;
        let qsize = Self::qsize_of(&inner);
        match &inner.notify {
            // Linux prints SIGNO only for a `SIGEV_SIGNAL` registration; any
            // other kind reports 0 (ipc/mqueue.c `mqueue_read_file`).
            Some(n) => {
                let signo = if n.notify == SIGEV_SIGNAL { n.signo } else { 0 };
                (qsize, n.notify, signo, n.pid)
            }
            None => (qsize, 0, 0, 0),
        }
    }

    /// The queue's owner uid (creator `fsuid`), for `/dev/mqueue` stat.
    pub fn uid(&self) -> u32 {
        self.uid
    }

    /// The queue's owner gid (creator `fsgid`), for `/dev/mqueue` stat.
    pub fn gid(&self) -> u32 {
        self.gid
    }

    /// The queue's permission bits (`i_mode & 0777`), for `/dev/mqueue` stat.
    pub fn mode(&self) -> u16 {
        self.mode
    }

    /// The inode timestamps `(atime, ctime, mtime)` for `/dev/mqueue` stat.
    pub fn times(&self) -> (Duration, Duration, Duration) {
        let inner = self.inner.lock();
        (inner.atime, inner.ctime, inner.mtime)
    }

    /// The mqueuefs inode `i_size`. Linux fixes it at `FILENT_SIZE` (80), the
    /// width of the `QSIZE:...NOTIFY_PID:...` status line, not the live queue
    /// byte count (ipc/mqueue.c:311).
    pub fn inode_size(&self) -> u64 {
        FILENT_SIZE
    }

    /// The queue's `Kstat` (for `fstat(mqd)` and `/dev/mqueue` file stat).
    /// Reports the mqueue inode's real `i_mode` (regular file + stored perm),
    /// creator uid/gid, fixed `FILENT_SIZE` and the maintained timestamps,
    /// matching what Linux `mqueue_get_inode` stamps on the inode.
    pub fn kstat(&self) -> Kstat {
        let (atime, ctime, mtime) = self.times();
        Kstat {
            mode: S_IFREG | self.mode as u32,
            uid: self.uid,
            gid: self.gid,
            size: self.inode_size(),
            atime,
            ctime,
            mtime,
            ..Default::default()
        }
    }

    /// Current byte occupancy of the queue (`info->qsize`, the sum of message
    /// sizes) reported in the `QSIZE:` column.
    fn qsize_of(inner: &Inner) -> usize {
        inner
            .buckets
            .values()
            .flat_map(|b| b.iter())
            .map(|m| m.data.len())
            .sum()
    }

    /// Bump atime+ctime to now, for the `mq_setattr` path which Linux
    /// timestamps with `inode_set_atime_to_ts(inode,
    /// inode_set_ctime_current(inode))` (ipc/mqueue.c:1420).
    pub fn touch_attr(&self) {
        let now = wall_time();
        let mut inner = self.inner.lock();
        inner.atime = now;
        inner.ctime = now;
    }
}

impl Drop for MessageQueue {
    fn drop(&mut self) {
        // The last reference to the queue is gone (all descriptors closed and
        // the name unlinked): this is Linux `mqueue_evict_inode`. Drop the live
        // queue count (ipc/mqueue.c:557) and refund the creator's
        // `RLIMIT_MSGQUEUE` charge (`dec_rlimit_ucounts`, ipc/mqueue.c:549).
        MQ_QUEUES_COUNT.fetch_sub(1, Ordering::Relaxed);
        if self.charged_bytes != 0 {
            refund_user_bytes(self.uid, self.charged_bytes);
        }
    }
}

impl Inner {
    /// Pop the highest-priority, earliest message, cleaning up empty buckets.
    fn pop_highest(&mut self) -> Option<Message> {
        let &prio = self.buckets.keys().next_back()?;
        let bucket = self.buckets.get_mut(&prio)?;
        let msg = bucket.pop_front();
        if bucket.is_empty() {
            self.buckets.remove(&prio);
        }
        if msg.is_some() {
            self.len -= 1;
        }
        msg
    }
}

/// Fire an `mq_notify` delivery on the empty -> non-empty edge, per
/// `__do_notify` (ipc/mqueue.c:777). `SIGEV_SIGNAL` raises a signal,
/// `SIGEV_THREAD` pushes the `NOTIFY_WOKENUP` cookie over the netlink socket,
/// and `SIGEV_NONE` delivers nothing.
fn deliver_notification(n: &Notification) {
    match n.notify {
        SIGEV_SIGNAL => {
            let Some(signo) = Signo::from_repr(n.signo as u8) else {
                return;
            };
            // Linux `__do_notify` sets si_pid / si_uid to the *sender* (the task
            // that enqueued the message driving the empty -> non-empty edge) and
            // si_value to the registrant's `sigev_value`. This runs in the
            // sender's context (`send` drives it on the current task).
            let sender = ax_task::current();
            let sender_pid = sender.as_thread().proc_data.proc.pid();
            let sender_uid = sender.as_thread().cred().uid;
            let info = SignalInfo::new_mqueue(signo, sender_pid, sender_uid, n.sigev_value);
            // Best-effort: a dead registrant just means no delivery, mirroring
            // Linux which silently drops the notification in that case.
            let _ = send_signal_to_process(n.pid, Some(info));
        }
        SIGEV_THREAD => {
            if let Some(thread) = &n.thread {
                // `set_cookie(cookie, NOTIFY_WOKENUP)` then
                // `netlink_sendskb(notify_sock, cookie)` (ipc/mqueue.c:825-826):
                // stamp the last byte and deliver the cookie to the socket the
                // libc helper thread is reading.
                let mut cookie = thread.cookie;
                cookie[NOTIFY_COOKIE_LEN - 1] = NOTIFY_WOKENUP;
                thread.sock.deliver_datagram(cookie.to_vec());
            }
        }
        _ => {}
    }
}

/// Tear down a `SIGEV_THREAD` registration that is being cleared before it
/// fires: send `NOTIFY_REMOVED` so the libc helper thread stops
/// (`remove_notification`, ipc/mqueue.c:850-853). No-op for other kinds.
fn notify_thread_teardown(n: &Notification) {
    if n.notify == SIGEV_THREAD
        && let Some(thread) = &n.thread
    {
        let mut cookie = thread.cookie;
        cookie[NOTIFY_COOKIE_LEN - 1] = NOTIFY_REMOVED;
        thread.sock.deliver_datagram(cookie.to_vec());
    }
}

impl FileLike for MessageQueue {
    fn read(&self, _dst: &mut IoDst) -> AxResult<usize> {
        // Message queues are not read via read(2); mq_timedreceive is used.
        Err(AxError::InvalidInput)
    }

    fn write(&self, _src: &mut IoSrc) -> AxResult<usize> {
        Err(AxError::InvalidInput)
    }

    fn stat(&self) -> AxResult<Kstat> {
        Ok(self.kstat())
    }

    fn nonblocking(&self) -> bool {
        // Blocking is a per-descriptor property (see `MqDescriptor`); the shared
        // queue object itself carries no `O_NONBLOCK` state.
        false
    }

    fn set_nonblocking(&self, _non_blocking: bool) -> AxResult {
        // No-op: `O_NONBLOCK` is applied to the descriptor, not the shared queue.
        Ok(())
    }

    fn path(&self) -> Cow<'_, str> {
        "anon_inode:[mqueue]".into()
    }
}

impl Pollable for MessageQueue {
    fn poll(&self) -> IoEvents {
        let inner = self.inner.lock();
        let mut events = IoEvents::empty();
        events.set(IoEvents::IN, inner.len > 0);
        events.set(IoEvents::OUT, inner.len < inner.max_msg);
        events
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        if events.contains(IoEvents::IN) {
            unsafe { self.poll_recv.register(context.waker(), IoEvents::IN) };
        }
        if events.contains(IoEvents::OUT) {
            unsafe { self.poll_send.register(context.waker(), IoEvents::OUT) };
        }
    }
}

/// A per-descriptor handle over a shared [`MessageQueue`]. Each `mq_open`
/// creates one so the access mode (`O_RDONLY`/`O_WRONLY`/`O_RDWR`) and the
/// `O_NONBLOCK` flag are tracked per descriptor and enforced by
/// `mq_send`/`mq_receive`. The same queue name can be opened with different
/// access modes and blocking behaviour, matching Linux where each open file
/// description carries its own `f_flags`: `mq_setattr` on one descriptor must
/// not change `O_NONBLOCK` on another descriptor of the same queue.
pub struct MqDescriptor {
    queue: Arc<MessageQueue>,
    /// Per-open-file-description flags. The `O_ACCMODE` bits are fixed at open;
    /// `O_NONBLOCK` is the only bit `mq_setattr` may toggle. Atomic so a
    /// `mq_setattr` here never disturbs a sibling descriptor of the same queue.
    flags: AtomicU32,
}

impl MqDescriptor {
    pub fn new(queue: Arc<MessageQueue>, flags: u32) -> Self {
        Self {
            queue,
            flags: AtomicU32::new(flags),
        }
    }

    /// The shared queue this descriptor refers to.
    pub fn queue(&self) -> &Arc<MessageQueue> {
        &self.queue
    }

    /// This descriptor's access mode (`O_RDONLY`/`O_WRONLY`/`O_RDWR`).
    pub fn access(&self) -> u32 {
        self.flags.load(Ordering::Acquire) & O_ACCMODE
    }

    /// Whether this descriptor is in non-blocking mode (`O_NONBLOCK`).
    pub fn is_nonblocking(&self) -> bool {
        self.flags.load(Ordering::Acquire) & O_NONBLOCK != 0
    }

    /// Toggle this descriptor's `O_NONBLOCK` bit — the only writable part of
    /// `mq_setattr` — leaving every other descriptor of the same queue and all
    /// other flag bits untouched.
    pub fn set_nonblocking_flag(&self, non_blocking: bool) {
        let _ = self
            .flags
            .try_update(Ordering::AcqRel, Ordering::Acquire, |f| {
                Some(if non_blocking {
                    f | O_NONBLOCK
                } else {
                    f & !O_NONBLOCK
                })
            });
    }

    /// The full per-descriptor flag word, for `mq_getattr`'s `mq_flags`.
    pub fn flags(&self) -> u32 {
        self.flags.load(Ordering::Acquire)
    }
}

impl FileLike for MqDescriptor {
    fn stat(&self) -> AxResult<Kstat> {
        // `fstat(mqd)` reports the underlying mqueue inode (Linux returns the
        // mqueuefs inode's mode/uid/gid/size/times).
        Ok(self.queue.kstat())
    }

    fn nonblocking(&self) -> bool {
        self.is_nonblocking()
    }

    fn set_nonblocking(&self, non_blocking: bool) -> AxResult {
        self.set_nonblocking_flag(non_blocking);
        Ok(())
    }

    fn open_flags(&self) -> u32 {
        self.flags.load(Ordering::Acquire)
    }

    fn on_close(&self, owner: Pid) {
        // Linux `mqueue_flush_file` clears the notification owned by the
        // closing task's tgid on every fd-closing path (ipc/mqueue.c:658).
        self.queue.clear_notify_owner(owner);
    }

    fn path(&self) -> Cow<'_, str> {
        "anon_inode:[mqueue]".into()
    }
}

impl Pollable for MqDescriptor {
    fn poll(&self) -> IoEvents {
        self.queue.poll()
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        self.queue.register(context, events)
    }
}

/// Global `name -> queue` registry. `mq_open` binds names here; `mq_unlink`
/// removes the binding while descriptors keep the `Arc` alive.
pub static MQ_REGISTRY: Mutex<BTreeMap<String, Arc<MessageQueue>>> = Mutex::new(BTreeMap::new());

/// Validate a POSIX message-queue name.
///
/// Per `mq_overview(7)`: a name is a leading `/` followed by one or more
/// characters, none of which is `/`, up to `NAME_MAX`. `EINVAL` for a bad
/// shape, `ENAMETOOLONG` for an overlong name.
pub fn validate_name(name: &str) -> AxResult<&str> {
    // glibc/musl strip the leading '/' before the mq_open syscall (`name + 1`),
    // so the kernel receives a bare single-component name — matching Linux
    // mqueuefs, which treats the argument as a filename under the mount.
    if name.is_empty() || name.contains('/') {
        return Err(LinuxError::EINVAL.into());
    }
    if name.len() > MQ_NAME_MAX {
        return Err(LinuxError::ENAMETOOLONG.into());
    }
    Ok(name)
}

/// Iterate registry entries (used by `/dev/mqueue`). The leading `/` is
/// stripped, matching the filenames Linux exposes under the mqueuefs mount.
pub fn registry_names() -> Vec<String> {
    MQ_REGISTRY
        .lock()
        .keys()
        .filter_map(|k| k.strip_prefix('/').map(String::from))
        .collect()
}

/// Look up a queue by its `/dev/mqueue` filename (no leading `/`).
pub fn lookup_by_short_name(short: &str) -> Option<Arc<MessageQueue>> {
    let mut key = String::with_capacity(short.len() + 1);
    key.push('/');
    key.push_str(short);
    MQ_REGISTRY.lock().get(&key).cloned()
}
