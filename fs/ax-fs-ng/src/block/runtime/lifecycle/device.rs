use super::*;

impl DeviceInner {
    pub(super) fn quarantine_resources(&self) {
        let registrations = core::mem::take(&mut *self.irq_registrations.lock());
        let hctxs = core::mem::take(&mut *self.hctxs.lock());
        let queues = core::mem::take(&mut *self.detached_queues.lock());
        let channels = core::mem::take(&mut *self.cpu_channels.lock());
        let controller_thread = self.controller_thread.lock().take();
        let controller = Arc::clone(&self.controller);
        core::mem::forget(registrations);
        core::mem::forget(hctxs);
        core::mem::forget(queues);
        core::mem::forget(channels);
        core::mem::forget(controller_thread);
        core::mem::forget(controller);
    }

    pub(super) fn prepare_group_shutdown_local(&self) {
        {
            let mut gate = self.lifecycle_gate.lock();
            if gate.phase != DevicePhase::Stopped {
                gate.phase = DevicePhase::Stopping;
                gate.submission_ready_hctx_count = 0;
            }
        }
        self.state.store(DEVICE_STOPPED, Ordering::Release);
        self.accepting.store(false, Ordering::Release);
        self.notify_all_barrier_waiters();
        let channels = core::mem::take(&mut *self.cpu_channels.lock());
        for channel in channels {
            channel.channel.close();
        }
        let hctxs = self.hctxs.lock().clone();
        for hctx in hctxs {
            hctx.seal_submission_channels();
        }
    }

    pub(super) fn quiesce_hctxs_for_group(&self) {
        let hctxs = self.hctxs.lock().clone();
        quiesce_hctxs(&hctxs);
    }

    /// Completes member-local resource teardown after a shared group has
    /// already synchronized and stopped its physical IRQ owner.
    pub(super) fn finish_group_member_shutdown(&self) -> BlockResult {
        let hctxs = self.hctxs.lock().clone();
        let detached_queues = core::mem::take(&mut *self.detached_queues.lock());
        quiesce_hctxs(&hctxs);
        let result = Self::shutdown_queues_after_controller_stop(&hctxs, detached_queues)
            .map_err(BlockError::from);
        self.finish_terminal_teardown(result)
    }

    pub(super) fn join_controller_thread(&self) {
        let thread = self.controller_thread.lock().take();
        if let Some(thread) = thread {
            thread.join();
        }
    }

    pub(super) fn controller_terminal(&self) {
        self.mark_failed();
        self.controller.commands.close();
        if let Some(group_owner) = &self.group_owner {
            if let (Some(member_id), Some(owner)) = (self.member_id, group_owner.terminal_owner()) {
                owner.shutdown_from_member(member_id);
            }
            return;
        }
        self.shutdown_from_controller();
    }

    pub(super) fn mark_ready(&self) -> bool {
        let ready = {
            let mut gate = self.lifecycle_gate.lock();
            match gate.phase {
                DevicePhase::Starting => {
                    gate.phase = DevicePhase::Ready;
                    self.state.store(DEVICE_READY, Ordering::Release);
                    true
                }
                DevicePhase::Ready => true,
                DevicePhase::Failed | DevicePhase::Stopping | DevicePhase::Stopped => false,
            }
        };
        if ready {
            self.accepting.store(true, Ordering::Release);
            self.state_notification.notify();
        }
        ready
    }

    pub(super) fn mark_failed(&self) {
        let changed = {
            let mut gate = self.lifecycle_gate.lock();
            if matches!(gate.phase, DevicePhase::Starting | DevicePhase::Ready) {
                gate.phase = DevicePhase::Failed;
                self.state.store(DEVICE_FAILED, Ordering::Release);
                true
            } else {
                false
            }
        };
        self.accepting.store(false, Ordering::Release);
        if changed {
            self.state_notification.notify();
            self.notify_all_barrier_waiters();
        }
    }

    pub(super) fn select_cpu_channel(&self) -> Option<CpuSubmissionChannel> {
        let channels = self.cpu_channels.lock();
        if channels.is_empty() {
            return None;
        }
        let cpu = runtime_ops().ok()?.current_cpu();
        Some(channels[cpu % channels.len()].clone())
    }

    pub(super) fn selected_queue_info(&self) -> Option<QueueInfo> {
        self.select_cpu_channel().map(|channel| {
            let mut info = channel.hctx.info();
            info.device = self.published_device_info();
            info
        })
    }

