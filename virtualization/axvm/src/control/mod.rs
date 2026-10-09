//! One task owns lifecycle transitions, backend transfers and resource retirement.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use crate::{
    AxVmError, AxVmResult, OperationId, RunId, VmStatus,
    arch::current::CurrentArch,
    architecture::ArchOps,
    boot::{BootImageProvider, PreparedGuestBoot},
    guest_memory::{DecodeMemory, MemoryRevision},
    identity::VcpuInstance,
    manager::{Command, ControlExit, ControlMessage, ControlShared, VmCreatePlan, VmHandle},
    operation::OperationCompletion,
    runtime::{
        hvc::HyperCallAbi,
        vcpus::{CpuOnArgs, VcpuPort},
    },
    services::{DevicePorts, RunServices, RunSignalWorker, RunSignals},
    vm::AxVM,
};

mod confirmations;
mod memory;
use confirmations::ConfirmationReceipt;
mod events;
mod lifecycle;
mod observations;
mod participants;
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
    vcpu_schedule_policy: crate::SchedulePolicy,
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
        vcpu_schedule_policy: plan.vcpu_schedule_policy,
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
    fn command(&mut self, command: Command, exit: &ControlExit) -> bool {
        let operation = command.id();
        self.current_operation = Some(operation);
        self.publish();
        let destroyed = match command {
            Command::Start(completion) => {
                if matches!(self.state, VmStatus::Ready | VmStatus::Stopped) {
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
                    completion.finish(Err(AxVmError::OperationCancelled { operation }));
                } else {
                    // A reset guest must leave before stop can join it. Reply
                    // only after this FIFO command validates the original run.
                    completion.finish(Ok(0));
                    let result = self.stop(operation).and_then(|()| self.start(operation));
                    if let Err(error) = result {
                        self.last_failure = Some(error);
                    }
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
        self.last_failure = Some(error.clone());
        if let Some(run) = &mut self.run {
            // Confirmation waits observe this failure as well as snapshots.
            // A failed reaper may have no remaining task to emit another event.
            run.failure.get_or_insert(error);
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
}
