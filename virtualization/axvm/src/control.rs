//! One task owns lifecycle transitions, backend transfers and resource retirement.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use crate::{
    AxVmError, AxVmResult, OperationId, RunId, VmStatus, VmVcpuState,
    arch::current::{self, CurrentArch},
    architecture::ArchOps,
    boot::{BootImageProvider, PreparedGuestBoot},
    engine::ExecutionEntry,
    guest_memory::{DecodeMemory, GuestMemoryPort, MemoryRevision},
    identity::{VcpuInstance, next_generation},
    manager::{
        Command, ControlExit, ControlMessage, ControlShared, CpuObservation, DeviceObservation,
        MemoryObservation, VmConfigSnapshot, VmCreatePlan, VmHandle, VmSnapshot,
    },
    operation::OperationCompletion,
    runtime::{
        hvc::HyperCallAbi,
        vcpus::{
            CpuOnArgs, StartupOwnership, VcpuCommand, VcpuEvent, VcpuExitOutcome, VcpuPort,
            prepare_vcpu_thread,
        },
    },
    services::{DevicePorts, RunServices, RunSignalWorker, RunSignals},
    vm::{AxVM, VcpuSnapshot},
};

mod confirmations;
mod memory;
use confirmations::ConfirmationReceipt;
mod requests;

struct Participant {
    instance: VcpuInstance,
    task: crate::ThreadHandle,
    port: Arc<VcpuPort>,
    startup: OperationId,
    started: bool,
    parked: ConfirmationReceipt,
    resumed: ConfirmationReceipt,
    root_installed: ConfirmationReceipt,
    returned: bool,
    retired: bool,
    cpu_off: bool,
}

struct StartupReply {
    operation: OperationId,
    completion: OperationCompletion<usize>,
    abi: HyperCallAbi,
    args: CpuOnArgs,
}

/// No task-service mutex or mutable backend is reachable from this run's entry.
struct RunState {
    id: RunId,
    signals: Arc<RunSignals>,
    worker: Option<RunSignalWorker>,
    ports: Arc<DevicePorts>,
    services: Arc<RunServices>,
    architecture: Arc<<CurrentArch as ArchOps>::Entry>,
    admission: Arc<AtomicBool>,
    revision: MemoryRevision,
    decode: Arc<DecodeMemory>,
    participants: BTreeMap<usize, Participant>,
    cancelled_startups: Vec<(usize, Box<crate::runtime::vcpus::PreparedVcpuThread>)>,
    startup_replies: BTreeMap<usize, StartupReply>,
    activations: Vec<u64>,
    joined_activations: Vec<u64>,
    topology: Vec<(usize, u64)>,
    failure: Option<AxVmError>,
    pending_memory: Option<memory::PreparedMemoryUpdate>,
    memory_operation: Option<OperationId>,
    retired_entries: u64,
    retired_parks: u64,
}