    pub(super) fn published_device_info(&self) -> DeviceInfo {
        self.device_info.lock().published()
    }

    pub(super) fn enter_data_submissions(
        &self,
        count: usize,
        admission: SubmissionAdmission,
    ) -> Result<(), BlkError> {
        if count == 0 {
            return Err(BlkError::InvalidRequest);
        }
        loop {
            let blocked_by_flush = {
                let mut gate = self.lifecycle_gate.lock();
                if gate.phase != DevicePhase::Ready {
                    return Err(BlkError::Io);
                }
                if gate.flush_active {
                    true
                } else {
                    gate.active_data = gate
                        .active_data
                        .checked_add(count)
                        .ok_or(BlkError::InvalidRequest)?;
                    false
                }
            };
            if !blocked_by_flush {
                return Ok(());
            }
            {
                if admission.cannot_wait() {
                    return Err(BlkError::Retry);
                }
                self.data_gate_waiters
                    .wait_while(|| {
                        let gate = self.lifecycle_gate.lock();
                        gate.flush_active && gate.phase == DevicePhase::Ready
                    })
                    .map_err(|_| BlkError::Other("block runtime adapter is not installed"))?;
            }
        }
    }

    pub(super) fn begin_flush_barrier(
        &self,
        admission: SubmissionAdmission,
    ) -> Result<(), BlkError> {
        loop {
            let acquired = {
                let mut gate = self.lifecycle_gate.lock();
                if gate.phase != DevicePhase::Ready {
                    return Err(BlkError::Io);
                }
                if gate.flush_active {
                    false
                } else {
                    gate.flush_active = true;
                    true
                }
            };
            if acquired {
                break;
            }
            if admission.cannot_wait() {
                return Err(BlkError::Retry);
            }
            self.flush_gate_waiters
                .wait_while(|| {
                    let gate = self.lifecycle_gate.lock();
                    gate.flush_active && gate.phase == DevicePhase::Ready
                })
                .map_err(|_| BlkError::Other("block runtime adapter is not installed"))?;
        }
        while self.lifecycle_gate.lock().active_data != 0 {
            if self.lifecycle_gate.lock().phase != DevicePhase::Ready {
                let mut gate = self.lifecycle_gate.lock();
                gate.flush_active = false;
                drop(gate);
                self.notify_flush_gate_released();
                return Err(BlkError::Io);
            }
            if admission.cannot_wait() {
                let mut gate = self.lifecycle_gate.lock();
                gate.flush_active = false;
                drop(gate);
                self.notify_flush_gate_released();
                return Err(BlkError::Retry);
            }
            self.data_drain_waiters
                .wait_while(|| {
                    let gate = self.lifecycle_gate.lock();
                    gate.active_data != 0 && gate.phase == DevicePhase::Ready
                })
                .map_err(|_| BlkError::Other("block runtime adapter is not installed"))?;
        }
        Ok(())
    }

    pub(super) fn undo_submission_admission(&self, op: RequestOp, count: usize) {
        let mut notify_data = false;
        let mut notify_flush = false;
        {
            let mut gate = self.lifecycle_gate.lock();
            match op {
                RequestOp::Read | RequestOp::Write => {
                    gate.active_data = gate.active_data.saturating_sub(count);
                    notify_data = gate.active_data == 0 && gate.flush_active;
                }
                RequestOp::Flush => {
                    gate.flush_active = false;
                    notify_flush = true;
                }
            }
        }
        if notify_data {
            self.data_drain_waiters.notify_all();
        }
        if notify_flush {
            self.notify_flush_gate_released();
        }
    }

    pub(super) fn install_update(
        self: &Arc<Self>,
        update: &mut ControllerUpdate,
        controller: Arc<ControllerPort>,
    ) -> Result<Vec<usize>, BlkError> {
        let mut transaction = InstallUpdateTransaction::new(
            self,
            update.take_queues(),
            update.take_irq_endpoints(),
            update.take_device_info(),
            update.controller_state(),
            controller,
        );
        transaction.prepare()?;
        transaction.commit()
    }

