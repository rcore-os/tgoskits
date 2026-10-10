//! One task owns each mutable vCPU backend and acknowledges explicit commands.

use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
};

use axvm_types::{GuestPhysAddr, NestedPagingConfig};

use crate::{
    AxVmError, AxVmResult, OperationId, VmOperation,
    arch::current::CurrentArch,
    architecture::{ArchOps, ops::RegisterCompletion},
    engine::{
        ArchitectureExitHandler, EngineOutcome, ExecutionEntry, ExitHandler, OwnedVcpuEngine,
        VcpuAction, VcpuTask,
    },
    guest_memory::{DecodeMemory, MemoryRevision},
    host::task::{StagedThread, ThreadHandle},
    identity::VcpuInstance,
    manager::ControlShared,
    runtime::hvc::GuestRequest,
    services::{RunServices, VcpuWait},
    sync::MutexExt,
    vcpu::VcpuSignals,
    vm::VCpu,
};

pub(crate) enum VcpuCommand {
    Park {
        operation: OperationId,
    },
    Resume {
        operation: OperationId,
    },
    Stop {
        operation: OperationId,
    },
    InstallRoot {
        operation: OperationId,
        root: NestedPagingConfig,
        revision: MemoryRevision,
        decode: Arc<DecodeMemory>,
    },
}

pub(crate) enum VcpuEvent {
    MemoryIdle {
        run: crate::RunId,
    },
    Started {
        instance: VcpuInstance,
        operation: OperationId,
    },
    Parked {
        instance: VcpuInstance,
        operation: OperationId,
    },
    Resumed {
        instance: VcpuInstance,
        operation: OperationId,
    },
    RootInstalled {
        instance: VcpuInstance,
        operation: OperationId,
    },
    FirstEntered {
        instance: VcpuInstance,
    },
    Exited {
        instance: VcpuInstance,
        outcome: VcpuExitOutcome,
        backend: Box<VCpu>,
    },
    /// Scheduler ordinary task-work witnessed the original stack becoming inactive.
    Retired {
        instance: VcpuInstance,
    },
}

pub(crate) enum VcpuExitOutcome {
    Stopped,
    CpuOff,
    Fault(AxVmError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VcpuOnError {
    AlreadyOn,
    OnPending,
    StartFailed,
}

pub(crate) struct CpuOnArgs {
    pub(crate) entry: GuestPhysAddr,
    pub(crate) argument: usize,
}

pub(crate) struct VcpuTaskOptions {
    pub(crate) cpu_on: Option<CpuOnArgs>,
    pub(crate) schedule_policy: crate::SchedulePolicy,
}

struct Mailbox {
    commands: Mutex<VecDeque<VcpuCommand>>,
    pending: AtomicBool,
}

/// Atomic progress only; retaining it keeps no backend or task resource alive.
#[derive(Default)]
pub(crate) struct VcpuProgress {
    pub(crate) entries: AtomicU64,
    pub(crate) parks: AtomicU64,
}

/// Task-side command port, absent from hardware entries and IRQ endpoints.
pub(crate) struct VcpuPort {
    pub(crate) instance: VcpuInstance,
    pub(crate) signals: Arc<VcpuSignals>,
    mailbox: Mailbox,
    queue: crate::HostWaitQueueHandle,
    pub(crate) progress: Arc<VcpuProgress>,
    run: Arc<crate::services::RunSignals>,
}

impl VcpuPort {
    fn new(
        instance: VcpuInstance,
        signals: Arc<VcpuSignals>,
        run: Arc<crate::services::RunSignals>,
    ) -> Arc<Self> {
        Arc::new(Self {
            instance,
            signals,
            run,
            mailbox: Mailbox {
                commands: Mutex::new(VecDeque::new()),
                pending: AtomicBool::new(false),
            },
            queue: crate::HostWaitQueueHandle::new(),
            progress: Arc::new(VcpuProgress::default()),
        })
    }

    pub(crate) fn send(&self, command: VcpuCommand) -> AxVmResult {
        match &command {
            VcpuCommand::Park { .. } | VcpuCommand::InstallRoot { .. } => {
                self.signals.close_entry()
            }
            VcpuCommand::Stop { .. } => self.signals.request_stop(),
            VcpuCommand::Resume { .. } => {}
        }
        {
            let mut commands = self.mailbox.commands.lock_unpoisoned();
            commands.push_back(command);
            self.mailbox.pending.store(true, Ordering::Release);
        }
        // No mailbox guard reaches a wake or host IPI.
        self.wake();
        self.run
            .kick(self.instance.vcpu_id)
            .map_err(|error| AxVmError::interrupt("kick vCPU command", format!("{error:?}")))
    }

    fn pop(&self) -> Option<VcpuCommand> {
        let mut commands = self.mailbox.commands.lock_unpoisoned();
        let command = commands.pop_front();
        self.mailbox
            .pending
            .store(!commands.is_empty(), Ordering::Release);
        command
    }

    fn wake(&self) {
        self.signals.request_unblock();
        crate::host::task::wait_queue_wake(&self.queue, u32::MAX);
    }

    fn wait_control(&self, entry: &ExecutionEntry<CurrentArch>) {
        crate::host::task::wait_queue_wait_until(&self.queue, || {
            self.signals.stop_requested()
                || self.mailbox.pending.load(Ordering::Acquire)
                || (self.signals.entry_is_open() && entry.admission.load(Ordering::Acquire))
        });
    }
}

struct ReplyWake {
    port: Arc<VcpuPort>,
    ready: Arc<AtomicBool>,
}
impl Wake for ReplyWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.ready.store(true, Ordering::Release);
        self.port.wake();
    }
}