#[derive(Clone, Copy)]
enum Confirmation {
    Started,
    Parked,
    Resumed,
    RootInstalled,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConfirmationProgress {
    Pending,
    Complete,
    Cancelled,
}

impl RunState {
    fn confirmation(
        &self,
        instance: VcpuInstance,
        operation: OperationId,
        confirmation: Confirmation,
    ) -> ConfirmationProgress {
        if instance.run != self.id {
            return ConfirmationProgress::Cancelled;
        }
        let allows_retirement = matches!(
            confirmation,
            Confirmation::Parked | Confirmation::RootInstalled
        );
        // An absent participant is quiet only after this exact activation was
        // joined. A late event from an older activation cannot satisfy a wait.
        if self.joined_activations.get(instance.vcpu_id) == Some(&instance.activation) {
            return if allows_retirement {
                ConfirmationProgress::Complete
            } else {
                ConfirmationProgress::Cancelled
            };
        }
        let Some(member) = self
            .participants
            .get(&instance.vcpu_id)
            .filter(|member| member.instance == instance)
        else {
            return ConfirmationProgress::Cancelled;
        };
        let confirmed = match confirmation {
            Confirmation::Started => member.started && member.startup == operation,
            Confirmation::Parked => member.parked.completed(instance, operation),
            Confirmation::Resumed => member.resumed.completed(instance, operation),
            Confirmation::RootInstalled => member.root_installed.completed(instance, operation),
        };
        if confirmed {
            ConfirmationProgress::Complete
        } else if member.returned && !allows_retirement {
            ConfirmationProgress::Cancelled
        } else {
            ConfirmationProgress::Pending
        }
    }
}

struct Owner {
    shared: Arc<ControlShared>,
    vm: AxVM,
    boot: PreparedGuestBoot,
    images: Arc<dyn BootImageProvider + Send + Sync>,
    state: VmStatus,
    run: Option<RunState>,
    run_generation: u64,
    last_run: Option<RunId>,
    current_operation: Option<OperationId>,
    last_failure: Option<AxVmError>,
    last_stop_reason: Option<crate::StopReason>,
    deferred: VecDeque<Command>,
    guest_stop: Option<RunId>,
    preparing_ports: Option<Arc<DevicePorts>>,
    ivc_bindings: Vec<crate::runtime::ivc::IvcBinding>,
}

pub(crate) fn run(
    shared: Arc<ControlShared>,
    exit: Arc<ControlExit>,
    plan: VmCreatePlan,
    creation: OperationCompletion<VmHandle>,
) {
    creation.accept();
    let first_run = RunId::new(shared.key(), 1);
    let signals = match RunSignals::new(first_run, plan.config.phys_cpu_ls.cpu_num()) {
        Ok(signals) => signals,
        Err(error) => {
            creation.finish(Err(error.clone()));
            shared.close_for_destroy(&exit);
            exit.record_creation_failure(error);
            return;
        }
    };
    let ports = DevicePorts::new(signals.clone(), Arc::downgrade(&shared));
    let vm = match AxVM::new(plan.config, ports.access_ports()) {
        Ok(vm) => vm,
        Err(error) => {
            creation.finish(Err(error.clone()));
            shared.close_for_destroy(&exit);
            exit.record_creation_failure(error);
            return;
        }
    };
    let mut owner = Owner {
        shared: shared.clone(),
        vm,
        boot: plan.boot,
        images: plan.images,
        state: VmStatus::Ready,
        run: None,
        run_generation: 1,
        last_run: None,
        current_operation: Some(creation.id()),
        last_failure: None,
        last_stop_reason: None,
        deferred: VecDeque::new(),
        guest_stop: None,
        preparing_ports: Some(ports.clone()),
        ivc_bindings: Vec::new(),
    };
    match owner.prepare_run(first_run, signals, ports) {
        Ok(()) => {
            owner.current_operation = None;
            owner.publish();
            creation.finish(Ok(shared.handle()));
        }
        Err(error) => {
            // The owner and reservation remain available for a retrying destroy.
            // In particular, failed device teardown cannot release live backing.
            owner.record_failure(error.clone());
            owner.current_operation = None;
            owner.publish();
            creation.finish(Err(error));
        }
    }
    loop {
        if let Some(expected) = owner.guest_stop {
            if owner.run.as_ref().is_none_or(|run| run.id != expected) {
                owner.guest_stop = None;
            } else {
                match shared.new_operation::<()>() {
                    Ok((observer, completion)) => {
                        owner.guest_stop = None;
                        drop(observer);
                        completion.accept();
                        let result = owner.stop(completion.id());
                        owner.finish(result, completion);
                    }
                    Err(error) => {
                        // Identity exhaustion cannot justify entering the guest
                        // or retiring unacknowledged resources. Freeze execution
                        // and retain the request and all owned resources.
                        owner.record_failure(error);
                        owner.publish();
                    }
                }
            }
        }
        let message = owner
            .deferred
            .pop_front()
            .map(ControlMessage::Command)
            .or_else(|| shared.next_message());
        match message {
            Some(ControlMessage::Command(command)) => {
                if owner.command(command, &exit) {
                    break;
                }
            }
            Some(message) => owner.internal(message),
            None => break,
        }
    }
    // A successful destroy reaches this point only after every external user of
    // the VM's resources retired. The task extension publishes completion after
    // this owner and its original stack have been retired by the scheduler.
}

impl Owner {
    fn prepare_run(
        &mut self,
        id: RunId,
        signals: Arc<RunSignals>,
        ports: Arc<DevicePorts>,
    ) -> AxVmResult {
        self.preparing_ports = Some(ports.clone());
        self.vm.replace_access_ports(ports.access_ports());
        self.vm.clear_boot_payload();
        let memory = self.vm.prepare_memory_layout()?;
        self.boot
            .clone()
            .load_images(memory.main_memory().clone(), &mut self.vm, &*self.images)?;
        self.vm.prepare()?;
        let revision = MemoryRevision {
            run: id,
            sequence: 1,
        };
        let weak = Arc::downgrade(&self.shared);
        let memory = GuestMemoryPort::new(
            revision,
            self.vm.resources.memory_leases.clone(),
            move || {
                if let Some(shared) = weak.upgrade() {
                    shared.post_event(VcpuEvent::MemoryIdle { run: id });
                }
            },
        );
        crate::arch::current::prepare_task_services(&self.vm.resources, memory.clone())?;
        let architecture = Arc::new(CurrentArch::prepare_entry(
            &self.vm.resources,
            signals.clone(),
        )?);
        let topology = self.vm.get_vcpu_guest_mpidrs();
        let count = self.vm.config().phys_cpu_ls.cpu_num();
        let services = Arc::new(RunServices::new(
            id,
            self.vm.get_devices()?,
            memory,
            signals.clone(),
        ));
        self.run = Some(RunState {
            id,
            signals,
            worker: None,
            ports,
            services,
            architecture,
            admission: Arc::new(AtomicBool::new(false)),
            revision,
            decode: Arc::new(DecodeMemory::new(self.vm.resources.memory_leases.clone())),
            participants: BTreeMap::new(),
            cancelled_startups: Vec::new(),
            startup_replies: BTreeMap::new(),
            activations: vec![0; count],
            joined_activations: vec![0; count],
            topology,
            failure: None,
            pending_memory: None,
            memory_operation: None,
            retired_entries: 0,
            retired_parks: 0,
        });
        self.preparing_ports = None;
        Ok(())
    }

