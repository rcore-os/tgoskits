use super::*;

impl DeviceInner {
    pub(super) fn mark_ready(&self) -> bool {
        // IRQ/controller updates race teardown. DEVICE_FAILED and
        // DEVICE_STOPPED are terminal and must never be revived by stale work.
        let ready = match self
            .state
            .try_update(Ordering::AcqRel, Ordering::Acquire, |state| match state {
                DEVICE_STARTING => Some(DEVICE_READY),
                DEVICE_READY | DEVICE_FAILED | DEVICE_STOPPED => None,
                _ => None,
            }) {
            Ok(_) => true,
            Err(state) => state == DEVICE_READY,
        };
        if ready {
            self.accepting.store(true, Ordering::Release);
            self.state_notification.notify();
        }
        ready
    }

    pub(super) fn mark_failed(&self) {
        self.accepting.store(false, Ordering::Release);
        if self
            .state
            .try_update(Ordering::AcqRel, Ordering::Acquire, |state| match state {
                DEVICE_STARTING | DEVICE_READY => Some(DEVICE_FAILED),
                DEVICE_FAILED | DEVICE_STOPPED => None,
                _ => None,
            })
            .is_ok()
        {
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
            info.device = *self.info.lock();
            info
        })
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
            while self.flush_active.load(Ordering::Acquire) {
                if !self.accepting.load(Ordering::Acquire) {
                    return Err(BlkError::Io);
                }
                if admission.cannot_wait() {
                    return Err(BlkError::Retry);
                }
                self.data_gate_waiters
                    .wait_while(|| {
                        self.flush_active.load(Ordering::Acquire)
                            && self.accepting.load(Ordering::Acquire)
                    })
                    .map_err(|_| BlkError::Other("block runtime adapter is not installed"))?;
            }
            if !self.accepting.load(Ordering::Acquire) {
                return Err(BlkError::Io);
            }
            self.active_data
                .try_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                    active.checked_add(count)
                })
                .map_err(|_| BlkError::InvalidRequest)?;
            if !self.flush_active.load(Ordering::Acquire) && self.accepting.load(Ordering::Acquire)
            {
                return Ok(());
            }
            let previous = self.active_data.fetch_sub(count, Ordering::AcqRel);
            if previous == count && self.flush_active.load(Ordering::Acquire) {
                self.data_drain_waiters.notify_all();
            }
            if !self.accepting.load(Ordering::Acquire) {
                return Err(BlkError::Io);
            }
        }
    }

    pub(super) fn begin_flush_barrier(
        &self,
        admission: SubmissionAdmission,
    ) -> Result<(), BlkError> {
        loop {
            if !self.accepting.load(Ordering::Acquire) {
                return Err(BlkError::Io);
            }
            if self
                .flush_active
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
            if admission.cannot_wait() {
                return Err(BlkError::Retry);
            }
            self.flush_gate_waiters
                .wait_while(|| {
                    self.flush_active.load(Ordering::Acquire)
                        && self.accepting.load(Ordering::Acquire)
                })
                .map_err(|_| BlkError::Other("block runtime adapter is not installed"))?;
        }
        while self.active_data.load(Ordering::Acquire) != 0 {
            if !self.accepting.load(Ordering::Acquire) {
                self.flush_active.store(false, Ordering::Release);
                self.notify_flush_gate_released();
                return Err(BlkError::Io);
            }
            if admission.cannot_wait() {
                self.flush_active.store(false, Ordering::Release);
                self.notify_flush_gate_released();
                return Err(BlkError::Retry);
            }
            self.data_drain_waiters
                .wait_while(|| {
                    self.active_data.load(Ordering::Acquire) != 0
                        && self.accepting.load(Ordering::Acquire)
                })
                .map_err(|_| BlkError::Other("block runtime adapter is not installed"))?;
        }
        Ok(())
    }

    pub(super) fn undo_submission_admission(&self, op: RequestOp, count: usize) {
        match op {
            RequestOp::Read | RequestOp::Write => {
                let previous = self.active_data.fetch_sub(count, Ordering::AcqRel);
                if previous == count && self.flush_active.load(Ordering::Acquire) {
                    self.data_drain_waiters.notify_all();
                }
            }
            RequestOp::Flush => {
                self.flush_active.store(false, Ordering::Release);
                self.notify_flush_gate_released();
            }
        }
    }

    pub(super) fn install_update(
        self: &Arc<Self>,
        update: &mut ControllerUpdate,
        controller: Arc<ControllerPort>,
    ) -> Result<Vec<usize>, BlkError> {
        let queues = update.take_queues();
        let endpoints = update.take_irq_endpoints();
        if let Some(info) = update.take_device_info() {
            *self.info.lock() = info;
        }
        let online_cpus = runtime_ops()
            .map_err(|_| BlkError::Other("block runtime adapter is not installed"))?
            .online_cpu_count()
            .max(1);
        let existing = self.hctxs.lock().clone();
        let observer: Arc<dyn HctxObserver> = self.clone();
        let observer = Arc::downgrade(&observer);
        let event_port: Arc<dyn ControllerEventPort> = controller.clone();
        let mut new_hctxs = Vec::new();

        for queue in queues {
            let queue_id = queue.id();
            if existing
                .iter()
                .chain(new_hctxs.iter())
                .any(|hctx| hctx.id() == queue_id)
            {
                self.retain_uninstalled_resources(new_hctxs, Vec::from([queue]));
                return Err(BlkError::InvalidRequest);
            }
            let cpu = (existing.len() + new_hctxs.len()) % online_cpus;
            match Hctx::start(queue, cpu, observer.clone(), Arc::clone(&event_port)) {
                Ok(hctx) => new_hctxs.push(hctx),
                Err(start_error) => {
                    let (error, queue) = start_error.into_parts();
                    self.retain_uninstalled_resources(new_hctxs, Vec::from([queue]));
                    return Err(error);
                }
            }
        }

        let mut candidates = existing;
        candidates.extend(new_hctxs.iter().cloned());
        let mut new_registrations = Vec::new();
        let mut rearm_sources = Vec::new();
        for endpoint in endpoints {
            rearm_sources.push(endpoint.source_id());
            match self.register_endpoint(endpoint, &candidates) {
                Ok(registration) => new_registrations.push(registration),
                Err(error) => {
                    disable_registrations(&new_registrations);
                    self.retain_uninstalled_resources(new_hctxs, Vec::new());
                    return Err(error);
                }
            }
        }
        for registration in &new_registrations {
            if registration.enable().is_err() {
                disable_registrations(&new_registrations);
                self.retain_uninstalled_resources(new_hctxs, Vec::new());
                return Err(BlkError::Io);
            }
        }
        let rebuild_cpu_channels = !candidates.is_empty()
            && (!new_hctxs.is_empty() || self.cpu_channels.lock().len() < online_cpus);
        let new_cpu_channels = if rebuild_cpu_channels {
            match create_cpu_channels(&candidates, online_cpus) {
                Ok(channels) => Some(channels),
                Err(error) => {
                    disable_registrations(&new_registrations);
                    self.retain_uninstalled_resources(new_hctxs, Vec::new());
                    return Err(error);
                }
            }
        } else {
            None
        };
        self.hctxs.lock().extend(new_hctxs);
        if let Some(new_cpu_channels) = new_cpu_channels {
            let old_channels = core::mem::replace(&mut *self.cpu_channels.lock(), new_cpu_channels);
            for channel in old_channels {
                channel.channel.close();
            }
        }
        self.irq_registrations.lock().extend(new_registrations);
        if update.controller_state() == ControllerState::Ready && !self.hctxs.lock().is_empty() {
            self.mark_ready();
        }
        Ok(rearm_sources)
    }

    fn retain_uninstalled_resources(
        &self,
        hctxs: Vec<Arc<Hctx>>,
        queues: Vec<Box<dyn HardwareQueue>>,
    ) {
        self.hctxs.lock().extend(hctxs);
        self.detached_queues.lock().extend(queues);
    }

    fn register_endpoint(
        &self,
        endpoint: IrqEndpoint,
        hctxs: &[Arc<Hctx>],
    ) -> Result<Box<dyn BlockIrqRegistration>, BlkError> {
        let source_id = endpoint.source_id();
        let queue_bits = endpoint.queue_bits();
        let mut targets: Vec<IrqTarget> = Vec::new();
        for hctx in hctxs {
            if queue_bits & (1u64 << hctx.id()) != 0 {
                targets.push(hctx.irq_target(source_id));
            }
        }
        let cpu = targets
            .first()
            .and_then(|_| {
                hctxs
                    .iter()
                    .find(|hctx| queue_bits & (1u64 << hctx.id()) != 0)
                    .map(|hctx| hctx.cpu())
            })
            .unwrap_or(0);
        let irq = self
            .irq_sources
            .iter()
            .find(|source| source.source_id == source_id)
            .map(|source| source.irq)
            .ok_or(BlkError::NotSupported)?;
        let controller_target = self.controller.irq_target(source_id);
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
        Ok(registration)
    }

    pub(super) fn group_irq_target(
        &self,
        member_id: usize,
        source_id: usize,
    ) -> GroupIrqMemberTarget {
        let hctxs = self.hctxs.lock();
        let targets = hctxs
            .iter()
            .map(|hctx| hctx.irq_target(source_id))
            .collect();
        GroupIrqMemberTarget::new(
            member_id,
            targets,
            Some(self.controller.irq_target(source_id)),
        )
    }

    pub(super) fn first_hctx_cpu(&self) -> Option<usize> {
        self.hctxs.lock().first().map(|hctx| hctx.cpu())
    }

    pub(super) fn prepare_group_shutdown(&self) {
        self.accepting.store(false, Ordering::Release);
        self.notify_all_barrier_waiters();
        let _ = self.controller.call(ControllerEvent::QuiesceIrqs);
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
        let current = self.hctxs.lock().len();
        if target <= current {
            self.ensure_cpu_channels(cpus)?;
            info!(
                "block device {} online with {} hctxs across {} CPUs",
                self.name, current, cpus
            );
            return Ok(());
        }
        let state = match self.controller.call(ControllerEvent::OnlineSmp {
            target_queues: target,
        }) {
            Ok(state) => state,
            Err(error) => {
                self.shutdown();
                return Err(error);
            }
        };
        if state == ControllerState::Ready && self.hctxs.lock().len() >= target {
            info!(
                "block device {} online with {} hctxs across {} CPUs",
                self.name,
                self.hctxs.lock().len(),
                cpus
            );
            Ok(())
        } else {
            Err(BlkError::Io)
        }
    }

    fn ensure_cpu_channels(&self, online_cpus: usize) -> Result<(), BlkError> {
        if self.cpu_channels.lock().len() >= online_cpus {
            return Ok(());
        }
        let hctxs = self.hctxs.lock().clone();
        let new_channels = create_cpu_channels(&hctxs, online_cpus)?;
        let old_channels = core::mem::replace(&mut *self.cpu_channels.lock(), new_channels);
        for channel in old_channels {
            channel.channel.close();
        }
        Ok(())
    }

    pub(super) fn shutdown(&self) -> usize {
        let previous = self.state.swap(DEVICE_STOPPED, Ordering::AcqRel);
        if previous == DEVICE_STOPPED {
            return 0;
        }
        self.accepting.store(false, Ordering::Release);
        self.notify_all_barrier_waiters();

        let quiesce_result = self.controller.call(ControllerEvent::QuiesceIrqs);
        let quiesce_confirmed_terminal = matches!(quiesce_result, Ok(ControllerState::Shutdown));
        let registrations = core::mem::take(&mut *self.irq_registrations.lock());
        let count = registrations.len();
        disable_registrations(&registrations);
        drop(registrations);

        let hctxs = core::mem::take(&mut *self.hctxs.lock());
        let detached_queues = core::mem::take(&mut *self.detached_queues.lock());
        let cpu_channels = core::mem::take(&mut *self.cpu_channels.lock());
        for channel in cpu_channels {
            channel.channel.close();
        }
        quiesce_hctxs(&hctxs);
        let shutdown_result = self.controller.call(ControllerEvent::Shutdown);
        let shutdown_confirmed_terminal = matches!(shutdown_result, Ok(ControllerState::Shutdown));
        let controller_stopped = quiesce_confirmed_terminal || shutdown_confirmed_terminal;
        if controller_stopped {
            stop_hctxs(&hctxs);
            drop(detached_queues);
        } else {
            warn!(
                "leaking {} block hctxs and {} detached queues because controller shutdown was \
                 not confirmed (quiesce={quiesce_result:?}, shutdown={shutdown_result:?})",
                hctxs.len(),
                detached_queues.len()
            );
            core::mem::forget(hctxs);
            core::mem::forget(detached_queues);
        }
        self.controller.commands.close();
        // Drop the IRQ-disabling slot guard before `join`, which may sleep.
        let thread = self.controller_thread.lock().take();
        if let Some(thread) = thread {
            thread.join();
        }
        count
    }
}