/// Startup ownership remains recoverable until the staged task is activated.
pub(crate) struct PreparedVcpuThread {
    staged: Option<StagedThread>,
    task: ThreadHandle,
    transfer: Arc<Mutex<Option<VCpu>>>,
    pub(crate) port: Arc<VcpuPort>,
}

impl PreparedVcpuThread {
    pub(crate) fn thread_handle(&self) -> ThreadHandle {
        self.task.clone()
    }
    pub(crate) fn activate(mut self) {
        self.staged
            .take()
            .expect("startup was not cancelled")
            .activate()
            .detach();
    }
    pub(crate) fn abort(mut self) -> Result<VCpu, (AxVmError, Box<Self>)> {
        // Cancelling TASK_NEW never executes its entry. Retain its transfer and
        // management lease until the reaper has disposed the entry captures.
        drop(self.staged.take());
        if let Err(error) = self.task.clone().join() {
            return Err((AxVmError::host("cancel staged vCPU", error), Box::new(self)));
        }
        let backend = self.transfer.lock_unpoisoned().take();
        match backend {
            Some(backend) => Ok(backend),
            None => Err((
                AxVmError::invalid_state("recover staged vCPU", "startup ownership was consumed"),
                Box::new(self),
            )),
        }
    }
}

pub(crate) enum StartupOwnership {
    Backend(Box<VCpu>),
    Cancelled(Box<PreparedVcpuThread>),
}

