use super::*;

pub(super) struct ControllerPort {
    pub(super) commands: BoundedChannel<ControllerCommand>,
    pub(super) notification: Arc<dyn BlockNotification>,
    pub(super) irq_latches: IrqMutex<Vec<Arc<ControllerIrqLatch>>>,
    pub(super) terminal_confirmed: AtomicBool,
}

pub(super) struct ControllerCommand {
    event: ControllerEvent,
    reply: Option<ControllerReplySender>,
}

struct ControllerReply {
    result: IrqMutex<Option<Result<ControllerState, BlkError>>>,
    notification: Arc<dyn BlockNotification>,
}

struct ControllerReplySender {
    inner: Arc<ControllerReply>,
}

struct PendingTransition {
    retry_at: Duration,
    deadline: Duration,
    reply: Option<ControllerReplySender>,
    event: ControllerEvent,
    exit_on_complete: bool,
}

impl ControllerPort {
    pub(super) fn call(&self, event: ControllerEvent) -> Result<ControllerState, BlkError> {
        let notification = runtime_ops()
            .map_err(|_| BlkError::Other("block runtime adapter is not installed"))?
            .notification();
        let reply = Arc::new(ControllerReply {
            result: IrqMutex::new(None),
            notification,
        });
        let command = ControllerCommand {
            event,
            reply: Some(ControllerReplySender {
                inner: Arc::clone(&reply),
            }),
        };
        match self.commands.send(command, false) {
            Ok(()) => {}
            Err(SendError::Closed(_)) => {
                warn!("cannot send synchronous block controller event {event:?}: channel closed");
                return Err(BlkError::Io);
            }
            Err(SendError::Full(_)) => {
                warn!("cannot send synchronous block controller event {event:?}: channel full");
                return Err(BlkError::Io);
            }
        }
        loop {
            if let Some(result) = reply.result.lock().take() {
                return result;
            }
            reply.notification.wait();
        }
    }

    pub(super) fn prepare_irq_target(
        self: &Arc<Self>,
        source_id: usize,
    ) -> (ControllerIrqTarget, ControllerIrqToken) {
        let latch = Arc::new(ControllerIrqLatch::new(source_id));
        (
            ControllerIrqTarget::new(Arc::clone(&latch), Arc::clone(&self.notification)),
            ControllerIrqToken {
                port: Arc::clone(self),
                latch,
                committed: false,
            },
        )
    }

    pub(super) fn terminal_confirmed(&self) -> bool {
        self.terminal_confirmed.load(Ordering::Acquire)
    }

    pub(super) fn reserve_irq_targets(&self, additional: usize) -> Result<(), BlkError> {
        self.irq_latches
            .lock()
            .try_reserve(additional)
            .map_err(|_| BlkError::NoMemory)
    }

    fn confirm_terminal(&self) {
        self.terminal_confirmed.store(true, Ordering::Release);
        self.commands.close();
        fail_queued_commands(self);
    }
}

pub(super) struct ControllerIrqToken {
    port: Arc<ControllerPort>,
    latch: Arc<ControllerIrqLatch>,
    committed: bool,
}

impl ControllerIrqToken {
    pub(super) fn commit(&mut self) {
        if !self.committed {
            let mut latches = self.port.irq_latches.lock();
            debug_assert!(latches.len() < latches.capacity());
            latches.push(Arc::clone(&self.latch));
            self.committed = true;
        }
    }
}

impl Drop for ControllerIrqToken {
    fn drop(&mut self) {
        if self.committed {
            let mut latches = self.port.irq_latches.lock();
            if let Some(index) = latches
                .iter()
                .position(|latch| Arc::ptr_eq(latch, &self.latch))
            {
                latches.swap_remove(index);
            }
        }
    }
}

impl ControllerEventPort for ControllerPort {
    fn post(&self, event: ControllerEvent) {
        if let Err(SendError::Closed(_) | SendError::Full(_)) = self
            .commands
            .send(ControllerCommand { event, reply: None }, false)
        {
            warn!("lost block controller event after controller shutdown");
        }
    }