    fn command(&mut self, command: Command, exit: &ControlExit) -> bool {
        let operation = command.id();
        self.current_operation = Some(operation);
        self.publish();
        let destroyed = match command {
            Command::Start(completion) => {
                if matches!(
                    self.state,
                    VmStatus::Ready | VmStatus::Stopped | VmStatus::Running
                ) {
                    completion.accept();
                    let result = self.start(operation);
                    self.finish(result, completion);
                } else {
                    completion.reject(self.transition_error(VmStatus::Running, "start"));
                }
                false
            }
            Command::Pause(completion) => {
                if matches!(self.state, VmStatus::Running | VmStatus::Paused) {
                    completion.accept();
                    let result = self.pause(operation);
                    self.finish(result, completion);
                } else {
                    completion.reject(self.transition_error(VmStatus::Paused, "pause"));
                }
                false
            }
            Command::Resume(completion) => {
                if matches!(self.state, VmStatus::Paused | VmStatus::Running) {
                    completion.accept();
                    let result = self.resume(operation);
                    self.finish(result, completion);
                } else {
                    completion.reject(self.transition_error(VmStatus::Running, "resume"));
                }
                false
            }
            Command::Stop(reason, completion) => {
                if self.state != VmStatus::Stopped {
                    self.last_stop_reason = Some(reason);
                }
                completion.accept();
                let result = self.stop(operation);
                self.finish(result, completion);
                false
            }
            Command::Reset(completion) => {
                completion.accept();
                let result = self.stop(operation).and_then(|()| self.start(operation));
                self.finish(result, completion);
                false
            }
            Command::GuestReset { run, completion } => {
                if self.state != VmStatus::Running
                    || self.run.as_ref().is_none_or(|current| current.id != run)
                {
                    completion.reject(AxVmError::OperationCancelled { operation });
                } else {
                    completion.accept();
                    let result = self.stop(operation).and_then(|()| self.start(operation));
                    self.finish(result, completion);
                }
                false
            }
            Command::UpdateMemory {
                expected_run,
                update,
                completion,
            } => {
                if self.run.as_ref().is_none_or(|run| run.id != expected_run) {
                    completion.reject(AxVmError::StaleRun {
                        expected: expected_run,
                        current: self.last_run,
                    });
                } else {
                    completion.accept();
                    let result = self.update_memory(operation, update);
                    self.finish(result, completion);
                }
                false
            }
            Command::Destroy(completion) => {
                completion.accept();
                match self.stop(operation) {
                    Ok(()) => {
                        self.state = VmStatus::Destroyed;
                        self.current_operation = None;
                        self.publish();
                        exit.handoff(completion);
                        for command in self.deferred.drain(..) {
                            match command {
                                Command::Destroy(completion) => exit.handoff(completion),
                                command => command.reject(AxVmError::EntryClosed {
                                    vm: self.shared.key(),
                                }),
                            }
                        }
                        self.shared.close_for_destroy(exit);
                        true
                    }
                    Err(error) => {
                        self.finish(Err(error), completion);
                        false
                    }
                }
            }
        };
        if !destroyed {
            self.current_operation = None;
            self.publish();
        }
        destroyed
    }

    fn finish<T>(&mut self, result: AxVmResult<T>, completion: OperationCompletion<T>) {
        if let Err(error) = &result {
            self.last_failure = Some(error.clone());
        }
        self.current_operation = None;
        self.publish();
        completion.finish(result);
    }

    fn transition_error(&self, to: VmStatus, operation: &'static str) -> AxVmError {
        AxVmError::invalid_transition(self.state, to, operation)
    }