pub(crate) fn prepare_vcpu_thread(
    backend: VCpu,
    instance: VcpuInstance,
    operation: OperationId,
    entry: ExecutionEntry<CurrentArch>,
    services: Arc<RunServices>,
    control: Arc<ControlShared>,
    options: VcpuTaskOptions,
) -> Result<PreparedVcpuThread, (Box<AxVmError>, StartupOwnership)> {
    let VcpuTaskOptions {
        cpu_on,
        schedule_policy,
    } = options;
    let signals = backend.run_state();
    let port = VcpuPort::new(instance, signals.clone(), entry.signals.clone());
    let transfer = Arc::new(Mutex::new(Some(backend)));
    let requested_affinity = transfer
        .lock_unpoisoned()
        .as_ref()
        .and_then(|backend| backend.phys_cpu_set());
    let available = crate::percpu::enabled_cpu_mask();
    let selected = requested_affinity.unwrap_or(available) & available;
    if selected == 0 {
        return Err((
            Box::new(AxVmError::invalid_config(
                "vCPU affinity has no enabled virtualization CPU",
            )),
            StartupOwnership::Backend(Box::new(
                transfer
                    .lock_unpoisoned()
                    .take()
                    .expect("unactivated vCPU retained"),
            )),
        ));
    }
    let affinity = crate::host::task::cpu_set_from_raw_bits(selected);
    let context = crate::task::VcpuTaskContext { instance };
    let extension = crate::task::attach(context, Arc::downgrade(&control));
    let mut builder = crate::host::task::builder(format!(
        "VM[{}]-vCPU[{}]",
        instance.run.vm().vm_id(),
        instance.vcpu_id
    ))
    .stack_size(0x40000)
    .extension(extension)
    .policy(schedule_policy);
    builder = builder.affinity(affinity);
    let task_port = port.clone();
    let task_transfer = transfer.clone();
    let prepared = match builder.prepare(move || {
        let backend = task_transfer
            .lock_unpoisoned()
            .take()
            .expect("one vCPU startup owner");
        run_owner(
            backend, task_port, operation, entry, services, control, cpu_on,
        );
    }) {
        Ok(prepared) => prepared,
        Err(error) => {
            return Err((
                Box::new(AxVmError::host("prepare vCPU owner", error)),
                StartupOwnership::Backend(Box::new(
                    transfer
                        .lock_unpoisoned()
                        .take()
                        .expect("unactivated vCPU retained"),
                )),
            ));
        }
    };
    let task = prepared.thread_handle();
    let staged = match prepared.stage() {
        Ok(staged) => staged,
        Err(error) => {
            // PreparedThread::drop queues cancellation. Keep both its task
            // handle and backend transfer until the control owner joins it.
            return Err((
                Box::new(AxVmError::host("stage vCPU owner", error)),
                StartupOwnership::Cancelled(Box::new(PreparedVcpuThread {
                    staged: None,
                    task,
                    transfer,
                    port,
                })),
            ));
        }
    };
    Ok(PreparedVcpuThread {
        task,
        staged: Some(staged),
        transfer,
        port,
    })
}