    fn register_endpoint(
        &self,
        endpoint: IrqEndpoint,
        hctxs: &[Arc<Hctx>],
    ) -> Result<InstalledIrqRegistration, BlkError> {
        let source_id = endpoint.source_id();
        let queue_bits = endpoint.queue_bits();
        let valid_queue_bits = hctxs
            .iter()
            .fold(0u64, |bits, hctx| bits | (1u64 << hctx.id()));
        if queue_bits & !valid_queue_bits != 0 {
            return Err(BlkError::InvalidRequest);
        }
        let target_count = hctxs
            .iter()
            .filter(|hctx| queue_bits & (1u64 << hctx.id()) != 0)
            .count();
        let mut targets = Vec::new();
        let mut hctx_tokens = Vec::new();
        targets
            .try_reserve(target_count)
            .map_err(|_| BlkError::NoMemory)?;
        hctx_tokens
            .try_reserve(target_count)
            .map_err(|_| BlkError::NoMemory)?;
        for hctx in hctxs {
            if queue_bits & (1u64 << hctx.id()) != 0 {
                let (target, token) = hctx.prepare_irq_target(source_id);
                targets.push(target);
                hctx_tokens.push(token);
            }
        }
        if queue_bits != 0 && targets.is_empty() {
            return Err(BlkError::NotSupported);
        }
        let cpu = hctxs
            .iter()
            .find(|hctx| queue_bits & (1u64 << hctx.id()) != 0)
            .map_or(0, |hctx| hctx.cpu());
        let irq = self
            .irq_sources
            .iter()
            .find(|source| source.source_id == source_id)
            .map(|source| source.irq)
            .ok_or(BlkError::NotSupported)?;
        let (controller_target, controller_token) = self.controller.prepare_irq_target(source_id);
        let registration = register_block_irq(
            format!("{}/irq-{source_id}", self.name),
            irq,
            cpu,
            BlockIrqAction::new(endpoint.into_handler(), targets)
                .with_controller_target(controller_target),
        )
        .map_err(|_| BlkError::Io)?;
        info!(
            "block device {} IRQ source {} ({irq:?}) fixed to CPU {} for queue mask \
             {queue_bits:#x}",
            self.name, source_id, cpu
        );
        Ok(InstalledIrqRegistration {
            registration,
            hctx_tokens,
            controller_token,
        })
    }

    pub(super) fn group_irq_target(
        &self,
        member_id: usize,
        source_id: usize,
    ) -> Result<(GroupIrqMemberTarget, Vec<HctxIrqToken>, ControllerIrqToken), BlkError> {
        let hctxs = self.hctxs.lock();
        let mut targets = Vec::new();
        let mut hctx_tokens = Vec::new();
        targets
            .try_reserve(hctxs.len())
            .map_err(|_| BlkError::NoMemory)?;
        hctx_tokens
            .try_reserve(hctxs.len())
            .map_err(|_| BlkError::NoMemory)?;
        for hctx in hctxs.iter() {
            let (target, token) = hctx.prepare_irq_target(source_id);
            targets.push(target);
            hctx_tokens.push(token);
        }
        let (controller_target, controller_token) = self.controller.prepare_irq_target(source_id);
        Ok((
            GroupIrqMemberTarget::new(member_id, targets, Some(controller_target)),
            hctx_tokens,
            controller_token,
        ))
    }

    pub(super) fn reserve_group_irq_targets(&self, additional: usize) -> Result<(), BlkError> {
        let hctxs = self.hctxs.lock();
        for hctx in hctxs.iter() {
            hctx.reserve_irq_targets(additional)?;
        }
        self.controller.reserve_irq_targets(additional)
    }

    pub(super) fn first_hctx_cpu(&self) -> Option<usize> {
        self.hctxs.lock().first().map(|hctx| hctx.cpu())
    }

    pub(super) fn wait_until_ready(&self, timeout: Duration) -> Result<(), BlkError> {
        let deadline = wall_time().saturating_add(timeout);
        loop {
            match self.state.load(Ordering::Acquire) {
                DEVICE_READY => return Ok(()),
                DEVICE_FAILED | DEVICE_STOPPED => return Err(BlkError::Io),
                _ => {}
            }
            let now = wall_time();
            if now >= deadline {
                self.mark_failed();
                return Err(BlkError::Io);
            }
            if self.state_notification.wait_timeout(deadline - now)
                && self.state.load(Ordering::Acquire) != DEVICE_READY
            {
                self.mark_failed();
                return Err(BlkError::Io);
            }
        }
    }