#[derive(Clone)]
pub(super) struct CpuSubmissionChannel {
    pub(super) hctx: Arc<Hctx>,
    pub(super) channel: Arc<BoundedChannel<Submission>>,
}

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
        match op {
            RequestOp::Read => {
                let previous = self.active_data.fetch_sub(1, Ordering::AcqRel);
                if result.is_ok() {
                    BLOCK_READS.fetch_add(1, Ordering::Relaxed);
                    BLOCK_SECTORS_READ.fetch_add(
                        sectors_for_blocks(self.info.lock().logical_block_size, block_count),
                        Ordering::Relaxed,
                    );
                }
                if previous == 1 && self.flush_active.load(Ordering::Acquire) {
                    self.data_drain_waiters.notify_all();
                }
            }
            RequestOp::Write => {
                let previous = self.active_data.fetch_sub(1, Ordering::AcqRel);
                if result.is_ok() {
                    BLOCK_WRITES.fetch_add(1, Ordering::Relaxed);
                    BLOCK_SECTORS_WRITTEN.fetch_add(
                        sectors_for_blocks(self.info.lock().logical_block_size, block_count),
                        Ordering::Relaxed,
                    );
                }
                if previous == 1 && self.flush_active.load(Ordering::Acquire) {
                    self.data_drain_waiters.notify_all();
                }
            }
            RequestOp::Flush => {
                self.flush_active.store(false, Ordering::Release);
                self.notify_flush_gate_released();
            }
        }
    }

    fn hctx_failed(&self, _hctx_id: usize, _error: BlkError) {
        self.mark_failed();
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
