use alloc::{collections::VecDeque, string::String, sync::Arc, vec::Vec};
use core::{
    num::NonZeroUsize,
    sync::atomic::{AtomicU8, Ordering},
};

use ax_sync::SpinLock;
use rd_net::{
    DmaBuffer, NetError, NetOwnerStartupProgress, NetRearmResult, PreparedNetPollGroup,
    RxCompletion,
};

use super::{
    COMMAND_QUARANTINE, COMMAND_STOP, COMMAND_WAIT, CPU_ROUND_BUDGET, PollGroupState, QUEUE_BUDGET,
    STATE_MASK, STATE_MISSED, STATE_POLLING, STATE_SCHEDULED, STATUS_FAILED, STATUS_READY,
    SpscConsumer, SpscProducer, TxQueueDiscipline,
};
use crate::device::{EthernetFramePort, NetDeviceError, NetDeviceResult, ProtocolEthernetFrame};

mod wifi;

pub(super) use wifi::WifiExecutorSlot;
use wifi::process_wifi_requests;

pub(super) struct ProtocolGroupPort {
    pub(super) rx_ready: SpscConsumer<RxCompletion>,
    pub(super) rx_recycle: SpscProducer<DmaBuffer>,
    pub(super) tx_ready: SpscProducer<DmaBuffer>,
    pub(super) tx_free: SpscConsumer<DmaBuffer>,
    pub(super) pending_recycle: Vec<DmaBuffer>,
    pub(super) tx_spares: Vec<DmaBuffer>,
    pub(super) shared: Arc<PollGroupState>,
}

impl ProtocolGroupPort {
    pub(super) fn flush_recycle(&mut self) {
        while let Some(buffer) = self.pending_recycle.pop() {
            if let Err(buffer) = self.rx_recycle.push(buffer) {
                self.pending_recycle.push(buffer);
                break;
            }
            self.shared.schedule_task();
        }
    }

    fn recycle_rx_buffer(&mut self, buffer: DmaBuffer) {
        if let Err(buffer) = self.rx_recycle.push(buffer) {
            self.pending_recycle.push(buffer);
        }
        self.shared.schedule_task();
    }

    pub(super) fn receive(&mut self) -> NetDeviceResult<ProtocolEthernetFrame> {
        self.flush_recycle();
        let completion = self.rx_ready.pop().ok_or(NetDeviceError::Again)?;
        let frame = if completion.packet_len > completion.buffer.capacity() {
            Err(NetDeviceError::Io)
        } else {
            completion
                .buffer
                .read_with_cpu(completion.packet_len, |packet| {
                    ProtocolEthernetFrame::copy_from_slice(packet)
                })
        };
        self.recycle_rx_buffer(completion.buffer);
        frame
    }

    fn transmit(&mut self, frame: &ProtocolEthernetFrame) -> NetDeviceResult {
        let Some(mut buffer) = self.tx_spares.pop().or_else(|| self.tx_free.pop()) else {
            return Err(NetDeviceError::Again);
        };
        if buffer.set_len(frame.packet_len()).is_err() {
            self.tx_spares.push(buffer);
            return Err(NetDeviceError::InvalidParam);
        }
        buffer.write_with_cpu(|target| target.copy_from_slice(frame.packet()));
        if let Err(buffer) = self.tx_ready.push(buffer) {
            self.tx_spares.push(buffer);
            return Err(NetDeviceError::Again);
        }
        self.shared.schedule_task();
        Ok(())
    }
}

pub(super) struct QueueFramePort {
    pub(super) name: String,
    pub(super) mac: Arc<SpinLock<[u8; 6]>>,
    pub(super) groups: Vec<ProtocolGroupPort>,
    /// Device-level policy for handling a busy transmit queue.
    pub(super) tx_queue_discipline: TxQueueDiscipline,
    /// Lazily allocated FIFO storage used only by `TxQueueDiscipline::Fifo`.
    pub(super) pending_tx: VecDeque<ProtocolEthernetFrame>,
    pub(super) next_rx: usize,
    pub(super) next_tx: usize,
}