    pub(super) fn online_smp(&self) -> Result<(), BlkError> {
        if self.state.load(Ordering::Acquire) != DEVICE_READY {
            return Err(BlkError::Io);
        }
        let cpus = runtime_ops()
            .map_err(|_| BlkError::Other("block runtime adapter is not installed"))?
            .online_cpu_count()
            .max(1);
        let target = cpus.min(self.max_io_queues).min(MAX_RUNTIME_HCTX);
        let state = match self.controller.call(ControllerEvent::OnlineSmp {
            target_queues: target,
        }) {
            Ok(state) => state,
            Err(error) => {
                self.shutdown();
                return Err(error);
            }
        };
        let ready_hctxs = self.lifecycle_gate.lock().submission_ready_hctx_count;
        if state == ControllerState::Ready && ready_hctxs >= target {
            info!(
                "block device {} online with {} hctxs across {} CPUs",
                self.name, ready_hctxs, cpus
            );
            Ok(())
        } else {
            Err(BlkError::Io)
        }
    }

    pub(super) fn shutdown(&self) -> usize {
        match self.shutdown_result() {
            Ok(count) => count,
            Err(error) => {
                warn!("block device {} shutdown failed: {error:?}", self.name);
                0
            }
        }
    }

    pub(super) fn shutdown_result(&self) -> BlockResult<usize> {
        if !self.claim_teardown()? {
            self.join_controller_thread();
            return self.terminal_teardown_error().map_or(Ok(0), Err);
        }
        self.notify_all_barrier_waiters();

        let hctxs = self.hctxs.lock().clone();
        for hctx in &hctxs {
            hctx.seal_submission_channels();
        }
        let cpu_channels = core::mem::take(&mut *self.cpu_channels.lock());
        for channel in cpu_channels {
            channel.channel.close();
        }
        let quiesce_state = match self.controller.call(ControllerEvent::QuiesceIrqs) {
            Ok(state) => state,
            Err(_) if self.controller.terminal_confirmed() => ControllerState::Shutdown,
            Err(error) => {
                quiesce_hctxs(&hctxs);
                return self.finish_teardown(Err(error.into()));
            }
        };

        let registrations = core::mem::take(&mut *self.irq_registrations.lock());
        let count = registrations.len();
        if let Err(error) = disable_registrations(&registrations) {
            *self.irq_registrations.lock() = registrations;
            quiesce_hctxs(&hctxs);
            return self.finish_teardown(Err(error));
        }
        drop(registrations);
        quiesce_hctxs(&hctxs);

        let controller_terminal = quiesce_state == ControllerState::Shutdown;
        if !controller_terminal {
            match self.controller.call(ControllerEvent::Shutdown) {
                Ok(ControllerState::Shutdown) => {}
                Ok(state) => {
                    warn!("block controller shutdown was not confirmed: {state:?}");
                    return self.finish_teardown(Err(BlockError::Io));
                }
                Err(_) if self.controller.terminal_confirmed() => {}
                Err(error) => {
                    return self.finish_teardown(Err(error.into()));
                }
            }
        }

        let detached_queues = core::mem::take(&mut *self.detached_queues.lock());
        let queue_result = Self::shutdown_queues_after_controller_stop(&hctxs, detached_queues);
        self.controller.commands.close();
        let thread = self.controller_thread.lock().take();
        if let Some(thread) = thread {
            thread.join();
        }
        self.finish_terminal_teardown(queue_result.map(|()| count).map_err(BlockError::from))
    }

    fn claim_teardown(&self) -> BlockResult<bool> {
        loop {
            let mut gate = self.lifecycle_gate.lock();
            if gate.teardown_in_progress {
                drop(gate);
                self.shutdown_waiters
                    .wait_while(|| self.lifecycle_gate.lock().teardown_in_progress)?;
                continue;
            }
            if gate.phase == DevicePhase::Stopped {
                return Ok(false);
            }
            self.begin_teardown(&mut gate);
            return Ok(true);
        }
    }

    pub(super) fn shutdown_from_controller(&self) {
        let mut gate = self.lifecycle_gate.lock();
        if gate.teardown_in_progress || gate.phase == DevicePhase::Stopped {
            return;
        }
        self.begin_teardown(&mut gate);
        drop(gate);
        self.notify_all_barrier_waiters();
        let hctxs = self.hctxs.lock().clone();
        for hctx in &hctxs {
            hctx.seal_submission_channels();
        }
        let channels = core::mem::take(&mut *self.cpu_channels.lock());
        for channel in channels {
            channel.channel.close();
        }
        let registrations = core::mem::take(&mut *self.irq_registrations.lock());
        let count = registrations.len();
        if let Err(error) = disable_registrations(&registrations) {
            *self.irq_registrations.lock() = registrations;
            quiesce_hctxs(&hctxs);
            let _ = self.finish_teardown(Err(error));
            return;
        }
        quiesce_hctxs(&hctxs);
        let detached_queues = core::mem::take(&mut *self.detached_queues.lock());
        let queue_result = Self::shutdown_queues_after_controller_stop(&hctxs, detached_queues);
        drop(registrations);
        let _ =
            self.finish_terminal_teardown(queue_result.map(|()| count).map_err(BlockError::from));
    }