    fn record_failure(&mut self, error: AxVmError) {
        self.state = VmStatus::Failed;
        self.last_failure = Some(error);
        if let Some(run) = &self.run {
            run.admission.store(false, Ordering::Release);
            run.signals.close_interrupts();
            for member in run.participants.values().filter(|member| !member.returned) {
                member.port.signals.close_entry();
                if let Err(error) = run.signals.kick(member.instance.vcpu_id) {
                    error!(
                        "cannot kick failed VM participant {:?}: {error}",
                        member.instance
                    );
                }
            }
        }
        self.shared.publish_run_services(None);
    }

    fn spawn_vcpu(
        &mut self,
        id: usize,
        operation: OperationId,
        cpu_on: Option<CpuOnArgs>,
    ) -> AxVmResult<VcpuInstance> {
        let run = self
            .run
            .as_mut()
            .ok_or_else(|| AxVmError::invalid_state("start vCPU", "no prepared run"))?;
        if run.participants.contains_key(&id) {
            return Err(AxVmError::resource_conflict(
                "vCPU activation",
                "old activation has not retired",
            ));
        }
        let activation = next_generation(
            run.activations
                .get_mut(id)
                .ok_or_else(|| AxVmError::invalid_config("invalid vCPU identifier"))?,
            "vCPU activation",
        )?;
        let instance = VcpuInstance {
            run: run.id,
            vcpu_id: id,
            activation,
        };
        let slot = self
            .vm
            .resources
            .vcpu_list
            .as_mut()
            .and_then(|cpus| cpus.get_mut(id))
            .ok_or_else(|| AxVmError::invalid_config("missing vCPU backend slot"))?;
        let mut backend = slot.take().ok_or_else(|| {
            AxVmError::resource_conflict("vCPU backend", "backend already transferred")
        })?;
        let preparation = backend
            .replace_signals(Arc::new(crate::vcpu::VcpuSignals::new()))
            .and_then(|()| {
                if cpu_on.is_some() {
                    backend.reserve_for_cpu_on()
                } else {
                    Ok(())
                }
            });
        if let Err(error) = preparation {
            *slot = Some(backend);
            return Err(error);
        }
        let entry = ExecutionEntry::<CurrentArch> {
            root: self.vm.resources.nested_paging,
            revision: run.revision,
            decode: run.decode.clone(),
            architecture: run.architecture.clone(),
            signals: run.signals.clone(),
            admission: run.admission.clone(),
        };
        let prepared = match prepare_vcpu_thread(
            backend,
            instance,
            operation,
            entry,
            run.services.clone(),
            self.shared.clone(),
            cpu_on,
        ) {
            Ok(prepared) => prepared,
            Err((mut error, ownership)) => {
                let mut backend = match ownership {
                    StartupOwnership::Backend(backend) => *backend,
                    StartupOwnership::Cancelled(cancelled) => match cancelled.abort() {
                        Ok(backend) => backend,
                        Err((failure, cancelled)) => {
                            run.cancelled_startups.push((id, cancelled));
                            error = AxVmError::lifecycle_rollback(
                                "cancel failed vCPU stage",
                                error,
                                failure,
                            );
                            return Err(error);
                        }
                    },
                };
                backend.rollback_cpu_on();
                *slot = Some(backend);
                return Err(error);
            }
        };
        let task = prepared.thread_handle();
        if let Err(error) =
            run.signals
                .register(instance, prepared.port.signals.clone(), task.wake_handle())
        {
            let mut backend = match prepared.abort() {
                Ok(backend) => backend,
                Err((failure, cancelled)) => {
                    run.cancelled_startups.push((id, cancelled));
                    return Err(AxVmError::lifecycle_rollback(
                        "cancel unregistered vCPU",
                        error,
                        failure,
                    ));
                }
            };
            backend.rollback_cpu_on();
            *slot = Some(backend);
            return Err(error.into());
        }
        run.participants.insert(
            id,
            Participant {
                instance,
                task,
                port: prepared.port.clone(),
                startup: operation,
                started: false,
                parked: ConfirmationReceipt::default(),
                resumed: ConfirmationReceipt::default(),
                root_installed: ConfirmationReceipt::default(),
                returned: false,
                retired: false,
                cpu_off: false,
            },
        );
        prepared.activate();
        Ok(instance)
    }

