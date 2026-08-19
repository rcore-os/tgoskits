//! Process wait channels and immutable exit metadata.

use alloc::sync::Arc;
use core::{future::poll_fn, task::Poll};

use axpoll::{IoEvents, PollSet};
use starry_signal::Signo;

use super::{PidRoleLease, ProcessData, Tid, TidNumber, current_user_task, future};
use crate::sync::{IrqMutex, PiMutex};

struct VforkDone {
    done: bool,
    poll: Arc<PollSet>,
}

struct RetiredLeader {
    nice: i32,
    tid_lease: PidRoleLease<Tid>,
}

impl VforkDone {
    fn new(poll: Arc<PollSet>) -> Self {
        Self { done: false, poll }
    }
}

/// Exit metadata and wait channels owned by one process generation.
pub(super) struct ProcessWaitState {
    child_exit_event: Arc<PollSet>,
    exit_event: Arc<PollSet>,
    thread_exit_event: Arc<PollSet>,
    exec_lock: PiMutex<()>,
    exit_signal: Option<Signo>,
    wait_parent_tid: TidNumber,
    retired_leader: IrqMutex<Option<RetiredLeader>>,
    vfork_done: IrqMutex<Option<VforkDone>>,
}

impl ProcessWaitState {
    pub(super) fn new(exit_signal: Option<Signo>, wait_parent_tid: TidNumber) -> Self {
        Self {
            child_exit_event: Arc::default(),
            exit_event: Arc::default(),
            thread_exit_event: Arc::default(),
            exec_lock: PiMutex::new(()),
            exit_signal,
            wait_parent_tid,
            retired_leader: IrqMutex::new(None),
            vfork_done: IrqMutex::new(None),
        }
    }

    pub(super) fn exit_event_arc(&self) -> Arc<PollSet> {
        self.exit_event.clone()
    }
}

/// Waits on a poll set while closing the check-versus-register race.
pub async fn wait_on_pollset<T>(poll: &PollSet, mut check: impl FnMut() -> Option<T>) -> T {
    poll_fn(move |cx| {
        if let Some(value) = check() {
            return Poll::Ready(value);
        }

        // Registration happens from wait task context.
        unsafe { poll.register(cx.waker(), IoEvents::IN) };

        if let Some(value) = check() {
            Poll::Ready(value)
        } else {
            Poll::Pending
        }
    })
    .await
}

impl ProcessData {
    pub fn child_exit_event(&self) -> &PollSet {
        &self.wait.child_exit_event
    }

    pub fn exit_event(&self) -> &PollSet {
        &self.wait.exit_event
    }

    pub fn thread_exit_event(&self) -> &PollSet {
        &self.wait.thread_exit_event
    }

    pub fn exec_lock(&self) -> &PiMutex<()> {
        &self.wait.exec_lock
    }

    pub fn exit_signal(&self) -> Option<Signo> {
        self.wait.exit_signal
    }

    pub fn wait_parent_tid(&self) -> TidNumber {
        self.wait.wait_parent_tid
    }

    /// Transfers the exited thread-group leader's retained state to the process.
    pub(crate) fn retire_leader(&self, nice: i32, tid_lease: PidRoleLease<Tid>) {
        let previous = self
            .wait
            .retired_leader
            .lock()
            .replace(RetiredLeader { nice, tid_lease });
        assert!(previous.is_none(), "process retired its leader twice");
    }

    /// Reports transfer readiness from the retained TID's exact PID identity.
    pub(crate) fn retired_leader_transfer_ready(&self) -> bool {
        self.wait
            .retired_leader
            .lock()
            .as_ref()
            .is_some_and(|leader| leader.tid_lease.task_transfer_ready())
    }

    /// Returns the nice value retained for an exited thread-group leader.
    pub fn retired_leader_nice(&self) -> Option<i32> {
        self.wait
            .retired_leader
            .lock()
            .as_ref()
            .map(|leader| leader.nice)
    }

    /// Transfers the retired leader state into the final zombie snapshot.
    pub(crate) fn take_retired_leader_for_zombie(&self) -> (i32, PidRoleLease<Tid>) {
        let leader = self
            .wait
            .retired_leader
            .lock()
            .take()
            .expect("process lost its retired leader state");
        (leader.nice, leader.tid_lease)
    }

    /// Transfers a fully retired leader identity to a non-leader exec caller.
    pub(crate) fn take_retired_leader_for_exec(&self) -> (i32, PidRoleLease<Tid>) {
        let leader = self
            .wait
            .retired_leader
            .lock()
            .take()
            .expect("process lost its retired leader state");
        assert!(
            leader.tid_lease.task_transfer_ready(),
            "exec transferred a leader identity before its exit path completed"
        );
        (leader.nice, leader.tid_lease)
    }

    /// Returns whether this child uses clone-style exit notification.
    pub fn is_clone_child(&self) -> bool {
        self.wait.exit_signal != Some(Signo::SIGCHLD)
    }

    /// Installs the vfork completion before the child is published.
    pub fn set_vfork_done(&self, poll: Arc<PollSet>) {
        *self.wait.vfork_done.lock() = Some(VforkDone::new(poll));
    }

    /// Waits until the vfork child execs/exits or this thread is zapped.
    pub fn wait_vfork_done(&self) {
        let poll = {
            let guard = self.wait.vfork_done.lock();
            match guard.as_ref() {
                Some(vfork) => vfork.poll.clone(),
                None => return,
            }
        };
        let curr_task = current_user_task();
        let curr_thr = curr_task.as_thread();
        loop {
            let result = future::block_on_user(
                &curr_task,
                core::future::poll_fn(|cx| {
                    // Registration happens before the completion recheck.
                    unsafe { poll.register(cx.waker(), IoEvents::IN) };
                    let done = self
                        .wait
                        .vfork_done
                        .lock()
                        .as_ref()
                        .map(|vfork| vfork.done)
                        .unwrap_or(true);
                    if done {
                        core::task::Poll::Ready(())
                    } else {
                        core::task::Poll::Pending
                    }
                }),
            );
            match result {
                future::UserWaitOutcome::Ready(()) => return,
                future::UserWaitOutcome::Interrupted if curr_thr.has_exit_request() => return,
                future::UserWaitOutcome::Interrupted => continue,
                future::UserWaitOutcome::TimedOut => {
                    unreachable!("vfork completion wait has no deadline")
                }
            }
        }
    }

    /// Publishes vfork completion before waking the parent.
    pub fn notify_vfork_done(&self) {
        let poll = {
            let mut guard = self.wait.vfork_done.lock();
            match guard.as_mut() {
                Some(vfork) => {
                    vfork.done = true;
                    vfork.poll.clone()
                }
                None => return,
            }
        };
        unsafe { poll.wake(IoEvents::IN) };
    }
}