    fn begin_teardown(&self, gate: &mut LifecycleGateState) {
        gate.phase = DevicePhase::Stopping;
        gate.submission_ready_hctx_count = 0;
        gate.teardown_in_progress = true;
        self.state.store(DEVICE_STOPPED, Ordering::Release);
        self.accepting.store(false, Ordering::Release);
    }

    fn finish_teardown(&self, result: BlockResult<usize>) -> BlockResult<usize> {
        {
            let mut gate = self.lifecycle_gate.lock();
            gate.teardown_in_progress = false;
            if result.is_ok() {
                gate.phase = DevicePhase::Stopped;
            }
        }
        self.shutdown_waiters.notify_all();
        result
    }

    fn finish_terminal_teardown<T>(&self, result: BlockResult<T>) -> BlockResult<T> {
        let error = result.as_ref().err().copied();
        let terminal_error = {
            let mut gate = self.lifecycle_gate.lock();
            gate.phase = DevicePhase::Stopped;
            gate.teardown_in_progress = false;
            if gate.terminal_teardown_error.is_none() {
                gate.terminal_teardown_error = error;
            }
            gate.terminal_teardown_error
        };
        self.shutdown_waiters.notify_all();
        terminal_error.map_or(result, Err)
    }

    pub(super) fn terminal_teardown_error(&self) -> Option<BlockError> {
        self.lifecycle_gate.lock().terminal_teardown_error
    }

    fn shutdown_queues_after_controller_stop(
        hctxs: &[Arc<Hctx>],
        detached_queues: Vec<Box<dyn HardwareQueue>>,
    ) -> Result<(), BlkError> {
        let hctx_result = stop_hctxs(hctxs);
        let detached_result = shutdown_detached_queues(detached_queues);
        hctx_result.and(detached_result)
    }
}

#[derive(Clone)]
pub(super) struct CpuSubmissionChannel {
    pub(super) hctx: Arc<Hctx>,
    pub(super) channel: Arc<BoundedChannel<Submission>>,
}

#[cfg(test)]
pub(super) fn create_cpu_channels(
    hctxs: &[Arc<Hctx>],
    online_cpus: usize,
) -> Result<Vec<CpuSubmissionChannel>, BlkError> {
    if hctxs.is_empty() || online_cpus == 0 {
        return Err(BlkError::InvalidRequest);
    }
    let mut channels = Vec::with_capacity(online_cpus);
    for cpu in 0..online_cpus {
        let hctx = Arc::clone(&hctxs[cpu % hctxs.len()]);
        let channel = hctx.add_submission_channel()?;
        channels.push(CpuSubmissionChannel { hctx, channel });
    }
    Ok(channels)
}

impl HctxObserver for DeviceInner {
    fn request_completed(&self, op: RequestOp, block_count: u32, result: Result<(), BlkError>) {
        let (notify_data, notify_flush) = {
            let mut gate = self.lifecycle_gate.lock();
            match op {
                RequestOp::Read | RequestOp::Write => {
                    gate.active_data = gate.active_data.saturating_sub(1);
                    (gate.active_data == 0 && gate.flush_active, false)
                }
                RequestOp::Flush => {
                    gate.flush_active = false;
                    (false, true)
                }
            }
        };
        match op {
            RequestOp::Read => {
                if result.is_ok() {
                    BLOCK_READS.fetch_add(1, Ordering::Relaxed);
                    BLOCK_SECTORS_READ.fetch_add(
                        sectors_for_blocks(
                            self.published_device_info().logical_block_size,
                            block_count,
                        ),
                        Ordering::Relaxed,
                    );
                }
            }
            RequestOp::Write => {
                if result.is_ok() {
                    BLOCK_WRITES.fetch_add(1, Ordering::Relaxed);
                    BLOCK_SECTORS_WRITTEN.fetch_add(
                        sectors_for_blocks(
                            self.published_device_info().logical_block_size,
                            block_count,
                        ),
                        Ordering::Relaxed,
                    );
                }
            }
            RequestOp::Flush => {}
        }
        if notify_data {
            self.data_drain_waiters.notify_all();
        }
        if notify_flush {
            self.notify_flush_gate_released();
        }
    }

