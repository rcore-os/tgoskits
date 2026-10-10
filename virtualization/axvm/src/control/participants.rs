//! Participants responsibilities of the unique lifecycle owner.

use std::sync::{Arc, atomic::Ordering};

use super::{Confirmation, Owner, Participant, confirmations::ConfirmationReceipt};
use crate::{
    AxVmError, AxVmResult, OperationId, VmVcpuState,
    arch::current::CurrentArch,
    architecture::ArchOps,
    engine::ExecutionEntry,
    identity::{VcpuInstance, next_generation},
    runtime::vcpus::{
        CpuOnArgs, StartupOwnership, VcpuCommand, VcpuTaskOptions, prepare_vcpu_thread,
    },
};

impl Owner {
    pub(super) fn spawn_vcpu(
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
            VcpuTaskOptions {
                cpu_on,
                schedule_policy: self.vcpu_schedule_policy,
            },
        ) {
            Ok(prepared) => prepared,
            Err((error, ownership)) => {
                let mut error = *error;
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

    pub(super) fn park_participants(&mut self, operation: OperationId) -> AxVmResult {
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

    pub(super) fn resume_participants(&mut self, operation: OperationId) -> AxVmResult {
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

    pub(super) fn reap_participants(&mut self) -> AxVmResult {
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
            // Returned backends remain owned even when producer cancellation
            // failed. Retry retirement after join before releasing the run.
            let backend = self
                .vm
                .resources
                .vcpu_list
                .as_mut()
                .expect("run owns vCPU slots")[id]
                .as_mut()
                .expect("a returned activation retains its backend");
            backend.with_backend(CurrentArch::quiet_vcpu)?;
            if backend.state() == VmVcpuState::Ready {
                backend.unbind()?;
            }
            run.signals.unregister(member.instance);
            run.joined_activations[id] = member.instance.activation;
            run.retired_entries = run
                .retired_entries
                .saturating_add(member.port.progress.entries.load(Ordering::Relaxed));
            run.retired_parks = run
                .retired_parks
                .saturating_add(member.port.progress.parks.load(Ordering::Relaxed));
            run.participants.remove(&id);
        }
        Ok(())
    }
}