impl EthernetFramePort for QueueFramePort {
    fn device_name(&self) -> &str {
        &self.name
    }

    fn mac_address(&self) -> [u8; 6] {
        *self.mac.lock_irqsave()
    }

    fn transmit(&mut self, frame: &ProtocolEthernetFrame) -> NetDeviceResult {
        if self.groups.is_empty() {
            return Err(NetDeviceError::Stopped);
        }

        let TxQueueDiscipline::Fifo { max_frames } = self.tx_queue_discipline else {
            debug_assert!(self.pending_tx.is_empty());
            return self.try_transmit(frame);
        };

        self.flush_pending_tx()?;
        if !self.pending_tx.is_empty() {
            return self.enqueue_pending(frame, max_frames);
        }

        match self.try_transmit(frame) {
            Ok(()) => Ok(()),
            Err(NetDeviceError::Again) => self.enqueue_pending(frame, max_frames),
            Err(error) => Err(error),
        }
    }

    fn receive(&mut self) -> NetDeviceResult<ProtocolEthernetFrame> {
        if self.groups.is_empty() {
            return Err(NetDeviceError::Stopped);
        }
        // Completion of a DMA TX token requests another protocol poll. Flush
        // the retained qdisc-like backlog before consuming RX work so a busy
        // TX queue cannot remain stranded while the link is otherwise live.
        self.flush_pending_tx()?;
        for offset in 0..self.groups.len() {
            let index = (self.next_rx + offset) % self.groups.len();
            match self.groups[index].receive() {
                Ok(frame) => {
                    self.next_rx = (index + 1) % self.groups.len();
                    return Ok(frame);
                }
                Err(NetDeviceError::Again) => {}
                Err(error) => return Err(error),
            }
        }
        Err(NetDeviceError::Again)
    }
}

impl QueueFramePort {
    fn try_transmit(&mut self, frame: &ProtocolEthernetFrame) -> NetDeviceResult {
        for offset in 0..self.groups.len() {
            let index = (self.next_tx + offset) % self.groups.len();
            match self.groups[index].transmit(frame) {
                Ok(()) => {
                    self.next_tx = (index + 1) % self.groups.len();
                    return Ok(());
                }
                Err(NetDeviceError::Again) => {}
                Err(error) => return Err(error),
            }
        }
        Err(NetDeviceError::Again)
    }

    fn enqueue_pending(
        &mut self,
        frame: &ProtocolEthernetFrame,
        max_frames: NonZeroUsize,
    ) -> NetDeviceResult {
        if self.pending_tx.len() >= max_frames.get() {
            return Err(NetDeviceError::Again);
        }
        self.pending_tx.push_back(frame.clone());
        Ok(())
    }

    fn flush_pending_tx(&mut self) -> NetDeviceResult {
        while let Some(frame) = self.pending_tx.pop_front() {
            match self.try_transmit(&frame) {
                Ok(()) => {}
                Err(NetDeviceError::Again) => {
                    self.pending_tx.push_front(frame);
                    break;
                }
                Err(error) => {
                    return Err(error);
                }
            }
        }
        Ok(())
    }
}

pub(super) enum GroupPollOutcome {
    Idle(usize),
    More(usize),
    Blocked(usize),
    Failed,
}

pub(super) const fn hardware_retry_outcome(work: usize) -> GroupPollOutcome {
    // The driver returned the token because only a future device event can
    // make progress.  Rearm the IRQ before sleeping instead of spinning on
    // the same token with the source masked.
    GroupPollOutcome::Idle(work)
}

pub(super) const fn waits_for_hardware_event(reason: &NetError) -> bool {
    matches!(reason, NetError::Retry | NetError::LinkDown)
}