    fn call(&self, event: ControllerEvent) -> Result<ControllerState, BlkError> {
        ControllerPort::call(self, event)
    }
}

impl ControllerReplySender {
    fn complete(self, result: Result<ControllerState, BlkError>) {
        *self.inner.result.lock() = Some(result);
        self.inner.notification.notify();
    }
}

pub(super) fn run_controller(
    mut controller: Box<dyn BlockController>,
    port: Arc<ControllerPort>,
    device: Weak<DeviceInner>,
) {
    let mut irq_events = Vec::<LatchedControllerIrq>::new();
    let mut pending = None;
    loop {
        if port.commands.is_closed() {
            complete_pending_reply(&mut pending, Err(BlkError::Io));
            fail_queued_commands(&port);
            exit_controller(controller, &port);
            return;
        }
        let mut progressed = false;

        // Acknowledged IRQ state is observed before task-context commands and
        // register retries. This lets an IRQ resolve a transition even when its
        // retry timer expires concurrently.
        {
            let latches = port.irq_latches.lock();
            for latch in latches.iter() {
                let event = latch.take();
                if !event.control.is_empty() || event.needs_rearm {
                    irq_events.push(event);
                }
            }
        }
        for event in irq_events.drain(..) {
            progressed = true;
            if !event.control.is_empty()
                && apply_unsolicited_event(
                    &mut *controller,
                    ControllerEvent::Irq(event.control),
                    &device,
                    &port,
                    &mut pending,
                )
            {
                exit_controller(controller, &port);
                return;
            }
            if event.needs_rearm
                && apply_unsolicited_event(
                    &mut *controller,
                    ControllerEvent::Rearm {
                        source_id: event.control.source_id(),
                    },
                    &device,
                    &port,
                    &mut pending,
                )
            {
                exit_controller(controller, &port);
                return;
            }
        }

        // Commands, most importantly shutdown, take priority over an expired
        // register retry. Synchronous transitions are serialized; only
        // quiesce/shutdown may supersede an already pending caller.
        while let Some(mut command) = port.commands.try_recv() {
            progressed = true;
            if pending.is_some() && command.reply.is_some() {
                if matches!(
                    command.event,
                    ControllerEvent::QuiesceIrqs
                        | ControllerEvent::Watchdog { .. }
                        | ControllerEvent::Shutdown
                ) {
                    fail_pending_reply(&mut pending, BlkError::Io);
                } else {
                    command
                        .reply
                        .take()
                        .expect("checked synchronous command")
                        .complete(Err(BlkError::Retry));
                    continue;
                }
            }

            let exit_on_complete = command.event == ControllerEvent::Shutdown;
            let result = advance_controller_once(
                &mut *controller,
                command.event,
                device.upgrade(),
                Arc::clone(&port),
            );
            match result {
                Ok(ControllerState::RegisterPending { retry_after }) => {
                    schedule_pending(
                        &mut pending,
                        retry_after,
                        command.reply,
                        command.event,
                        exit_on_complete,
                    );
                }
                Ok(state) => {
                    let terminal = state == ControllerState::Shutdown;
                    let controller_originated_terminal = terminal
                        && !matches!(
                            command.event,
                            ControllerEvent::Shutdown | ControllerEvent::QuiesceIrqs
                        );
                    if terminal {
                        port.confirm_terminal();
                    }
                    if let Some(reply) = command.reply {
                        reply.complete(Ok(state));
                    }
                    complete_pending_reply(&mut pending, Ok(state));
                    if controller_originated_terminal && let Some(device) = device.upgrade() {
                        device.controller_terminal();
                    }
                    if terminal {
                        exit_controller(controller, &port);
                        return;
                    }
                }
                Err(error) => {
                    if let Some(reply) = command.reply {
                        reply.complete(Err(error));
                    }
                    complete_pending_reply(&mut pending, Err(error));
                    mark_device_failed(device.upgrade());
                    if exit_on_complete {
                        exit_controller(controller, &port);
                        return;
                    }
                }
            }
        }

        let now = wall_time();
        if let Some(current) = &pending {
            if now >= current.deadline {
                let exit = current.exit_on_complete;
                complete_pending_reply(&mut pending, Err(BlkError::TimedOut));
                mark_device_failed(device.upgrade());
                if exit {
                    exit_controller(controller, &port);
                    return;
                }
                continue;
            }
            if now >= current.retry_at {
                progressed = true;
                let current = pending
                    .take()
                    .expect("pending transition was inspected above");
                let result = advance_controller_once(
                    &mut *controller,
                    ControllerEvent::RegisterRetry,
                    device.upgrade(),
                    Arc::clone(&port),
                );
                match result {
                    Ok(ControllerState::RegisterPending { retry_after }) => {
                        reschedule_pending(&mut pending, current, retry_after);
                    }
                    Ok(state) => {
                        let exit = current.exit_on_complete;
                        let terminal = state == ControllerState::Shutdown;
                        if terminal {
                            port.confirm_terminal();
                        }
                        if let Some(reply) = current.reply {
                            reply.complete(Ok(state));
                        }
                        if terminal
                            && !matches!(
                                current.event,
                                ControllerEvent::Shutdown | ControllerEvent::QuiesceIrqs
                            )
                            && let Some(device) = device.upgrade()
                        {
                            device.controller_terminal();
                        }
                        if terminal || exit {
                            exit_controller(controller, &port);
                            return;
                        }
                    }
                    Err(error) => {
                        let exit = current.exit_on_complete;
                        if let Some(reply) = current.reply {
                            reply.complete(Err(error));
                        }
                        mark_device_failed(device.upgrade());
                        if exit {
                            exit_controller(controller, &port);
                            return;
                        }
                    }
                }
            }
        }

        if !progressed {
            if port.commands.is_closed() {
                continue;
            }
            match &pending {
                Some(current) => {
                    let now = wall_time();
                    let wake_at = current.retry_at.min(current.deadline);
                    if wake_at > now {
                        port.notification.wait_timeout(wake_at - now);
                    }
                }
                None => port.notification.wait(),
            }
        }
    }
}

