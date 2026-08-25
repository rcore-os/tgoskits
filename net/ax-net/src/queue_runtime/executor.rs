use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicU8, Ordering};

use ax_sync::SpinLock;
use rd_net::{DmaBuffer, NetError, NetRearmResult, PreparedNetPollGroup, RxCompletion};

use super::{
    COMMAND_QUARANTINE, COMMAND_STOP, COMMAND_WAIT, CPU_ROUND_BUDGET, PollGroupState, QUEUE_BUDGET,
    STATE_MASK, STATE_MISSED, STATE_POLLING, STATE_SCHEDULED, STATUS_FAILED, STATUS_READY,
    SpscConsumer, SpscProducer, WifiControlQueue,
};
use crate::device::{EthernetFramePort, NetDeviceError, NetDeviceResult, ProtocolEthernetFrame};

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

    fn receive(&mut self) -> NetDeviceResult<ProtocolEthernetFrame> {
        if self.groups.is_empty() {
            return Err(NetDeviceError::Stopped);
        }
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
    pub(super) shared: Arc<PollGroupState>,
}

impl QueueGroupExecutor {
    fn initialize(&mut self) -> Result<(), NetError> {
        if let Some(mut startup) = self.group.owner_startup.take() {
            startup.initialize()?;
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
        let pending = matches!(
            self.group.irq_control.rearm_and_check()?,
            NetRearmResult::WorkPending(_)
        );
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
        if let Some(completion) = self.pending_rx.take() {
            match self.rx_ready.push(completion) {
                Ok(()) => crate::request_poll(),
                Err(completion) => {
                    self.pending_rx = Some(completion);
                    return GroupPollOutcome::Blocked(work);
                }
            }
        }
        if let Some(buffer) = self.pending_tx_free.take() {
            if let Err(buffer) = self.tx_free.push(buffer) {
                self.pending_tx_free = Some(buffer);
                return GroupPollOutcome::Blocked(work);
            }
            crate::request_poll();
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
        match self.group.irq_control.rearm_and_check() {
            Ok(NetRearmResult::Idle) => {}
            Ok(NetRearmResult::WorkPending(_)) => {
                self.shared.stats.rearm_race.fetch_add(1, Ordering::Relaxed);
                self.shared.schedule_task();
            }
            Err(_) => self.shared.disable(),
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

pub(super) struct WifiExecutorSlot {
    pub(super) group_index: usize,
    pub(super) control: Box<dyn rd_net::WifiControl>,
    pub(super) queue: Arc<WifiControlQueue>,
}

fn process_wifi_requests(groups: &mut [QueueGroupExecutor], wifi: &mut [WifiExecutorSlot]) -> bool {
    let mut handled = false;
    for slot in wifi {
        while let Some(request) = slot.queue.try_pop() {
            handled = true;
            let group = &mut groups[slot.group_index];
            let mut result = group.group.irq_control.quiesce();
            if result.is_ok() {
                result = slot.control.execute(request.transaction.operation());
            }

            match group.group.irq_control.rearm_and_check() {
                Ok(NetRearmResult::Idle) => {}
                Ok(NetRearmResult::WorkPending(_)) => group.shared.schedule_task(),
                Err(error) => {
                    group.shared.disable();
                    if result.is_ok() {
                        result = Err(error);
                    }
                }
            }
            request.completion.complete(result);
        }
    }
    handled
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

    let initialized = groups.iter_mut().all(|group| group.initialize().is_ok());
    control.startup_status.store(
        if initialized {
            STATUS_READY
        } else {
            STATUS_FAILED
        },
        Ordering::Release,
    );
    control.notify.notify();
    if !initialized {
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
        if !wifi.iter().any(|slot| slot.queue.has_pending())
            && !groups.iter().any(|group| {
                let state = group.shared.state.load(Ordering::Acquire);
                state & STATE_MASK == STATE_SCHEDULED
                    || (state & STATE_MASK == STATE_POLLING && state & STATE_MISSED != 0)
            })
        {
            control.notify.wait();
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
    wifi: Vec<WifiExecutorSlot>,
    irq_synchronized: bool,
) {
    if irq_synchronized {
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
