use alloc::{collections::VecDeque, string::String, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicU8, Ordering};

use ax_sync::SpinLock;
use rd_net::{
    DmaBuffer, NetError, NetOwnerStartupProgress, NetRearmResult, PreparedNetPollGroup,
    RxCompletion, TxChecksumCapabilities, TxSubmitOptions,
};

use super::{
    COMMAND_QUARANTINE, COMMAND_STOP, COMMAND_WAIT, CPU_ROUND_BUDGET, PollGroupState, QUEUE_BUDGET,
    STATE_MASK, STATE_MISSED, STATE_POLLING, STATE_SCHEDULED, STATUS_FAILED, STATUS_READY,
    SpscConsumer, SpscProducer, TxQueueDiscipline,
};
use crate::device::{
    ETH_ZLEN, EthernetFramePort, NetDeviceError, NetDeviceResult, ProtocolEthernetFrame,
    ProtocolRxFrame, RxBufferRecycler,
};

mod wifi;

#[cfg(test)]
mod queue_tests;

pub(super) use wifi::WifiExecutorSlot;
use wifi::process_wifi_requests;

pub(super) struct TxRequest {
    pub(super) buffer: DmaBuffer,
    pub(super) options: TxSubmitOptions,
}

struct RxRecycleState {
    producer: SpscProducer<DmaBuffer>,
    overflow: Vec<DmaBuffer>,
}

pub(super) struct RxRecycler {
    state: SpinLock<RxRecycleState>,
    shared: Arc<PollGroupState>,
}

impl RxRecycler {
    pub(super) fn new(
        producer: SpscProducer<DmaBuffer>,
        shared: Arc<PollGroupState>,
        capacity: usize,
    ) -> Self {
        Self {
            state: SpinLock::new(RxRecycleState {
                producer,
                overflow: Vec::with_capacity(capacity.max(QUEUE_BUDGET)),
            }),
            shared,
        }
    }

    #[cfg(test)]
    pub(super) fn flush_overflow(&self) {
        let mut state = self.state.lock_irqsave();
        while let Some(buffer) = state.overflow.pop() {
            if let Err(buffer) = state.producer.push(buffer) {
                state.overflow.push(buffer);
                break;
            }
        }
    }

    fn drain_into(
        &self,
        consumer: &mut SpscConsumer<DmaBuffer>,
        spares: &mut Vec<DmaBuffer>,
        budget: usize,
    ) -> usize {
        let mut state = self.state.lock_irqsave();
        let start = spares.len();
        while spares.len() - start < budget {
            if let Some(buffer) = consumer.pop() {
                spares.push(buffer);
            } else if let Some(buffer) = state.overflow.pop() {
                spares.push(buffer);
            } else {
                break;
            }
        }
        spares.len() - start
    }

    #[cfg(test)]
    pub(super) fn overflow_len(&self) -> usize {
        self.state.lock_irqsave().overflow.len()
    }
}

impl RxBufferRecycler for RxRecycler {
    fn recycle(&self, buffer: DmaBuffer) {
        let mut state = self.state.lock_irqsave();
        if let Err(buffer) = state.producer.push(buffer) {
            state.overflow.push(buffer);
        }
        drop(state);
        self.shared.schedule_task();
    }
}

pub(super) struct ProtocolGroupPort {
    pub(super) rx_ready: SpscConsumer<RxCompletion>,
    pub(super) rx_recycler: Arc<RxRecycler>,
    pub(super) tx_ready: SpscProducer<TxRequest>,
    pub(super) tx_free: SpscConsumer<DmaBuffer>,
    pub(super) tx_spares: Vec<DmaBuffer>,
    pub(super) shared: Arc<PollGroupState>,
}

impl ProtocolGroupPort {
    pub(super) fn receive_owned(&mut self) -> NetDeviceResult<ProtocolRxFrame> {
        let completion = self.rx_ready.pop().ok_or(NetDeviceError::Again)?;
        if completion.packet_len > completion.buffer.capacity() {
            self.rx_recycler.recycle(completion.buffer);
            return Err(NetDeviceError::Io);
        }
        Ok(ProtocolRxFrame::new(
            completion,
            Arc::clone(&self.rx_recycler) as Arc<dyn RxBufferRecycler>,
        ))
    }

    pub(super) fn receive(&mut self) -> NetDeviceResult<ProtocolEthernetFrame> {
        let frame = self.receive_owned()?;
        frame.read_with(ProtocolEthernetFrame::copy_from_slice)
    }

    pub(super) fn receive_with(
        &mut self,
        consume: &mut dyn FnMut(&[u8]) -> usize,
    ) -> NetDeviceResult<usize> {
        let frame = self.receive_owned()?;
        Ok(frame.read_with(consume))
    }

