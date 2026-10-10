//! Lifecycle responsibilities of the unique lifecycle owner.

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use super::{Confirmation, Owner, RunState};
use crate::{
    AxVmError, AxVmResult, OperationId, RunId, VmStatus,
    arch::current::{self, CurrentArch},
    architecture::ArchOps,
    guest_memory::{DecodeMemory, GuestMemoryPort, MemoryRevision},
    identity::next_generation,
    runtime::vcpus::{VcpuCommand, VcpuEvent},
    services::{DevicePorts, RunServices, RunSignalWorker, RunSignals},
};

impl Owner {
    pub(super) fn prepare_run(
        &mut self,
        id: RunId,
        signals: Arc<RunSignals>,
        ports: Arc<DevicePorts>,
    ) -> AxVmResult {
        self.preparing_ports = Some(ports.clone());
        // Hard-IRQ/controller producers publish only into this run's fixed
        // ingress; the signal worker forwards the events to this owner task.
        signals.bind_control(Arc::downgrade(&self.shared));
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

    pub(super) fn start(&mut self, operation: OperationId) -> AxVmResult<RunId> {
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

    pub(super) fn pause(&mut self, operation: OperationId) -> AxVmResult {
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
                Ok(()) => {
                    self.state = VmStatus::Running;
                    self.start_retired_reservations();
                }
                Err(error) => self.record_failure(error),
            }
            return Err(error);
        }
        self.state = VmStatus::Paused;
        Ok(())
    }

    pub(super) fn resume(&mut self, operation: OperationId) -> AxVmResult {
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
        // Retirements received while Pausing/Paused could not launch CPU_ON.
        // Re-drive their reservations here without requiring another event.
        self.start_retired_reservations();
        Ok(())
    }

    pub(super) fn open_run(&self) -> AxVmResult {
        let run = self.run.as_ref().expect("open prepared run");
        run.admission.store(true, Ordering::Release);
        for member in run.participants.values().filter(|member| !member.returned) {
            run.signals.kick(member.instance.vcpu_id)?;
        }
        Ok(())
    }

    pub(super) fn stop(&mut self, operation: OperationId) -> AxVmResult {
        let result = self.stop_run(operation);
        if let Err(error) = &result {
            self.record_failure(error.clone());
        }
        result
    }

    pub(super) fn stop_run(&mut self, operation: OperationId) -> AxVmResult {
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
        // Architecture runtime teardown may release physical interrupt
        // bindings. Every vCPU must have returned and retired its backend
        // before this call; AArch64 rejects SPI teardown while a CPU
        // interface is still loaded.
        if let Err(error) = CurrentArch::exit_runtime(&mut self.vm, &run.signals) {
            failure.get_or_insert(error);
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
}
