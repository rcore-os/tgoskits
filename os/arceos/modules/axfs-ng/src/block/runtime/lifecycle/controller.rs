use super::*;

pub(super) struct ControllerPort {
    pub(super) commands: BoundedChannel<ControllerCommand>,
    pub(super) notification: Arc<dyn BlockNotification>,
    pub(super) irq_latches: IrqMutex<Vec<Arc<ControllerIrqLatch>>>,
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
        self.commands
            .send(command, false)
            .map_err(|_| BlkError::Io)?;
        loop {
            if let Some(result) = reply.result.lock().take() {
                return result;
            }
            reply.notification.wait();
        }
    }

    pub(super) fn irq_target(&self, source_id: usize) -> ControllerIrqTarget {
        let latch = Arc::new(ControllerIrqLatch::new(source_id));
        self.irq_latches.lock().push(Arc::clone(&latch));
        ControllerIrqTarget::new(latch, Arc::clone(&self.notification))
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
    loop {
        let mut progressed = false;
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
            if !event.control.is_empty() {
                let _ = drive_controller_transition(
                    &mut *controller,
                    ControllerEvent::Irq(event.control),
                    device.upgrade(),
                    Arc::clone(&port),
                );
            }
            if event.needs_rearm {
                let _ = drive_controller_transition(
                    &mut *controller,
                    ControllerEvent::Rearm {
                        source_id: event.control.source_id(),
                    },
                    device.upgrade(),
                    Arc::clone(&port),
                );
            }
        }

        while let Some(command) = port.commands.try_recv() {
            progressed = true;
            let result = drive_controller_transition(
                &mut *controller,
                command.event,
                device.upgrade(),
                Arc::clone(&port),
            );
            if let Some(reply) = command.reply {
                reply.complete(result);
            }
            if command.event == ControllerEvent::Shutdown {
                return;
            }
        }
        if !progressed {
            port.notification.wait();
        }
    }
}

fn drive_controller_transition(
    controller: &mut dyn BlockController,
    mut event: ControllerEvent,
    device: Option<Arc<DeviceInner>>,
    port: Arc<ControllerPort>,
) -> Result<ControllerState, BlkError> {
    let deadline = wall_time().saturating_add(CONTROLLER_TRANSITION_TIMEOUT);
    loop {
        let mut update = match controller.advance(event) {
            Ok(update) => update,
            Err(error) => {
                warn!("block controller transition {event:?} failed: {error:?}");
                return Err(error);
            }
        };
        let state = update.controller_state();
        if let Some(device) = &device {
            let rearm_sources = match device.install_update(&mut update, Arc::clone(&port)) {
                Ok(sources) => sources,
                Err(error) => {
                    warn!("failed to install block controller update after {event:?}: {error:?}");
                    device.state.store(DEVICE_FAILED, Ordering::Release);
                    device.state_notification.notify();
                    return Err(error);
                }
            };
            for source_id in rearm_sources {
                let mut rearm = match controller.advance(ControllerEvent::Rearm { source_id }) {
                    Ok(update) => update,
                    Err(error) => {
                        warn!("failed to rearm block IRQ source {source_id}: {error:?}");
                        return Err(error);
                    }
                };
                if let Err(error) = device.install_update(&mut rearm, Arc::clone(&port)) {
                    warn!(
                        "failed to install block controller rearm update for source {source_id}: \
                         {error:?}"
                    );
                    device.state.store(DEVICE_FAILED, Ordering::Release);
                    device.state_notification.notify();
                    return Err(error);
                }
            }
        }
        if state != ControllerState::RegisterPending {
            return Ok(state);
        }
        if wall_time() >= deadline {
            return Err(BlkError::Io);
        }
        core::hint::spin_loop();
        event = ControllerEvent::RegisterRetry;
    }
}