pub(super) const fn rx_refill_retry_outcome(work: usize, received: usize) -> GroupPollOutcome {
    if received == 0 {
        hardware_retry_outcome(work)
    } else {
        // Reclaiming an RX descriptor can make the retained refill token
        // immediately submittable, so retry only after observable progress.
        GroupPollOutcome::More(work)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExecutorWait {
    Notification,
    Deadline(core::time::Duration),
    Ready,
}

const fn executor_wait(now_nanos: u64, deadline_nanos: Option<u64>) -> ExecutorWait {
    match deadline_nanos {
        None => ExecutorWait::Notification,
        Some(deadline_nanos) if deadline_nanos <= now_nanos => ExecutorWait::Ready,
        Some(deadline_nanos) => {
            ExecutorWait::Deadline(core::time::Duration::from_nanos(deadline_nanos - now_nanos))
        }
    }
}

pub(super) struct QueueGroupExecutor {
    pub(super) group: PreparedNetPollGroup,
    pub(super) rx_ready: SpscProducer<RxCompletion>,
    pub(super) rx_recycle: SpscConsumer<DmaBuffer>,
    pub(super) tx_ready: SpscConsumer<DmaBuffer>,
    pub(super) tx_free: SpscProducer<DmaBuffer>,
    pub(super) pending_rx: Option<RxCompletion>,
    pub(super) pending_rx_recycle: Option<DmaBuffer>,
    pub(super) pending_tx: Option<DmaBuffer>,
    pub(super) pending_tx_free: Option<DmaBuffer>,
    pub(super) retry_at: Option<u64>,
    pub(super) shared: Arc<PollGroupState>,
}

impl QueueGroupExecutor {
    fn initialize(&mut self) -> Result<(), NetError> {
        if let Some(mut startup) = self.group.owner_startup.take() {
            let mut progress = startup.start(ax_hal::time::monotonic_time_nanos());
            loop {
                progress = match progress {
                    Ok(NetOwnerStartupProgress::Ready) => break,
                    Ok(NetOwnerStartupProgress::WaitForInterrupt) => {
                        self.shared.wait_startup_irq();
                        startup.advance(ax_hal::time::monotonic_time_nanos())
                    }
                    Ok(NetOwnerStartupProgress::WaitForInterruptUntil { deadline_nanos }) => {
                        self.shared.wait_startup_deadline(deadline_nanos);
                        startup.advance(ax_hal::time::monotonic_time_nanos())
                    }
                    Ok(NetOwnerStartupProgress::RetryAt { deadline_nanos }) => {
                        self.shared.wait_startup_deadline(deadline_nanos);
                        startup.advance(ax_hal::time::monotonic_time_nanos())
                    }
                    Err(error) => {
                        let _ = startup.cancel();
                        return Err(error);
                    }
                };
            }
        }
        let tx_capacity = self.group.tx.capacity();
        for _ in 0..tx_capacity {
            let buffer = self
                .group
                .tx_pool
                .allocate(self.group.tx_pool.buffer_size())?;
            self.tx_free
                .push(buffer)
                .map_err(|_| NetError::InvalidParts)?;
        }
        self.group.rx.initial_refill(self.group.rx.capacity())?;
        let pending = match self
            .group
            .irq_control
            .rearm_and_check(ax_hal::time::monotonic_time_nanos())?
        {
            NetRearmResult::Idle => false,
            NetRearmResult::WorkPending(_) => true,
            NetRearmResult::RetryAt { deadline_nanos } => {
                self.retry_at = Some(deadline_nanos);
                false
            }
        };
        self.shared.activate(pending);
        Ok(())
    }

    fn poll(&mut self, cpu_budget: usize) -> GroupPollOutcome {
        if self.shared.is_disabled() {
            return GroupPollOutcome::Failed;
        }
        if self.group.irq_control.quiesce().is_err() {
            self.shared.disable();
            return GroupPollOutcome::Failed;
        }

        let mut work = 0;
        if let Some(buffer) = self.pending_tx_free.take() {
            if let Err(buffer) = self.tx_free.push(buffer) {
                self.pending_tx_free = Some(buffer);
                return GroupPollOutcome::Blocked(work);
            }
            crate::request_poll();
        }

        let tx_completion_budget = QUEUE_BUDGET.min(cpu_budget.saturating_sub(work));
        let mut tx_completed = 0;
        while tx_completed < tx_completion_budget {
            let Some(buffer) = self.group.tx.reclaim() else {
                break;
            };
            tx_completed += 1;
            work += 1;
            if let Err(buffer) = self.tx_free.push(buffer) {
                self.pending_tx_free = Some(buffer);
                return GroupPollOutcome::Blocked(work);
            }
            crate::request_poll();
        }

        let tx_submit_budget = QUEUE_BUDGET.min(cpu_budget.saturating_sub(work));
        let mut submitted = 0;
        while submitted < tx_submit_budget {
            let buffer = match self.pending_tx.take().or_else(|| self.tx_ready.pop()) {
                Some(buffer) => buffer,
                None => break,
            };
            match self.group.tx.submit(buffer) {
                Ok(()) => {
                    submitted += 1;
                    work += 1;
                }
                Err(error) => {
                    let (buffer, reason) = error.into_parts();
                    if waits_for_hardware_event(&reason) {
                        self.pending_tx = Some(buffer);
                        return hardware_retry_outcome(work);
                    }
                    if let Err(buffer) = self.tx_free.push(buffer) {
                        self.pending_tx_free = Some(buffer);
                        return GroupPollOutcome::Blocked(work);
                    }
                }
            }
        }

        if let Some(completion) = self.pending_rx.take() {
            match self.rx_ready.push(completion) {
                Ok(()) => crate::request_poll(),
                Err(completion) => {
                    self.pending_rx = Some(completion);
                    return GroupPollOutcome::Blocked(work);
                }
            }
        }
        let mut rx_refill_blocked = false;
        if let Some(buffer) = self.pending_rx_recycle.take() {
            match self.group.rx.recycle(buffer) {
                Ok(()) => work += 1,
                Err(error) => {
                    let (buffer, reason) = error.into_parts();
                    self.pending_rx_recycle = Some(buffer);
                    if !matches!(reason, NetError::Retry) {
                        self.shared.disable();
                        return GroupPollOutcome::Failed;
                    }
                    rx_refill_blocked = true;
                }
            }
        }

        let per_class = QUEUE_BUDGET.min(cpu_budget.saturating_sub(work));
        let mut recycled = 0;
        while !rx_refill_blocked && recycled < per_class {
            let Some(buffer) = self.rx_recycle.pop() else {
                break;
            };
            match self.group.rx.recycle(buffer) {
                Ok(()) => {
                    recycled += 1;
                    work += 1;
                }
                Err(error) => {
                    let (buffer, reason) = error.into_parts();
                    self.pending_rx_recycle = Some(buffer);
                    if !matches!(reason, NetError::Retry) {
                        self.shared.disable();
                        return GroupPollOutcome::Failed;
                    }
                    rx_refill_blocked = true;
                }
            }
        }

        let rx_budget = QUEUE_BUDGET.min(cpu_budget.saturating_sub(work));
        let mut received = 0;
        while received < rx_budget {
            let Some(completion) = self.group.rx.reclaim() else {
                break;
            };
            received += 1;
            work += 1;
            if let Err(completion) = self.rx_ready.push(completion) {
                self.pending_rx = Some(completion);
                return GroupPollOutcome::Blocked(work);
            }
            crate::request_poll();
        }

        if rx_refill_blocked {
            return rx_refill_retry_outcome(work, received);
        }

        let exhausted = budget_was_exhausted(received, rx_budget)
            || budget_was_exhausted(tx_completed, tx_completion_budget)
            || budget_was_exhausted(submitted, tx_submit_budget)
            || budget_was_exhausted(recycled, per_class);
        if exhausted || work >= cpu_budget {
            self.shared
                .stats
                .budget_exhaustion
                .fetch_add(1, Ordering::Relaxed);
            GroupPollOutcome::More(work)
        } else {
            GroupPollOutcome::Idle(work)
        }
    }

    fn finish_idle(&mut self) {
        if !self.shared.begin_rearm() {
            return;
        }
        match self
            .group
            .irq_control
            .rearm_and_check(ax_hal::time::monotonic_time_nanos())
        {
            Ok(NetRearmResult::Idle) => {}
            Ok(NetRearmResult::WorkPending(_)) => {
                self.shared.stats.rearm_race.fetch_add(1, Ordering::Relaxed);
                self.shared.schedule_task();
            }
            Ok(NetRearmResult::RetryAt { deadline_nanos }) => {
                self.retry_at = Some(deadline_nanos);
            }
            Err(_) => self.shared.disable(),
        }
    }

    fn schedule_elapsed_retry(&mut self, now_nanos: u64) {
        if self.retry_at.is_some_and(|deadline| now_nanos >= deadline) {
            self.retry_at = None;
            self.shared.schedule_task();
        }
    }
}

pub(super) const fn budget_was_exhausted(processed: usize, budget: usize) -> bool {
    budget != 0 && processed == budget
}

pub(super) struct ExecutorControl {
    pub(super) owner_cpu: usize,
    pub(super) command: AtomicU8,
    pub(super) affinity_status: AtomicU8,
    pub(super) startup_status: AtomicU8,
    pub(super) startup_error: SpinLock<Option<NetError>>,
    pub(super) notify: Arc<ax_task::IrqNotify>,
}

pub(super) struct ExecutorLease {
    pub(super) control: Arc<ExecutorControl>,
    pub(super) task: ax_task::AxTaskRef,
}

impl ExecutorLease {
    pub(super) fn stop(&self, irq_synchronized: bool) {
        self.control.command.store(
            if irq_synchronized {
                COMMAND_STOP
            } else {
                COMMAND_QUARANTINE
            },
            Ordering::Release,
        );
        self.control.notify.notify();
    }
}

pub(super) fn queue_executor_main(
    mut groups: Vec<QueueGroupExecutor>,
    mut wifi: Vec<WifiExecutorSlot>,
    control: Arc<ExecutorControl>,
) {
    let affinity = ax_task::AxCpuMask::one_shot(control.owner_cpu);
    if !ax_task::set_current_affinity(affinity) {
        control
            .affinity_status
            .store(STATUS_FAILED, Ordering::Release);
        control.notify.notify();
        quarantine_executor_resources(groups, wifi);
        return;
    }
    ax_task::yield_now();
    if ax_hal::percpu::this_cpu_id() != control.owner_cpu {
        control
            .affinity_status
            .store(STATUS_FAILED, Ordering::Release);
        control.notify.notify();
        quarantine_executor_resources(groups, wifi);
        return;
    }
    control
        .affinity_status
        .store(STATUS_READY, Ordering::Release);
    control.notify.notify();

    while control.command.load(Ordering::Acquire) == COMMAND_WAIT {
        control.notify.wait();
    }
    if let Some(irq_synchronized) =
        requested_irq_synchronization(control.command.load(Ordering::Acquire))
    {
        release_executor_resources(groups, wifi, irq_synchronized);
        return;
    }

    let initialization = groups
        .iter_mut()
        .try_for_each(QueueGroupExecutor::initialize);
    if let Err(error) = initialization {
        *control.startup_error.lock_irqsave() = Some(error);
    }
    control.startup_status.store(
        if control.startup_error.lock_irqsave().is_none() {
            STATUS_READY
        } else {
            STATUS_FAILED
        },
        Ordering::Release,
    );
    control.notify.notify();
    if control.startup_error.lock_irqsave().is_some() {
        let irq_synchronized = wait_for_cleanup_command(&control);
        release_executor_resources(groups, wifi, irq_synchronized);
        return;
    }

    loop {
        if let Some(irq_synchronized) =
            requested_irq_synchronization(control.command.load(Ordering::Acquire))
        {
            release_executor_resources(groups, wifi, irq_synchronized);
            return;
        }

        let now_nanos = ax_hal::time::monotonic_time_nanos();
        for group in &mut groups {
            group.schedule_elapsed_retry(now_nanos);
        }
        let mut cpu_work = 0;
        let mut runnable = process_wifi_requests(&mut groups, &mut wifi);
        for group in &mut groups {
            if cpu_work >= CPU_ROUND_BUDGET || !group.shared.claim() {
                continue;
            }
            runnable = true;
            match group.poll(CPU_ROUND_BUDGET - cpu_work) {
                GroupPollOutcome::Idle(work) => {
                    cpu_work += work;
                    group.finish_idle();
                }
                GroupPollOutcome::More(work) => {
                    cpu_work += work;
                    group.shared.finish_more();
                }
                GroupPollOutcome::Blocked(work) => {
                    cpu_work += work;
                }
                GroupPollOutcome::Failed => {}
            }
        }
        if runnable && cpu_work >= CPU_ROUND_BUDGET {
            ax_task::yield_now();
            continue;
        }
        let now_nanos = ax_hal::time::monotonic_time_nanos();
        if !wifi
            .iter()
            .any(|slot| slot.has_runnable_work(&groups, now_nanos))
            && !groups.iter().any(|group| {
                let state = group.shared.state.load(Ordering::Acquire);
                state & STATE_MASK == STATE_SCHEDULED
                    || (state & STATE_MASK == STATE_POLLING && state & STATE_MISSED != 0)
            })
        {
            let deadline_nanos = wifi
                .iter()
                .filter_map(WifiExecutorSlot::deadline)
                .chain(groups.iter().filter_map(|group| group.retry_at))
                .min();
            match executor_wait(ax_hal::time::monotonic_time_nanos(), deadline_nanos) {
                ExecutorWait::Notification => control.notify.wait(),
                ExecutorWait::Deadline(duration) => {
                    let _ = control.notify.wait_timeout(duration);
                }
                ExecutorWait::Ready => {}
            }
        }
    }
}

fn wait_for_cleanup_command(control: &ExecutorControl) -> bool {
    loop {
        if let Some(irq_synchronized) =
            requested_irq_synchronization(control.command.load(Ordering::Acquire))
        {
            return irq_synchronized;
        }
        control.notify.wait();
    }
}

fn release_executor_resources(
    groups: Vec<QueueGroupExecutor>,
    mut wifi: Vec<WifiExecutorSlot>,
    irq_synchronized: bool,
) {
    if irq_synchronized {
        for slot in &mut wifi {
            slot.cancel_active();
        }
        drop(wifi);
        shutdown_queue_groups(groups, true);
    } else {
        quarantine_executor_resources(groups, wifi);
    }
}

fn shutdown_queue_groups(mut groups: Vec<QueueGroupExecutor>, irq_synchronized: bool) {
    debug_assert!(irq_synchronized);
    let mut dma_stopped = true;
    for group in &mut groups {
        group.shared.disable();
        let _ = group.group.irq_control.quiesce();
        if group.group.irq_control.shutdown().is_err() {
            dma_stopped = false;
        }
    }
    if !dma_stopped {
        log::warn!(
            "quarantining {} network poll groups because DMA shutdown was not confirmed",
            groups.len()
        );
    }
    release_or_quarantine(
        groups,
        backing_can_be_released(irq_synchronized, dma_stopped),
    );
}

fn quarantine_executor_resources(groups: Vec<QueueGroupExecutor>, wifi: Vec<WifiExecutorSlot>) {
    for group in &groups {
        group.shared.disable();
    }
    for slot in &wifi {
        slot.abandon_active();
    }
    log::warn!(
        "quarantining {} network poll groups because IRQ callback synchronization was not \
         confirmed",
        groups.len()
    );
    core::mem::forget(groups);
    core::mem::forget(wifi);
}

pub(super) fn release_or_quarantine<T>(resource: T, dma_stopped: bool) {
    if dma_stopped {
        drop(resource);
    } else {
        core::mem::forget(resource);
    }
}

pub(super) const fn backing_can_be_released(irq_synchronized: bool, dma_stopped: bool) -> bool {
    irq_synchronized && dma_stopped
}

pub(super) const fn requested_irq_synchronization(command: u8) -> Option<bool> {
    match command {
        COMMAND_STOP => Some(true),
        COMMAND_QUARANTINE => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_executor_waits_only_for_notification_without_a_deadline() {
        assert_eq!(executor_wait(100, None), ExecutorWait::Notification);
    }

    #[test]
    fn retry_deadline_waits_for_exact_remaining_duration() {
        assert_eq!(
            executor_wait(100, Some(175)),
            ExecutorWait::Deadline(core::time::Duration::from_nanos(75))
        );
        assert_eq!(executor_wait(175, Some(175)), ExecutorWait::Ready);
    }
}
