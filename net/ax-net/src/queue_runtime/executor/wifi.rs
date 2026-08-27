use alloc::{boxed::Box, sync::Arc};
use core::sync::atomic::Ordering;

use rd_net::{NetError, NetRearmResult, WifiControlProgress};

use super::{
    super::{WifiControlQueue, WifiControlRequest},
    QueueGroupExecutor,
};

pub(in crate::queue_runtime) struct WifiExecutorSlot {
    pub(in crate::queue_runtime) group_index: usize,
    pub(in crate::queue_runtime) control: Box<dyn rd_net::WifiControl>,
    pub(in crate::queue_runtime) queue: Arc<WifiControlQueue>,
    pub(in crate::queue_runtime) active: Option<ActiveWifiRequest>,
}

pub(in crate::queue_runtime) struct ActiveWifiRequest {
    request: WifiControlRequest,
    wait: WifiWait,
}

enum WifiWait {
    Ready,
    Interrupt {
        generation: u64,
    },
    InterruptUntil {
        generation: u64,
        deadline_nanos: u64,
    },
    Deadline {
        deadline_nanos: u64,
    },
}

pub(super) fn process_wifi_requests(
    groups: &mut [QueueGroupExecutor],
    wifi: &mut [WifiExecutorSlot],
) -> bool {
    let mut handled = false;
    for slot in wifi {
        let now_nanos = ax_hal::time::monotonic_time_nanos();
        if let Some(active) = slot.active.take() {
            if !active.wait.is_ready(&groups[slot.group_index], now_nanos) {
                slot.active = Some(active);
                continue;
            }
            handled = true;
            advance_wifi_request(
                slot,
                &mut groups[slot.group_index],
                active.request,
                now_nanos,
            );
        } else if let Some(request) = slot.queue.try_pop() {
            handled = true;
            start_wifi_request(slot, &mut groups[slot.group_index], request, now_nanos);
        }
    }
    handled
}

fn start_wifi_request(
    slot: &mut WifiExecutorSlot,
    group: &mut QueueGroupExecutor,
    request: WifiControlRequest,
    now_nanos: u64,
) {
    let progress = run_wifi_step(group, || {
        slot.control
            .start(request.transaction.operation(), now_nanos)
    });
    finish_wifi_step(slot, group, request, progress);
}

fn advance_wifi_request(
    slot: &mut WifiExecutorSlot,
    group: &mut QueueGroupExecutor,
    request: WifiControlRequest,
    now_nanos: u64,
) {
    let progress = run_wifi_step(group, || slot.control.advance(now_nanos));
    finish_wifi_step(slot, group, request, progress);
}

fn run_wifi_step(
    group: &mut QueueGroupExecutor,
    step: impl FnOnce() -> Result<WifiControlProgress, NetError>,
) -> Result<(WifiControlProgress, bool), NetError> {
    group.group.irq_control.quiesce()?;
    let progress = step();
    let rearm = group
        .group
        .irq_control
        .rearm_and_check(ax_hal::time::monotonic_time_nanos());
    match (progress, rearm) {
        (Ok(progress), Ok(NetRearmResult::Idle)) => Ok((progress, false)),
        (Ok(progress), Ok(NetRearmResult::WorkPending(_))) => Ok((progress, true)),
        (Ok(progress), Ok(NetRearmResult::RetryAt { deadline_nanos })) => {
            group.retry_at = Some(deadline_nanos);
            Ok((progress, false))
        }
        (Err(error), Ok(_)) => Err(error),
        (_, Err(error)) => {
            group.shared.disable();
            Err(error)
        }
    }
}

fn finish_wifi_step(
    slot: &mut WifiExecutorSlot,
    group: &mut QueueGroupExecutor,
    request: WifiControlRequest,
    progress: Result<(WifiControlProgress, bool), NetError>,
) {
    match progress {
        Ok((WifiControlProgress::Complete, work_pending)) => {
            if work_pending {
                group.shared.schedule_task();
            }
            request.completion.complete(Ok(()));
        }
        Ok((progress, work_pending)) => {
            let wait = if work_pending {
                WifiWait::Ready
            } else {
                WifiWait::from_progress(progress, group)
            };
            slot.active = Some(ActiveWifiRequest { request, wait });
        }
        Err(error) => {
            let _ = slot.control.cancel();
            request.completion.complete(Err(error));
        }
    }
}

impl WifiWait {
    fn from_progress(progress: WifiControlProgress, group: &QueueGroupExecutor) -> Self {
        match progress {
            WifiControlProgress::Complete => Self::Ready,
            WifiControlProgress::WaitForInterrupt => Self::Interrupt {
                generation: group.shared.stats.irq.load(Ordering::Acquire),
            },
            WifiControlProgress::WaitForInterruptUntil { deadline_nanos } => Self::InterruptUntil {
                generation: group.shared.stats.irq.load(Ordering::Acquire),
                deadline_nanos,
            },
            WifiControlProgress::RetryAt { deadline_nanos } => Self::Deadline { deadline_nanos },
        }
    }

    fn is_ready(&self, group: &QueueGroupExecutor, now_nanos: u64) -> bool {
        match self {
            Self::Ready => true,
            Self::Interrupt { generation } => {
                group.shared.stats.irq.load(Ordering::Acquire) != *generation
            }
            Self::InterruptUntil {
                generation,
                deadline_nanos,
            } => {
                group.shared.stats.irq.load(Ordering::Acquire) != *generation
                    || now_nanos >= *deadline_nanos
            }
            Self::Deadline { deadline_nanos } => now_nanos >= *deadline_nanos,
        }
    }

    const fn deadline(&self) -> Option<u64> {
        match self {
            Self::Deadline { deadline_nanos } | Self::InterruptUntil { deadline_nanos, .. } => {
                Some(*deadline_nanos)
            }
            Self::Ready | Self::Interrupt { .. } => None,
        }
    }
}

impl WifiExecutorSlot {
    pub(super) fn has_runnable_work(&self, groups: &[QueueGroupExecutor], now_nanos: u64) -> bool {
        self.queue.has_pending()
            || self
                .active
                .as_ref()
                .is_some_and(|active| active.wait.is_ready(&groups[self.group_index], now_nanos))
    }

    pub(super) fn deadline(&self) -> Option<u64> {
        self.active
            .as_ref()
            .and_then(|active| active.wait.deadline())
    }

    pub(super) fn cancel_active(&mut self) {
        if let Some(active) = self.active.take() {
            let _ = self.control.cancel();
            active.request.completion.complete(Err(NetError::Stopped));
        }
        self.queue.stop();
    }

    pub(super) fn abandon_active(&self) {
        if let Some(active) = &self.active {
            active.request.completion.complete(Err(NetError::Stopped));
        }
        self.queue.stop();
    }
}
