//! Process wait channels and immutable exit metadata.

use alloc::sync::Arc;
use core::task::Poll;

use axpoll::IoEvents;
use axpoll_set::PollSet;
use starry_signal::Signo;

use super::{PidRoleLease, ProcessData, Tid, TidNumber, future};
use crate::sync::{IrqMutex, Mutex};

struct RetiredLeader {
    nice: i32,
    tid_lease: PidRoleLease<Tid>,
}

/// Exit metadata and wait channels owned by one process generation.
pub(super) struct ProcessWaitState {
    child_exit_event: Arc<PollSet>,
    exit_event: Arc<PollSet>,
    thread_exit_event: Arc<PollSet>,
    exec_lock: Mutex<()>,
    exit_signal: Option<Signo>,
    wait_parent_tid: TidNumber,
    retired_leader: IrqMutex<Option<RetiredLeader>>,
}

impl ProcessWaitState {
    pub(super) fn new(exit_signal: Option<Signo>, wait_parent_tid: TidNumber) -> Self {
        Self {
            child_exit_event: Arc::default(),
            exit_event: Arc::default(),
            thread_exit_event: Arc::default(),
            exec_lock: Mutex::new(()),
            exit_signal,
            wait_parent_tid,
            retired_leader: IrqMutex::new(None),
        }
    }

    pub(super) fn exit_event_arc(&self) -> Arc<PollSet> {
        self.exit_event.clone()
    }
}

/// Waits on a poll set while closing the check-versus-register race.
pub async fn wait_on_pollset<T>(poll: &PollSet, mut check: impl FnMut() -> Option<T>) -> T {
    future::poll_shared(
        || check().map_or(Poll::Pending, Poll::Ready),
        |registrar| unsafe { registrar.register(poll, IoEvents::IN) },
    )
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

    pub fn exec_lock(&self) -> &Mutex<()> {
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
}