fn run_owner(
    mut backend: VCpu,
    port: Arc<VcpuPort>,
    operation: OperationId,
    entry: ExecutionEntry<CurrentArch>,
    services: Arc<RunServices>,
    control: Arc<ControlShared>,
    cpu_on: Option<CpuOnArgs>,
) {
    let initialized = (|| {
        if port.signals.stop_requested() {
            return Err(AxVmError::OperationCancelled { operation });
        }
        if let Some(args) = cpu_on {
            crate::arch::current::initialize_cpu_on(&mut backend, args.entry, args.argument)?;
        }
        if backend.state() == crate::VmVcpuState::Starting {
            backend.bind_after_cpu_on_or_rollback()?;
        } else {
            backend.bind()?;
        }
        // An inactive backend may still store the previous translation root.
        // Install this run's revision before startup is acknowledged or guest
        // entry is admitted, including activations after a memory update.
        backend.with_engine_scope(&entry.decode, &entry.signals, |backend| {
            backend.set_nested_page_table(entry.root)
        })?;
        backend.with_backend(|backend| CurrentArch::prepare_vcpu(backend, &entry.architecture))
    })();
    if let Err(mut error) = initialized {
        backend.rollback_cpu_on();
        if backend.state() == crate::VmVcpuState::Ready
            && let Err(retirement) = backend.unbind()
        {
            error = AxVmError::lifecycle_rollback(
                "retire failed vCPU initialization",
                error,
                retirement,
            );
        }
        control.post_event(VcpuEvent::Exited {
            instance: port.instance,
            outcome: VcpuExitOutcome::Fault(error),
            backend: Box::new(backend),
        });
        return;
    }
    let idle_wait = VcpuWait::new(
        port.instance,
        port.signals.clone(),
        services.signals().clone(),
        crate::HostWaitQueueHandle::new(),
    );
    let mut task = VcpuTask::new(
        OwnedVcpuEngine::<CurrentArch>::new(Box::new(backend)),
        entry,
        ArchitectureExitHandler::<CurrentArch>::new(port.instance.vcpu_id),
        services,
        port.signals.clone(),
    );
    let mut reply: Option<VmOperation<usize>> = None;
    let mut reply_request = None;
    let mut reply_action = None;
    let reply_ready = Arc::new(AtomicBool::new(false));
    let waker = Waker::from(Arc::new(ReplyWake {
        port: port.clone(),
        ready: reply_ready.clone(),
    }));
    let mut context = Context::from_waker(&waker);
    control.post_event(VcpuEvent::Started {
        instance: port.instance,
        operation,
    });
    // Hardware is initialized, but the control owner has not opened admission.
    let mut parked = true;
    let outcome = loop {
        if port.signals.stop_requested() {
            break VcpuExitOutcome::Stopped;
        }
        while let Some(command) = port.pop() {
            let result = match command {
                VcpuCommand::Park { operation } => {
                    parked = true;
                    task.engine
                        .vcpu_mut()
                        .with_backend(CurrentArch::suspend_vcpu)
                        .map(|()| {
                            port.progress.parks.fetch_add(1, Ordering::Relaxed);
                            control.post_event(VcpuEvent::Parked {
                                instance: port.instance,
                                operation,
                            });
                        })
                }
                VcpuCommand::Resume { operation } => task
                    .engine
                    .vcpu_mut()
                    .with_backend(CurrentArch::resume_vcpu)
                    .map(|()| {
                        if port.signals.open_entry() {
                            parked = false;
                            control.post_event(VcpuEvent::Resumed {
                                instance: port.instance,
                                operation,
                            });
                        }
                    }),
                VcpuCommand::InstallRoot {
                    operation,
                    root,
                    revision,
                    decode,
                } => {
                    parked = true;
                    task.engine
                        .vcpu_mut()
                        .with_backend(CurrentArch::suspend_vcpu)
                        .and_then(|()| {
                            task.engine
                                .install_root(&mut task.entry, root, revision, decode)
                        })
                        .map(|()| {
                            control.post_event(VcpuEvent::RootInstalled {
                                instance: port.instance,
                                operation,
                            });
                        })
                }
                VcpuCommand::Stop { operation } => {
                    let _operation = operation;
                    port.signals.request_stop();
                    Ok(())
                }
            };
            if let Err(error) = result {
                let (backend, retirement) = task.engine.into_backend();
                let error = match retirement {
                    Ok(()) => error,
                    Err(retirement) => AxVmError::lifecycle_rollback(
                        "retire failed vCPU command",
                        error,
                        retirement,
                    ),
                };
                control.post_event(VcpuEvent::Exited {
                    instance: port.instance,
                    outcome: VcpuExitOutcome::Fault(error),
                    backend,
                });
                return;
            }
        }
        if port.signals.stop_requested() {
            break VcpuExitOutcome::Stopped;
        }
        if let Some(pending) = &mut reply {
            reply_ready.store(false, Ordering::Release);
            match Pin::new(pending).poll(&mut context) {
                Poll::Ready(result) => {
                    reply = None;
                    let request = reply_request.take().expect("guest reply has its request");
                    match result {
                        Ok(0) if matches!(request, GuestRequest::CpuOff { .. }) => {
                            reply_action = Some(VcpuAction::CpuOff);
                        }
                        Ok(0) if matches!(request, GuestRequest::NestedFault { .. }) => {
                            task.completion = Some(Default::default())
                        }
                        Ok(0) if matches!(request, GuestRequest::Reset) => {
                            break VcpuExitOutcome::Stopped;
                        }
                        Ok(value) => {
                            task.completion = Some(RegisterCompletion::Return(value).into())
                        }
                        Err(error) => {
                            if matches!(request, GuestRequest::NestedFault { .. }) {
                                break VcpuExitOutcome::Fault(error);
                            }
                            if matches!(request, GuestRequest::Reset) {
                                break VcpuExitOutcome::Stopped;
                            }
                            warn!("guest control request failed: {error}");
                            task.completion = Some(RegisterCompletion::Return(usize::MAX).into());
                        }
                    }
                }
                Poll::Pending => {
                    // Still consume park/stop/root commands while the control
                    // request depends on an event handled by the same owner.
                    crate::host::task::wait_queue_wait_until(&port.queue, || {
                        port.signals.stop_requested()
                            || port.mailbox.pending.load(Ordering::Acquire)
                            || reply_ready.load(Ordering::Acquire)
                    });
                    continue;
                }
            }
        }
        let action = if let Some(action) = reply_action.take() {
            action
        } else {
            if parked || !task.entry.admission.load(Ordering::Acquire) {
                port.wait_control(&task.entry);
                continue;
            }
            let result = task
                .services
                .poll_devices(port.instance.vcpu_id, false)
                .and_then(|()| {
                    task.run_once()
                        .map_err(|error| AxVmError::vcpu("run owned vCPU", error))
                })
                .and_then(|outcome| match outcome {
                    EngineOutcome::Interrupted => Ok(None),
                    EngineOutcome::Exit(exit) => {
                        let count = port.progress.entries.fetch_add(1, Ordering::Relaxed);
                        if count == 0 {
                            control.post_event(VcpuEvent::FirstEntered {
                                instance: port.instance,
                            });
                        }
                        task.exits.handle(exit, &task.services).map(Some)
                    }
                });
            match result {
                Ok(Some(action)) => action,
                Ok(None) => continue,
                Err(error) => break VcpuExitOutcome::Fault(error),
            }
        };
        match action {
            VcpuAction::Reenter(value) => task.completion = Some(value),
            VcpuAction::Wait(reason) => {
                if let Some(value) = reason.return_value
                    && let Err(error) = task
                        .engine
                        .commit_only(&task.entry, RegisterCompletion::Return(value).into())
                {
                    break VcpuExitOutcome::Fault(error);
                }
                if let Err(error) = task.services.poll_devices(port.instance.vcpu_id, true) {
                    break VcpuExitOutcome::Fault(error);
                }
                let vcpu_id = port.instance.vcpu_id;
                let result = task.engine.vcpu_mut().with_backend(|backend| {
                    CurrentArch::wait_for_event(
                        backend,
                        vcpu_id,
                        &task.entry.architecture,
                        &idle_wait,
                    )
                });
                if let Err(error) = result {
                    break VcpuExitOutcome::Fault(error);
                }
            }
            VcpuAction::Control(request) => match control.request_guest(port.instance, request) {
                Ok(operation) => {
                    reply = Some(operation);
                    reply_request = Some(request);
                }
                Err(error) => break VcpuExitOutcome::Fault(error),
            },
            VcpuAction::CpuOff => break VcpuExitOutcome::CpuOff,
            VcpuAction::Stop(reason) => {
                let _result = control.request_run_stop(port.instance.run, reason);
                break VcpuExitOutcome::Stopped;
            }
        }
        if matches!(
            crate::host::task::current_thread().effective_policy(),
            crate::host::task::SchedulePolicy::Fifo { .. }
        ) {
            crate::host::task::yield_now();
        }
    };
    let (backend, retired) = task.engine.into_backend();
    let outcome = match retired {
        Ok(()) => outcome,
        Err(error) => VcpuExitOutcome::Fault(error),
    };
    control.post_event(VcpuEvent::Exited {
        instance: port.instance,
        outcome,
        backend,
    });
}