    fn hctx_failed(&self, _hctx_id: usize, _error: BlkError) {
        self.mark_failed();
    }
}

pub(super) struct DeviceInfoEpoch {
    published: DeviceInfo,
    frozen: bool,
}

impl DeviceInfoEpoch {
    pub(super) const fn new(published: DeviceInfo) -> Self {
        Self {
            published,
            frozen: false,
        }
    }

    pub(super) const fn published(&self) -> DeviceInfo {
        self.published
    }

    pub(super) fn observe(&mut self, observed: DeviceInfo) -> Result<(), BlkError> {
        if self.frozen && observed != self.published {
            return Err(BlkError::InvalidRequest);
        }
        self.published = observed;
        Ok(())
    }

    pub(super) fn freeze(&mut self) {
        self.frozen = true;
    }
}

impl DeviceInner {
    fn notify_flush_gate_released(&self) {
        self.data_gate_waiters.notify_all();
        self.flush_gate_waiters.notify_one();
    }

    fn notify_all_barrier_waiters(&self) {
        self.data_gate_waiters.notify_all();
        self.flush_gate_waiters.notify_all();
        self.data_drain_waiters.notify_all();
    }
}

struct CleanupBundle {
    device: Arc<DeviceInner>,
    registrations: Vec<InstalledIrqRegistration>,
    prepared_hctxs: Vec<PreparedHctx>,
    channels: Vec<CpuSubmissionChannel>,
    queues: Vec<Box<dyn HardwareQueue>>,
}

impl CleanupBundle {
    fn from_transaction(transaction: &mut InstallUpdateTransaction) -> Self {
        Self {
            device: Arc::clone(&transaction.device),
            registrations: core::mem::take(&mut transaction.registrations),
            prepared_hctxs: core::mem::take(&mut transaction.prepared_hctxs),
            channels: transaction.new_cpu_channels.take().unwrap_or_default(),
            queues: core::mem::take(&mut transaction.queues),
        }
    }

    fn cleanup(mut self) -> Result<(), BlkError> {
        let irq_result = disable_registrations(&self.registrations);
        for channel in self.channels {
            channel.channel.close();
        }
        for prepared in self.prepared_hctxs {
            if let Some(queue) = prepared.abort() {
                self.queues.push(queue);
            }
        }

        if irq_result.is_err() {
            self.device
                .irq_registrations
                .lock()
                .extend(self.registrations);
        }
        let mut detached = self.device.detached_queues.lock();
        if detached.try_reserve(self.queues.len()).is_err() {
            core::mem::forget(self.queues);
            drop(detached);
            return Err(BlkError::NoMemory);
        }
        detached.extend(self.queues);
        drop(detached);
        if irq_result.is_err() {
            Err(BlkError::Io)
        } else {
            Ok(())
        }
    }
}

struct InstallUpdateTransaction {
    device: Arc<DeviceInner>,
    queues: Vec<Box<dyn HardwareQueue>>,
    endpoints: Vec<IrqEndpoint>,
    device_info: Option<DeviceInfo>,
    state: ControllerState,
    controller: Arc<ControllerPort>,
    existing_ready_hctx_count: usize,
    candidates: Vec<Arc<Hctx>>,
    prepared_hctxs: Vec<PreparedHctx>,
    registrations: Vec<InstalledIrqRegistration>,
    new_cpu_channels: Option<Vec<CpuSubmissionChannel>>,
    old_cpu_channels: Vec<CpuSubmissionChannel>,
    rearm_sources: Vec<usize>,
    activated: Vec<ActivatedHctx>,
}

impl InstallUpdateTransaction {
    fn new(
        device: &Arc<DeviceInner>,
        queues: Vec<Box<dyn HardwareQueue>>,
        endpoints: Vec<IrqEndpoint>,
        device_info: Option<DeviceInfo>,
        state: ControllerState,
        controller: Arc<ControllerPort>,
    ) -> Self {
        Self {
            device: Arc::clone(device),
            queues,
            endpoints,
            device_info,
            state,
            controller,
            existing_ready_hctx_count: 0,
            candidates: Vec::new(),
            prepared_hctxs: Vec::new(),
            registrations: Vec::new(),
            new_cpu_channels: None,
            old_cpu_channels: Vec::new(),
            rearm_sources: Vec::new(),
            activated: Vec::new(),
        }
    }