fn apply_unsolicited_event(
    controller: &mut dyn BlockController,
    event: ControllerEvent,
    device: &Weak<DeviceInner>,
    port: &Arc<ControllerPort>,
    pending: &mut Option<PendingTransition>,
) -> bool {
    match advance_controller_once(controller, event, device.upgrade(), Arc::clone(port)) {
        Ok(ControllerState::RegisterPending { retry_after }) => {
            match pending.take() {
                Some(current) => reschedule_pending(pending, current, retry_after),
                None => schedule_pending(pending, retry_after, None, event, false),
            }
            false
        }
        Ok(state) => {
            let exit = pending
                .as_ref()
                .is_some_and(|current| current.exit_on_complete);
            let terminal = state == ControllerState::Shutdown;
            complete_pending_reply(
                pending,
                if terminal {
                    Err(BlkError::Io)
                } else {
                    Ok(state)
                },
            );
            if terminal {
                port.confirm_terminal();
                if let Some(device) = device.upgrade() {
                    device.controller_terminal();
                }
            }
            exit || terminal
        }
        Err(error) => {
            let exit = pending
                .as_ref()
                .is_some_and(|current| current.exit_on_complete);
            complete_pending_reply(pending, Err(error));
            mark_device_failed(device.upgrade());
            exit
        }
    }
}

fn exit_controller(controller: Box<dyn BlockController>, port: &ControllerPort) {
    port.commands.close();
    fail_queued_commands(port);
    if !port.terminal_confirmed() {
        // The controller may still own DMA-visible state. Keep it alive until
        // a later recovery path can prove that hardware no longer accesses it.
        core::mem::forget(controller);
    }
}