    fn start(&mut self, operation: OperationId) -> AxVmResult<RunId> {
        if self.state == VmStatus::Running {
            return Ok(self.run.as_ref().expect("running run").id);
        }
        if self.run.is_none() {
            let generation = next_generation(&mut self.run_generation, "VM run")?;
            let id = RunId::new(self.shared.key(), generation);
            let signals = RunSignals::new(id, self.vm.config().phys_cpu_ls.cpu_num())?;
            let ports = DevicePorts::new(signals.clone(), Arc::downgrade(&self.shared));
            if let Err(error) = self.prepare_run(id, signals, ports) {
                self.record_failure(error.clone());
                return Err(error);
            }
        }
        let run = self.run.as_mut().expect("prepared run");
        let id = run.id;
        run.worker = Some(RunSignalWorker::start(run.signals.clone())?);
        if let Err(error) = CurrentArch::enter_runtime(&mut self.vm, &run.signals) {
            return match self.stop(operation) {
                Ok(()) => Err(error),
                Err(cleanup) => {
                    let failure =
                        AxVmError::lifecycle_rollback("start IRQ runtime", error, cleanup);
                    self.record_failure(failure.clone());
                    Err(failure)
                }
            };
        }
        self.last_run = Some(id);
        self.last_stop_reason = None;
        self.shared.publish_run_services(Some(run.services.clone()));
        let count = self.vm.config().phys_cpu_ls.cpu_num();
        let mut instances = Vec::new();
        let result = (|| {
            for cpu in current::boot_vcpu_ids(count) {
                instances.push(self.spawn_vcpu(cpu, operation, None)?);
            }
            self.wait_confirm(operation, &instances, Confirmation::Started)?;
            self.resume_participants(operation)?;
            let run = self.run.as_ref().expect("started run");
            run.signals
                .set_poll_owner(instances.first().map(|instance| instance.vcpu_id))?;
            run.admission.store(true, Ordering::Release);
            for member in run.participants.values() {
                run.signals
                    .kick(member.instance.vcpu_id)
                    .map_err(|error| AxVmError::interrupt("wake started vCPU", error))?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            let cleanup = self.stop(operation);
            if let Err(cleanup) = cleanup {
                self.record_failure(cleanup);
            }
            return Err(error);
        }
        self.state = VmStatus::Running;
        self.publish();
        Ok(id)
    }

    fn park_participants(&mut self, operation: OperationId) -> AxVmResult {
        let run = self.run.as_mut().expect("parking run");
        run.admission.store(false, Ordering::Release);
        let mut instances = Vec::new();
        let mut failure = None;
        for member in run
            .participants
            .values_mut()
            .filter(|member| !member.returned)
        {
            instances.push(member.instance);
            member.parked.request(member.instance, operation);
            if let Err(error) = member.port.send(VcpuCommand::Park { operation }) {
                failure.get_or_insert(error);
            }
        }
        self.wait_confirm(operation, &instances, Confirmation::Parked)?;
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn resume_participants(&mut self, operation: OperationId) -> AxVmResult {
        let run = self.run.as_mut().expect("resuming run");
        let mut instances = Vec::new();
        for member in run
            .participants
            .values_mut()
            .filter(|member| !member.returned)
        {
            instances.push(member.instance);
            member.resumed.request(member.instance, operation);
            member.port.send(VcpuCommand::Resume { operation })?;
        }
        self.wait_confirm(operation, &instances, Confirmation::Resumed)
    }

    fn pause(&mut self, operation: OperationId) -> AxVmResult {
        if self.state == VmStatus::Paused {
            return Ok(());
        }
        self.state = VmStatus::Pausing;
        self.publish();
        let quiet = self.park_participants(operation).and_then(|()| {
            let run = self.run.as_ref().expect("paused run");
            run.ports.suspend()?;
            run.services
                .devices()
                .suspend_lifecycle_devices()
                .map_err(|error| AxVmError::device("suspend VM devices", error))
        });
        if let Err(error) = quiet {
            let rollback = (|| {
                let run = self.run.as_ref().expect("rollback pause");
                run.services
                    .devices()
                    .resume_lifecycle_devices()
                    .map_err(|error| AxVmError::device("undo device pause", error))?;
                run.ports.resume()?;
                self.resume_participants(operation)?;
                self.open_run()?;
                Ok(())
            })();
            match rollback {
                Ok(()) => self.state = VmStatus::Running,
                Err(error) => self.record_failure(error),
            }
            return Err(error);
        }
        self.state = VmStatus::Paused;
        Ok(())
    }

    fn resume(&mut self, operation: OperationId) -> AxVmResult {
        if self.state == VmStatus::Running {
            return Ok(());
        }
        let result = (|| {
            let run = self.run.as_ref().expect("resume run");
            run.services
                .devices()
                .resume_lifecycle_devices()
                .map_err(|error| AxVmError::device("resume VM devices", error))?;
            run.ports.resume()?;
            self.resume_participants(operation)?;
            self.open_run()?;
            Ok(())
        })();
        if let Err(error) = result {
            let rollback = self.park_participants(operation).and_then(|()| {
                let run = self.run.as_ref().expect("undo resume");
                run.ports.suspend()?;
                run.services
                    .devices()
                    .suspend_lifecycle_devices()
                    .map_err(|error| AxVmError::device("undo device resume", error))
            });
            if let Err(error) = rollback {
                self.record_failure(error);
            }
            return Err(error);
        }
        self.state = VmStatus::Running;
        Ok(())
    }

    fn open_run(&self) -> AxVmResult {
        let run = self.run.as_ref().expect("open prepared run");
        run.admission.store(true, Ordering::Release);
        for member in run.participants.values().filter(|member| !member.returned) {
            run.signals.kick(member.instance.vcpu_id)?;
        }
        Ok(())
    }

    fn stop(&mut self, operation: OperationId) -> AxVmResult {
        let result = self.stop_run(operation);
        if let Err(error) = &result {
            self.record_failure(error.clone());
        }
        result
    }

    fn stop_run(&mut self, operation: OperationId) -> AxVmResult {
        self.last_stop_reason
            .get_or_insert(crate::StopReason::Clean);
        self.shared.publish_run_services(None);
        if self.run.is_none() {
            if let Some(ports) = &self.preparing_ports {
                ports.close();
                ports.suspend()?;
            }
            // Creation may have failed after preparing devices but before a run
            // was published. Teardown still owns and quiets those devices.
            if let Some(devices) = &self.vm.resources.devices {
                devices
                    .stop_lifecycle_devices()
                    .map_err(|error| AxVmError::device("stop prepared devices", error))?;
            }
            self.state = VmStatus::Stopped;
            return Ok(());
        }
        self.state = VmStatus::Stopping;
        self.publish();
        let run = self.run.as_mut().expect("stopping run");
        run.admission.store(false, Ordering::Release);
        run.signals.close_interrupts();
        for (_, reply) in std::mem::take(&mut run.startup_replies) {
            reply
                .completion
                .finish(Err(AxVmError::OperationCancelled { operation }));
        }
        let mut failure = None;
        // Kick every participant before unregistering any external producer.
        for member in run.participants.values().filter(|member| !member.returned) {
            if let Err(error) = member.port.send(VcpuCommand::Stop { operation }) {
                failure.get_or_insert(error);
            }
        }
        if let Err(error) = CurrentArch::exit_runtime(&mut self.vm, &run.signals) {
            failure.get_or_insert(error);
        }
        if let Err(error) = run.ports.suspend() {
            failure.get_or_insert(error);
        }
        if let Err(error) = run.services.devices().stop_lifecycle_devices() {
            failure.get_or_insert_with(|| AxVmError::device("stop VM devices", error));
        }
        self.pump_until(|owner| {
            owner.run.as_ref().is_none_or(|run| {
                run.participants
                    .values()
                    .all(|member| member.returned && member.retired)
            })
        })?;
        self.reap_participants()?;
        let run = self.run.as_mut().expect("retained stopping run");
        while !run.signals.interrupts_quiet() {
            crate::host::task::yield_now();
        }
        run.services.memory().close();
        self.pump_until(|owner| {
            owner
                .run
                .as_ref()
                .is_none_or(|run| run.services.memory().quiescent())
        })?;
        if let Some(worker) = &mut self.run.as_mut().expect("stop worker").worker
            && let Err(error) = worker.stop()
        {
            failure.get_or_insert(error);
        }
        // Failed producer retirement is not permission to unmap or release its
        // backing, even if no access lease is currently observed. All ownership
        // stays attached to this closed run for the next cleanup attempt.
        if let Some(error) = failure {
            return Err(error);
        }
        self.retire_failed_memory_update()?;
        self.teardown_ivc(operation)?;
        self.invalidate_root(self.vm.resources.nested_paging)?;
        self.run
            .as_ref()
            .expect("retire stopped memory")
            .services
            .memory()
            .retire()?;
        self.shared.publish_run_services(None);
        // Device routes, decode views and workers are now quiet. New starts
        // prepare fresh controllers and fresh ports for another RunId.
        self.run = None;
        self.state = VmStatus::Stopped;
        Ok(())
    }

    fn wait_confirm(
        &mut self,
        operation: OperationId,
        instances: &[VcpuInstance],
        confirmation: Confirmation,
    ) -> AxVmResult {
        self.pump_until(|owner| {
            let Some(run) = &owner.run else { return true };
            run.failure.is_some()
                || instances.iter().any(|instance| {
                    run.confirmation(*instance, operation, confirmation)
                        == ConfirmationProgress::Cancelled
                })
                || instances.iter().all(|instance| {
                    run.confirmation(*instance, operation, confirmation)
                        == ConfirmationProgress::Complete
                })
        })?;
        let run = self
            .run
            .as_ref()
            .ok_or(AxVmError::OperationCancelled { operation })?;
        if let Some(error) = &run.failure {
            return Err(error.clone());
        }
        if instances.iter().any(|instance| {
            run.confirmation(*instance, operation, confirmation) == ConfirmationProgress::Cancelled
        }) {
            return Err(AxVmError::OperationCancelled { operation });
        }
        Ok(())
    }

    /// Management commands remain FIFO while internal messages keep progressing.
    fn pump_until(&mut self, complete: impl Fn(&Self) -> bool) -> AxVmResult {
        while !complete(self) {
            match self.shared.next_message() {
                Some(ControlMessage::Command(command)) => self.deferred.push_back(command),
                Some(message) => self.internal(message),
                None => {
                    return Err(AxVmError::EntryClosed {
                        vm: self.shared.key(),
                    });
                }
            }
        }
        Ok(())
    }

    fn internal(&mut self, message: ControlMessage) {
        match message {
            ControlMessage::Event(event) => self.event(event),
            ControlMessage::RunStop { run, reason } => {
                if self.run.as_ref().is_some_and(|current| current.id == run) {
                    self.guest_stop = Some(run);
                    self.last_stop_reason = Some(reason);
                }
            }
            ControlMessage::Guest {
                instance,
                request,
                completion,
            } => self.guest_request(instance, request, completion),
            ControlMessage::Command(command) => self.deferred.push_back(command),
        }
    }

    fn event(&mut self, event: VcpuEvent) {
        if let VcpuEvent::MemoryIdle { run } = event {
            if self.run.as_ref().is_some_and(|current| current.id == run) {
                self.publish();
            }
            return;
        }
        let instance = match &event {
            VcpuEvent::Started { instance, .. }
            | VcpuEvent::Parked { instance, .. }
            | VcpuEvent::Resumed { instance, .. }
            | VcpuEvent::RootInstalled { instance, .. }
            | VcpuEvent::FirstEntered { instance }
            | VcpuEvent::Exited { instance, .. }
            | VcpuEvent::Retired { instance } => *instance,
            VcpuEvent::MemoryIdle { .. } => unreachable!(),
        };
        let Some(run) = self.run.as_mut().filter(|run| run.id == instance.run) else {
            return;
        };
        let Some(member) = run
            .participants
            .get_mut(&instance.vcpu_id)
            .filter(|member| member.instance == instance)
        else {
            return;
        };
        match event {
            VcpuEvent::Started { operation, .. } if operation == member.startup => {
                member.started = true;
                if run.startup_replies.contains_key(&instance.vcpu_id)
                    && self.state == VmStatus::Running
                {
                    member.resumed.request(instance, operation);
                    if let Err(error) = member.port.send(VcpuCommand::Resume { operation }) {
                        run.failure.get_or_insert(error);
                    }
                }
            }
            VcpuEvent::Parked { operation, .. } if member.parked.accept(instance, operation) => {}
            VcpuEvent::Resumed { operation, .. } if member.resumed.accept(instance, operation) => {
                if run
                    .startup_replies
                    .get(&instance.vcpu_id)
                    .is_some_and(|reply| reply.operation == member.startup && member.started)
                {
                    let reply = run
                        .startup_replies
                        .remove(&instance.vcpu_id)
                        .expect("matching startup reply");
                    reply
                        .completion
                        .finish(Ok(requests::cpu_on_success(reply.abi)));
                    if run.signals.poll_owner().is_none()
                        && let Err(error) = run.signals.set_poll_owner(Some(instance.vcpu_id))
                    {
                        run.failure.get_or_insert(error.into());
                    }
                }
            }
            VcpuEvent::RootInstalled { operation, .. }
                if member.root_installed.accept(instance, operation) => {}
            VcpuEvent::Exited {
                backend, outcome, ..
            } => {
                member.returned = true;
                member.port.signals.request_stop();
                if let Some(cpus) = &mut self.vm.resources.vcpu_list {
                    cpus[instance.vcpu_id] = Some(*backend);
                }
                let startup_failed = !member.started
                    && run
                        .startup_replies
                        .get(&instance.vcpu_id)
                        .is_some_and(|reply| reply.operation == member.startup);
                if run
                    .startup_replies
                    .get(&instance.vcpu_id)
                    .is_some_and(|reply| reply.operation == member.startup)
                    && let Some(reply) = run.startup_replies.remove(&instance.vcpu_id)
                {
                    reply
                        .completion
                        .finish(Ok(requests::cpu_on_failure(reply.abi)));
                }
                match outcome {
                    VcpuExitOutcome::CpuOff => member.cpu_off = true,
                    VcpuExitOutcome::Fault(error) if startup_failed => {
                        self.last_failure = Some(error);
                    }
                    VcpuExitOutcome::Fault(error) => {
                        self.last_stop_reason = Some(crate::StopReason::Fault(error.to_string()));
                        run.failure.get_or_insert(error.clone());
                        self.last_failure = Some(error);
                        self.guest_stop = Some(instance.run);
                    }
                    VcpuExitOutcome::Stopped => {}
                }
                let next = run
                    .participants
                    .values()
                    .find(|member| !member.returned)
                    .map(|member| member.instance.vcpu_id);
                if let Err(error) = run.signals.set_poll_owner(next) {
                    run.failure.get_or_insert(error.into());
                }
            }
            VcpuEvent::Retired { .. } => member.retired = true,
            _ => {}
        }
        if let Err(error) = self.reap_participants() {
            self.record_failure(error);
        }
        if self.state == VmStatus::Running
            && self
                .run
                .as_ref()
                .is_some_and(|run| run.participants.is_empty())
        {
            self.guest_stop = Some(instance.run);
        }
        self.start_retired_reservations();
        self.publish();
    }

    fn reap_participants(&mut self) -> AxVmResult {
        let Some(run) = &mut self.run else {
            return Ok(());
        };
        while let Some((id, cancelled)) = run.cancelled_startups.pop() {
            match cancelled.abort() {
                Ok(mut backend) => {
                    backend.rollback_cpu_on();
                    self.vm
                        .resources
                        .vcpu_list
                        .as_mut()
                        .expect("run owns vCPU slots")[id] = Some(backend);
                }
                Err((error, cancelled)) => {
                    run.cancelled_startups.push((id, cancelled));
                    return Err(error);
                }
            }
        }
        let ids = run
            .participants
            .iter()
            .filter(|(_, member)| member.returned && member.retired)
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        for id in ids {
            let member = &run.participants[&id];
            member
                .task
                .clone()
                .join()
                .map_err(|error| AxVmError::host("join retired vCPU", error))?;
            run.signals.unregister(member.instance);
            run.joined_activations[id] = member.instance.activation;
            run.retired_entries = run
                .retired_entries
                .saturating_add(member.port.entries.load(Ordering::Relaxed));
            run.retired_parks = run
                .retired_parks
                .saturating_add(member.port.parks.load(Ordering::Relaxed));
            run.participants.remove(&id);
        }
        Ok(())
    }

    fn publish(&self) {
        let description = VmConfigSnapshot {
            bsp_entry: self.vm.config().bsp_entry(),
            ap_entry: self.vm.config().ap_entry(),
            image: self.vm.config().image_config().clone(),
            address_space_policy: self.vm.config().address_space_policy(),
            vcpu_affinities: self.vm.get_vcpu_affinities_pcpu_ids(),
            passthrough_devices: self.vm.config().pass_through_devices().to_vec(),
            passthrough_addresses: self.vm.config().pass_through_addresses().to_vec(),
            passthrough_irqs: self.vm.config().pass_through_irqs().to_vec(),
        };
        let mut vcpu = self.vm.vcpu_snapshots();
        let mut entry_count = 0;
        let mut park_count = 0;
        if let Some(run) = &self.run {
            entry_count = run.retired_entries;
            park_count = run.retired_parks;
            for member in run.participants.values() {
                entry_count =
                    entry_count.saturating_add(member.port.entries.load(Ordering::Relaxed));
                park_count = park_count.saturating_add(member.port.parks.load(Ordering::Relaxed));
                if member.returned {
                    continue;
                }
                vcpu.push(VcpuSnapshot {
                    id: member.instance.vcpu_id,
                    state: if member.started {
                        VmVcpuState::Ready
                    } else {
                        VmVcpuState::Starting
                    },
                    phys_cpu_set: description
                        .vcpu_affinities
                        .iter()
                        .find(|(id, ..)| *id == member.instance.vcpu_id)
                        .and_then(|(_, mask, _)| *mask),
                });
            }
        }
        vcpu.sort_by_key(|cpu| cpu.id);
        let regions = self.vm.memory_regions();
        self.shared.publish(VmSnapshot {
            key: self.shared.key(),
            vm_id: self.vm.id(),
            name: self.vm.name(),
            state: self.state,
            run: self.last_run,
            current_operation: self.current_operation,
            last_failure: self.last_failure.clone(),
            last_stop_reason: self.last_stop_reason.clone(),
            cpu: CpuObservation {
                vcpu_num: self.vm.config().phys_cpu_ls.cpu_num(),
                running_vcpu_count: self.run.as_ref().map_or(0, |run| {
                    run.participants
                        .values()
                        .filter(|member| member.started && !member.returned)
                        .count()
                }),
            },
            memory: MemoryObservation {
                nested_page_table_root: Some(self.vm.nested_page_table_root()),
                total_bytes: regions.iter().map(|region| region.size()).sum(),
                regions,
            },
            device: DeviceObservation {
                device_count: self.vm.device_count(),
            },
            vcpu,
            description,
            entry_count,
            park_count,
        });
    }
}