    pub(super) fn transmit_frame_with_options(
        &mut self,
        frame_len: usize,
        options: TxSubmitOptions,
        fill: &mut dyn FnMut(&mut [u8]),
    ) -> NetDeviceResult {
        let Some(mut buffer) = self.tx_spares.pop().or_else(|| self.tx_free.pop()) else {
            return Err(NetDeviceError::Again);
        };
        let tx_len = frame_len.max(ETH_ZLEN);
        if buffer.set_len(tx_len).is_err() {
            self.tx_spares.push(buffer);
            return Err(NetDeviceError::InvalidParam);
        }
        buffer.write_with_cpu(|target| {
            fill(&mut target[..frame_len]);
            target[frame_len..].fill(0);
        });
        if let Err(request) = self.tx_ready.push(TxRequest { buffer, options }) {
            self.tx_spares.push(request.buffer);
            return Err(NetDeviceError::Again);
        }
        self.shared.schedule_task();
        Ok(())
    }
}

pub(super) struct PendingProtocolTx {
    frame: ProtocolEthernetFrame,
    options: TxSubmitOptions,
}

pub(super) struct QueueFramePort {
    pub(super) name: String,
    pub(super) mac: Arc<SpinLock<[u8; 6]>>,
    pub(super) groups: Vec<ProtocolGroupPort>,
    /// Device-level policy for handling a busy transmit queue.
    pub(super) tx_queue_discipline: TxQueueDiscipline,
    /// Lazily allocated FIFO storage used only by `TxQueueDiscipline::Fifo`.
    pub(super) pending_tx: VecDeque<PendingProtocolTx>,
    pub(super) next_rx: usize,
    pub(super) next_tx: usize,
    pub(super) checksum_capabilities: TxChecksumCapabilities,
}

impl EthernetFramePort for QueueFramePort {
    fn device_name(&self) -> &str {
        &self.name
    }

    fn mac_address(&self) -> [u8; 6] {
        *self.mac.lock_irqsave()
    }

    fn checksum_capabilities(&self) -> TxChecksumCapabilities {
        self.checksum_capabilities
    }

    fn transmit(&mut self, frame: &ProtocolEthernetFrame) -> NetDeviceResult {
        self.transmit_frame_with_options(
            frame.packet_len(),
            TxSubmitOptions::default(),
            &mut |target| target.copy_from_slice(frame.packet()),
        )
    }

    fn transmit_frame_with_options(
        &mut self,
        frame_len: usize,
        options: TxSubmitOptions,
        fill: &mut dyn FnMut(&mut [u8]),
    ) -> NetDeviceResult {
        if self.groups.is_empty() {
            return Err(NetDeviceError::Stopped);
        }
        let TxQueueDiscipline::Fifo { max_frames } = self.tx_queue_discipline else {
            debug_assert!(self.pending_tx.is_empty());
            return self.try_transmit_with_options(frame_len, options, fill);
        };

        self.flush_pending_tx()?;
        if self.pending_tx.len() >= max_frames.get() {
            return Err(NetDeviceError::Again);
        }
        if self.pending_tx.is_empty() {
            let mut filled = false;
            match self.try_transmit_with_options(frame_len, options, &mut |target| {
                filled = true;
                fill(target);
            }) {
                Ok(()) => return Ok(()),
                Err(NetDeviceError::Again) if !filled => {}
                // A callback may own a one-shot packet builder. If publication
                // failed after filling, leave retry ownership with the caller.
                Err(error) => return Err(error),
            }
        }
        // Only a busy FIFO needs an inline frame. Available DMA tokens use
        // the same direct-fill path as NoQueue devices above.
        let mut frame = ProtocolEthernetFrame::new(frame_len)?;
        fill(frame.packet_mut());
        self.pending_tx
            .push_back(PendingProtocolTx { frame, options });
        Ok(())
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
    fn receive_owned(&mut self) -> NetDeviceResult<Option<ProtocolRxFrame>> {
        if self.groups.is_empty() {
            return Err(NetDeviceError::Stopped);
        }
        self.flush_pending_tx()?;
        for offset in 0..self.groups.len() {
            let index = (self.next_rx + offset) % self.groups.len();
            match self.groups[index].receive_owned() {
                Ok(frame) => {
                    self.next_rx = (index + 1) % self.groups.len();
                    return Ok(Some(frame));
                }
                Err(NetDeviceError::Again) => {}
                Err(error) => return Err(error),
            }
        }
        Err(NetDeviceError::Again)
    }

    fn receive_with(&mut self, consume: &mut dyn FnMut(&[u8]) -> usize) -> NetDeviceResult<usize> {
        if self.groups.is_empty() {
            return Err(NetDeviceError::Stopped);
        }
        self.flush_pending_tx()?;
        for offset in 0..self.groups.len() {
            let index = (self.next_rx + offset) % self.groups.len();
            match self.groups[index].receive_with(consume) {
                Ok(consumed) => {
                    self.next_rx = (index + 1) % self.groups.len();
                    return Ok(consumed);
                }
                Err(NetDeviceError::Again) => {}
                Err(error) => return Err(error),
            }
        }
        Err(NetDeviceError::Again)
    }
}

impl QueueFramePort {
    fn try_transmit_with_options(
        &mut self,
        frame_len: usize,
        options: TxSubmitOptions,
        fill: &mut dyn FnMut(&mut [u8]),
    ) -> NetDeviceResult {
        for offset in 0..self.groups.len() {
            let index = (self.next_tx + offset) % self.groups.len();
            let mut filled = false;
            let mut fill_once = |target: &mut [u8]| {
                filled = true;
                fill(target);
            };
            match self.groups[index].transmit_frame_with_options(frame_len, options, &mut fill_once)
            {
                Ok(()) => {
                    self.next_tx = (index + 1) % self.groups.len();
                    return Ok(());
                }
                Err(NetDeviceError::Again) if !filled => {}
                Err(error) => return Err(error),
            }
        }
        Err(NetDeviceError::Again)
    }