fn schedule_pending(
    pending: &mut Option<PendingTransition>,
    retry_after: Duration,
    reply: Option<ControllerReplySender>,
    event: ControllerEvent,
    exit_on_complete: bool,
) {
    let now = wall_time();
    let retry_after = nonzero_retry_delay(retry_after);
    *pending = Some(PendingTransition {
        retry_at: now.saturating_add(retry_after),
        deadline: now.saturating_add(CONTROLLER_TRANSITION_TIMEOUT),
        reply,
        event,
        exit_on_complete,
    });
}

fn reschedule_pending(
    pending: &mut Option<PendingTransition>,
    mut current: PendingTransition,
    retry_after: Duration,
) {
    current.retry_at = wall_time().saturating_add(nonzero_retry_delay(retry_after));
    *pending = Some(current);
}

fn nonzero_retry_delay(retry_after: Duration) -> Duration {
    if retry_after.is_zero() {
        Duration::from_micros(1)
    } else {
        retry_after
    }
}

fn complete_pending_reply(
    pending: &mut Option<PendingTransition>,
    result: Result<ControllerState, BlkError>,
) {
    if let Some(current) = pending.take()
        && let Some(reply) = current.reply
    {
        reply.complete(result);
    }
}

fn fail_pending_reply(pending: &mut Option<PendingTransition>, error: BlkError) {
    complete_pending_reply(pending, Err(error));
}

fn fail_queued_commands(port: &ControllerPort) {
    while let Some(mut command) = port.commands.try_recv() {
        if let Some(reply) = command.reply.take() {
            reply.complete(Err(BlkError::Io));
        }
    }
}

fn mark_device_failed(device: Option<Arc<DeviceInner>>) {
    if let Some(device) = device {
        device.mark_failed();
    }
}

fn advance_controller_once(
    controller: &mut dyn BlockController,
    event: ControllerEvent,
    device: Option<Arc<DeviceInner>>,
    port: Arc<ControllerPort>,
) -> Result<ControllerState, BlkError> {
    let mut update = match controller.advance(event) {
        Ok(update) => update,
        Err(error) => {
            warn!(
                "block controller {} transition {event:?} failed: {error:?}",
                controller.name()
            );
            return Err(error);
        }
    };
    let mut state = update.controller_state();
    if let Some(device) = &device {
        let rearm_sources = match device.install_update(&mut update, Arc::clone(&port)) {
            Ok(sources) => sources,
            Err(error) => {
                warn!("failed to install block controller update after {event:?}: {error:?}");
                mark_device_failed(Some(Arc::clone(device)));
                return Err(error);
            }
        };
        if state == ControllerState::Shutdown {
            return Ok(state);
        }
        for source_id in rearm_sources {
            let mut rearm = match controller.advance(ControllerEvent::Rearm { source_id }) {
                Ok(update) => update,
                Err(error) => {
                    warn!("failed to rearm block IRQ source {source_id}: {error:?}");
                    return Err(error);
                }
            };
            let rearm_state = rearm.controller_state();
            if let Err(error) = device.install_update(&mut rearm, Arc::clone(&port)) {
                warn!(
                    "failed to install block controller rearm update for source {source_id}: \
                     {error:?}"
                );
                mark_device_failed(Some(Arc::clone(device)));
                return Err(error);
            }
            if rearm_state == ControllerState::Shutdown {
                state = ControllerState::Shutdown;
                break;
            }
            if let ControllerState::RegisterPending {
                retry_after: rearm_retry,
            } = rearm_state
            {
                state = match state {
                    ControllerState::RegisterPending { retry_after } => {
                        ControllerState::RegisterPending {
                            retry_after: retry_after.min(rearm_retry),
                        }
                    }
                    _ => rearm_state,
                };
            }
        }
    }
    Ok(state)
}