    fn prepare(&mut self) -> Result<(), BlkError> {
        let teardown_started = matches!(
            self.device.lifecycle_gate.lock().phase,
            DevicePhase::Stopping | DevicePhase::Stopped
        );
        if teardown_started && (!self.queues.is_empty() || !self.endpoints.is_empty()) {
            return Err(self.fail(BlkError::Io));
        }
        let online_cpus = runtime_ops()
            .map_err(|_| BlkError::Other("block runtime adapter is not installed"))?
            .online_cpu_count()
            .max(1);
        let queue_count = self.queues.len();
        if self
            .device
            .detached_queues
            .lock()
            .try_reserve(queue_count)
            .is_err()
        {
            core::mem::forget(core::mem::take(&mut self.queues));
            return Err(BlkError::NoMemory);
        }
        if self.device.hctxs.lock().try_reserve(queue_count).is_err() {
            return Err(self.fail(BlkError::NoMemory));
        }
        if self
            .device
            .irq_registrations
            .lock()
            .try_reserve(self.endpoints.len())
            .is_err()
        {
            return Err(self.fail(BlkError::NoMemory));
        }
        if self.prepared_hctxs.try_reserve(queue_count).is_err()
            || self.activated.try_reserve(queue_count).is_err()
            || self
                .registrations
                .try_reserve(self.endpoints.len())
                .is_err()
            || self
                .rearm_sources
                .try_reserve(self.endpoints.len())
                .is_err()
        {
            return Err(self.fail(BlkError::NoMemory));
        }
        self.existing_ready_hctx_count = self
            .device
            .lifecycle_gate
            .lock()
            .submission_ready_hctx_count;
        let existing = self.device.hctxs.lock();
        if self.existing_ready_hctx_count > existing.len() {
            drop(existing);
            return Err(self.fail(BlkError::Io));
        }
        let Some(candidate_count) = existing.len().checked_add(queue_count) else {
            drop(existing);
            return Err(self.fail(BlkError::InvalidRequest));
        };
        if self.candidates.try_reserve(candidate_count).is_err() {
            drop(existing);
            return Err(self.fail(BlkError::NoMemory));
        }
        self.candidates.extend(existing.iter().cloned());
        drop(existing);
        let observer_arc: Arc<dyn HctxObserver> = self.device.clone();
        let observer = Arc::downgrade(&observer_arc);
        let event_port: Arc<dyn ControllerEventPort> = self.controller.clone();
        while !self.queues.is_empty() {
            let queue = self.queues.remove(0);
            let queue_id = queue.id();
            if self.candidates.iter().any(|hctx| hctx.id() == queue_id)
                || self.prepared_hctxs.iter().any(|hctx| hctx.id() == queue_id)
            {
                self.queues.insert(0, queue);
                return Err(self.fail(BlkError::InvalidRequest));
            }
            let cpu = (self.candidates.len() + self.prepared_hctxs.len()) % online_cpus;
            match Hctx::prepare(queue, cpu, observer.clone(), Arc::clone(&event_port)) {
                Ok(hctx) => self.prepared_hctxs.push(hctx),
                Err(start_error) => {
                    let (error, queue) = start_error.into_parts();
                    self.queues.insert(0, queue);
                    return Err(self.fail(error));
                }
            }
        }
        self.candidates.extend(
            self.prepared_hctxs
                .iter()
                .map(|hctx| Arc::clone(hctx.hctx())),
        );

        let target_reservation = self.candidates.iter().try_for_each(|hctx| {
            let additional = self
                .endpoints
                .iter()
                .filter(|endpoint| endpoint.queue_bits() & (1u64 << hctx.id()) != 0)
                .count();
            hctx.reserve_irq_targets(additional)
        });
        if let Err(error) = target_reservation {
            return Err(self.fail(error));
        }
        if let Err(error) = self.controller.reserve_irq_targets(self.endpoints.len()) {
            return Err(self.fail(error));
        }

        for endpoint in core::mem::take(&mut self.endpoints) {
            self.rearm_sources.push(endpoint.source_id());
            match self.device.register_endpoint(endpoint, &self.candidates) {
                Ok(registration) => self.registrations.push(registration),
                Err(error) => return Err(self.fail(error)),
            }
        }
        let channels_installable = {
            let gate = self.device.lifecycle_gate.lock();
            !matches!(gate.phase, DevicePhase::Stopping | DevicePhase::Stopped)
        };
        let rebuild = channels_installable
            && self.state == ControllerState::Ready
            && !self.candidates.is_empty()
            && (self.candidates.len() != self.existing_ready_hctx_count
                || self.device.cpu_channels.lock().len() != online_cpus);
        if rebuild {
            let mut channels: Vec<CpuSubmissionChannel> = Vec::new();
            if channels.try_reserve(online_cpus).is_err() {
                return Err(self.fail(BlkError::NoMemory));
            }
            let candidate_count = self.candidates.len();
            for (index, hctx) in self.candidates.iter().enumerate() {
                let additional = online_cpus / candidate_count
                    + usize::from(index < online_cpus % candidate_count);
                if let Err(error) = hctx.reserve_submission_channels(additional) {
                    return Err(self.fail(error));
                }
            }
            for cpu in 0..online_cpus {
                let hctx = Arc::clone(&self.candidates[cpu % self.candidates.len()]);
                let channel = match hctx.new_submission_channel() {
                    Ok(channel) => channel,
                    Err(error) => {
                        for channel in channels {
                            channel.channel.close();
                        }
                        return Err(self.fail(error));
                    }
                };
                channels.push(CpuSubmissionChannel { hctx, channel });
            }
            self.new_cpu_channels = Some(channels);
        }
        for registration in &mut self.registrations {
            if registration.enable().is_err() {
                return Err(self.fail(BlkError::Io));
            }
        }
        Ok(())
    }

