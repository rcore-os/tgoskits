//! Process job-control state and parent-report delivery.

use alloc::sync::Arc;

use axpoll::{IoEvents, PollSet};
use starry_signal::Signo;

use super::{ProcessData, TidNumber};
use crate::sync::IrqMutex;

/// A pending job-control status change for `waitpid`.
#[derive(Clone, Copy)]
pub enum JobStatus {
    Stopped(Signo),
    Continued,
}

/// Live stop state and one-shot parent report.
///
/// The two fields intentionally remain independent: a stopped process may
/// already have had its parent report consumed.
#[derive(Default)]
struct JobControl {
    stopped: Option<Signo>,
    status: Option<JobStatus>,
    continue_generation: u64,
    /// The thread physically parked by the process-directed stop.
    waiter_tid: Option<TidNumber>,
}

pub(super) struct ProcessJobControl {
    state: IrqMutex<JobControl>,
    continue_event: Arc<PollSet>,
}

impl ProcessJobControl {
    pub(super) fn new() -> Self {
        Self {
            state: IrqMutex::new(JobControl::default()),
            continue_event: Arc::default(),
        }
    }
}

impl ProcessData {
    pub fn is_job_stopped(&self) -> bool {
        self.job_control.state.lock().stopped.is_some()
    }

    pub fn set_job_stopped(
        &self,
        signo: Signo,
        continue_gen_snapshot: u64,
        waiter_tid: TidNumber,
    ) -> bool {
        let mut state = self.job_control.state.lock();
        if state.continue_generation != continue_gen_snapshot {
            return false;
        }
        state.stopped = Some(signo);
        state.status = Some(JobStatus::Stopped(signo));
        state.waiter_tid = Some(waiter_tid);
        true
    }

    /// Returns whether `tid` is the thread physically parked for this stop.
    pub fn is_job_stop_waiter(&self, tid: TidNumber) -> bool {
        let state = self.job_control.state.lock();
        state.stopped.is_some() && state.waiter_tid == Some(tid)
    }

    pub fn continue_generation(&self) -> u64 {
        self.job_control.state.lock().continue_generation
    }

    pub fn set_job_continued(&self) -> bool {
        let mut state = self.job_control.state.lock();
        state.continue_generation = state.continue_generation.wrapping_add(1);
        let was_stopped = state.stopped.take().is_some();
        state.waiter_tid = None;
        if was_stopped {
            state.status = Some(JobStatus::Continued);
            drop(state);
            // Continue state is visible before parked threads are woken.
            unsafe { self.job_control.continue_event.wake(IoEvents::IN) };
        }
        was_stopped
    }

    pub fn clear_job_stop_for_kill(&self) {
        let mut state = self.job_control.state.lock();
        let was_stopped = state.stopped.take().is_some();
        state.waiter_tid = None;
        drop(state);
        if was_stopped {
            // Kill observes cleared stop state before the wake.
            unsafe { self.job_control.continue_event.wake(IoEvents::IN) };
        }
    }

    /// Wakes the parked stop waiter so it can publish a pending ptrace event.
    ///
    /// This does not change the job-control state. Only continue or kill may
    /// release the stop.
    pub fn wake_job_stop_waiter(&self) {
        unsafe { self.job_control.continue_event.wake(IoEvents::IN) };
    }

    pub fn cont_event(&self) -> Arc<PollSet> {
        self.job_control.continue_event.clone()
    }

    pub fn peek_job_status_if(
        &self,
        want_stopped: bool,
        want_continued: bool,
    ) -> Option<JobStatus> {
        let state = self.job_control.state.lock();
        match state.status {
            Some(status @ JobStatus::Stopped(_)) if want_stopped => Some(status),
            Some(status @ JobStatus::Continued) if want_continued => Some(status),
            _ => None,
        }
    }

    pub fn take_job_status_if(
        &self,
        want_stopped: bool,
        want_continued: bool,
    ) -> Option<JobStatus> {
        let mut state = self.job_control.state.lock();
        match state.status {
            Some(JobStatus::Stopped(_)) if want_stopped => state.status.take(),
            Some(JobStatus::Continued) if want_continued => state.status.take(),
            _ => None,
        }
    }
}