    fn flush_pending_tx(&mut self) -> NetDeviceResult {
        while let Some(request) = self.pending_tx.pop_front() {
            match self.try_transmit_with_options(
                request.frame.packet_len(),
                request.options,
                &mut |target| target.copy_from_slice(request.frame.packet()),
            ) {
                Ok(()) => {}
                Err(NetDeviceError::Again) => {
                    self.pending_tx.push_front(request);
                    break;
                }
                Err(error) => return Err(error),
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

pub(super) struct PendingRxRefill {
    completion: RxCompletion,
    replacement: DmaBuffer,
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
    pub(super) rx_recycler: Arc<RxRecycler>,
    pub(super) rx_spares: Vec<DmaBuffer>,
    pub(super) tx_ready: SpscConsumer<TxRequest>,
    pub(super) tx_free: SpscProducer<DmaBuffer>,
    pub(super) pending_rx: Option<RxCompletion>,
    pub(super) pending_rx_refill: VecDeque<PendingRxRefill>,
    pub(super) pending_tx: Option<TxRequest>,
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
            let request = match self.pending_tx.take().or_else(|| self.tx_ready.pop()) {
                Some(request) => request,
                None => break,
            };
            match self
                .group
                .tx
                .submit_with_options(request.buffer, request.options)
            {
                Ok(()) => {
                    submitted += 1;
                    work += 1;
                }
                Err(error) => {
                    let (buffer, reason) = error.into_parts();
                    if waits_for_hardware_event(&reason) {
                        self.pending_tx = Some(TxRequest {
                            buffer,
                            options: request.options,
                        });
                        if submitted > 0 {
                            self.group.tx.flush();
                        }
                        return hardware_retry_outcome(work);
                    }
                    if let Err(buffer) = self.tx_free.push(buffer) {
                        self.pending_tx_free = Some(buffer);
                        if submitted > 0 {
                            self.group.tx.flush();
                        }
                        return GroupPollOutcome::Blocked(work);
                    }
                }
            }
        }
        if submitted > 0 {
            self.group.tx.flush();
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

        let per_class = QUEUE_BUDGET.min(cpu_budget.saturating_sub(work));
        let recycled =
            self.rx_recycler
                .drain_into(&mut self.rx_recycle, &mut self.rx_spares, per_class);
        work += recycled;

        let rx_budget = QUEUE_BUDGET.min(cpu_budget.saturating_sub(work));
        let mut received = 0;
        let mut rx_refill_blocked = false;
        loop {
            while !rx_refill_blocked && work < cpu_budget {
                let Some(pending) = self.pending_rx_refill.pop_front() else {
                    break;
                };
                match self.group.rx.recycle(pending.replacement) {
                    Ok(()) => {
                        work += 1;
                        if let Err(completion) = self.rx_ready.push(pending.completion) {
                            self.pending_rx = Some(completion);
                            return GroupPollOutcome::Blocked(work);
                        }
                        crate::request_poll();
                    }
                    Err(error) => {
                        let (replacement, reason) = error.into_parts();
                        self.pending_rx_refill.push_front(PendingRxRefill {
                            completion: pending.completion,
                            replacement,
                        });
                        if !matches!(reason, NetError::Retry) {
                            self.shared.disable();
                            return GroupPollOutcome::Failed;
                        }
                        rx_refill_blocked = true;
                    }
                }
            }
            if work >= cpu_budget
                || received >= rx_budget
                || self.pending_rx_refill.len() >= self.group.rx.capacity()
            {
                break;
            }
            let Some(completion) = self.group.rx.reclaim() else {
                break;
            };
            received += 1;
            work += 1;
            let replacement = match self.rx_spares.pop() {
                Some(buffer) => buffer,
                None => match self.group.rx.allocate_replacement() {
                    Ok(buffer) => buffer,
                    Err(_) => {
                        self.shared.disable();
                        return GroupPollOutcome::Failed;
                    }
                },
            };
            // Retain completion ownership until its replacement is submitted.
            // Reclaim remains allowed while refill is blocked: software-backed
            // queues may need completion-ring space before they accept buffers.
            // The retained queue is bounded by the hardware RX capacity.
            self.pending_rx_refill.push_back(PendingRxRefill {
                completion,
                replacement,
            });
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
