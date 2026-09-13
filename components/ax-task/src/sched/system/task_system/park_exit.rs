//! Park, current-thread exit, and physical switch-tail completion.

use super::*;
use crate::{
    sched::system::cpu::{PreviousSwitchDisposition, PreviousSwitchOwnership},
    thread::ParkPublication,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RqOnlyParkClass {
    Fair,
    Realtime,
}

fn classify_rq_only_park_class(
    policy: SchedulePolicy,
    linked_current: bool,
    rt_quota_exempt: bool,
) -> Option<RqOnlyParkClass> {
    match policy {
        SchedulePolicy::Fair { .. } if !linked_current => Some(RqOnlyParkClass::Fair),
        SchedulePolicy::Fifo { .. } | SchedulePolicy::RoundRobin { .. }
            if linked_current && !rt_quota_exempt =>
        {
            Some(RqOnlyParkClass::Realtime)
        }
        _ => None,
    }
}

pub(crate) struct CurrentExitPermit {
    scheduler_exit: OwnedThreadSchedulerExit,
    current_core: Arc<ThreadCore>,
}

impl CurrentExitPermit {
    pub(crate) fn thread(&self) -> ThreadId {
        self.current_core.id()
    }

    fn current_core(&self) -> &Arc<ThreadCore> {
        &self.current_core
    }

    fn seal(&mut self) {
        self.scheduler_exit.seal();
    }
}

mod park;

mod park_commit;

mod park_run_queue;

mod exit;

mod completion;
