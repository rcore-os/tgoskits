//! Events responsibilities of the unique lifecycle owner.

use super::{Confirmation, ConfirmationProgress, Owner, requests};
use crate::{
    AxVmError, AxVmResult, OperationId, VmStatus,
    identity::VcpuInstance,
    manager::ControlMessage,
    runtime::vcpus::{VcpuCommand, VcpuEvent, VcpuExitOutcome},
};

impl Owner {
    pub(super) fn wait_confirm(
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
    pub(super) fn pump_until(&mut self, complete: impl Fn(&Self) -> bool) -> AxVmResult {
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

    pub(super) fn internal(&mut self, message: ControlMessage) {
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

    pub(super) fn event(&mut self, event: VcpuEvent) {
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
                        log::error!("vCPU {instance:?} failed during startup: {error}");
                        self.last_failure = Some(error);
                    }
                    VcpuExitOutcome::Fault(error) => {
                        log::error!("vCPU {instance:?} exited with a fault: {error}");
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
}