    fn commit(mut self) -> Result<Vec<usize>, BlkError> {
        let ready = self.state == ControllerState::Ready;
        let mut channels_rebuilt = false;
        {
            let mut gate = self.device.lifecycle_gate.lock();
            if matches!(gate.phase, DevicePhase::Stopping | DevicePhase::Stopped) {
                if !self.prepared_hctxs.is_empty() || !self.registrations.is_empty() {
                    drop(gate);
                    return Err(self.fail(BlkError::Io));
                }
                if self
                    .device_info
                    .is_some_and(|info| info != self.device.device_info.lock().published())
                {
                    drop(gate);
                    return Err(self.fail(BlkError::InvalidRequest));
                }
                drop(gate);
                return Ok(self.rearm_sources);
            }
            if gate.phase == DevicePhase::Failed && ready {
                drop(gate);
                return Err(self.fail(BlkError::Io));
            }
            if ready && self.candidates.is_empty() {
                drop(gate);
                return Err(self.fail(BlkError::Io));
            }
            if gate.submission_ready_hctx_count != self.existing_ready_hctx_count {
                drop(gate);
                return Err(self.fail(BlkError::Io));
            }
            if let Some(info) = self.device_info.take() {
                let observe_result = self.device.device_info.lock().observe(info);
                if let Err(error) = observe_result {
                    drop(gate);
                    return Err(self.fail(error));
                }
            }
            if ready {
                for hctx in &self.candidates {
                    hctx.freeze_queue_info();
                }
                self.device.device_info.lock().freeze();
            }

            self.device.hctxs.lock().extend(
                self.prepared_hctxs
                    .iter()
                    .map(|hctx| Arc::clone(hctx.hctx())),
            );
            if let Some(new_channels) = self.new_cpu_channels.take() {
                let old = core::mem::replace(&mut *self.device.cpu_channels.lock(), new_channels);
                self.old_cpu_channels = old;
                channels_rebuilt = true;
                let channels = self.device.cpu_channels.lock();
                for channel in channels.iter() {
                    channel
                        .hctx
                        .install_submission_channel_committed(Arc::clone(&channel.channel));
                }
            }
            self.device
                .irq_registrations
                .lock()
                .extend(self.registrations.drain(..));
            if ready {
                gate.phase = DevicePhase::Ready;
                gate.submission_ready_hctx_count = self.candidates.len();
                self.device.state.store(DEVICE_READY, Ordering::Release);
                self.device.accepting.store(true, Ordering::Release);
            }

            for prepared in self.prepared_hctxs.drain(..) {
                self.activated.push(prepared.activate());
            }
        }

        for channel in self.old_cpu_channels.drain(..) {
            channel.channel.close();
        }
        if channels_rebuilt {
            for hctx in &self.candidates {
                hctx.notify_submission_channels_changed();
            }
        }
        for activated in &self.activated {
            activated.notify_worker();
        }
        self.device.state_notification.notify();
        Ok(self.rearm_sources)
    }

    fn fail(&mut self, error: BlkError) -> BlkError {
        match CleanupBundle::from_transaction(self).cleanup() {
            Ok(()) => error,
            Err(cleanup_error) => cleanup_error,
        }
    }
}
