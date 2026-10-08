//! Guest requests execute on the lifecycle owner with exact activation identity.

use axaddrspace::NestedPageTableOps;
use axdevice_base::GuestMemoryAccess;
use axhvc::HyperCallCode;

use super::{Owner, StartupReply};
use crate::{
    AxVmError, AxVmResult, OperationId, VmStatus, VmVcpuState,
    guest_memory::MemoryUpdate,
    identity::VcpuInstance,
    operation::OperationCompletion,
    runtime::{
        hvc::{self, GuestRequest, HyperCallAbi},
        ivc::{self, IvcEndpointPlan},
        vcpus::{CpuOnArgs, VcpuOnError},
    },
};

pub(super) fn cpu_on_success(_abi: HyperCallAbi) -> usize {
    0
}
pub(super) fn cpu_on_failure(abi: HyperCallAbi) -> usize {
    cpu_on_error(abi, VcpuOnError::StartFailed)
}

fn cpu_on_error(abi: HyperCallAbi, error: VcpuOnError) -> usize {
    match abi {
        HyperCallAbi::AArch64 => hvc::psci_cpu_on_result(Err(error)),
        HyperCallAbi::Generic => match error {
            VcpuOnError::AlreadyOn => (-6isize) as usize,
            VcpuOnError::OnPending => (-6isize) as usize,
            VcpuOnError::StartFailed => (-1isize) as usize,
        },
    }
}

impl Owner {
    pub(super) fn guest_request(
        &mut self,
        instance: VcpuInstance,
        request: GuestRequest,
        completion: OperationCompletion<usize>,
    ) {
        let valid = self.run.as_ref().is_some_and(|run| {
            run.id == instance.run
                && run
                    .participants
                    .get(&instance.vcpu_id)
                    .is_some_and(|member| member.instance == instance && !member.returned)
        });
        if !valid {
            completion.reject(AxVmError::StaleRun {
                expected: instance.run,
                current: self.last_run,
            });
            return;
        }
        completion.accept();
        let request = match request {
            GuestRequest::Hypercall {
                code: HyperCallCode::PSCICpuOn | HyperCallCode::PSCICpuOn64,
                args,
            } => {
                let run = self.run.as_ref().expect("validated PSCI run");
                let Some(target) =
                    hvc::psci_find_vcpu_by_mpidr(args[0], run.topology.iter().copied())
                else {
                    completion.finish(Ok(hvc::PSCI_RET_INVALID_PARAMETERS));
                    return;
                };
                GuestRequest::CpuOn {
                    target_vcpu_id: target,
                    entry_point: (args[1] as usize).into(),
                    context_id: args[2] as usize,
                    abi: HyperCallAbi::AArch64,
                }
            }
            request => request,
        };
        match request {
            GuestRequest::CpuOn {
                target_vcpu_id,
                entry_point,
                context_id,
                abi,
            } => {
                self.cpu_on(
                    target_vcpu_id,
                    CpuOnArgs {
                        entry: entry_point,
                        argument: context_id,
                    },
                    abi,
                    completion,
                );
            }
            GuestRequest::CpuOff { abi } => {
                let run = self.run.as_mut().expect("validated CPU_OFF run");
                let online = run
                    .participants
                    .values()
                    .filter(|member| !member.returned && !member.cpu_off)
                    .count();
                let result = if self.state != VmStatus::Running
                    || (abi == HyperCallAbi::AArch64 && online <= 1)
                {
                    hvc::psci_cpu_off_result(false)
                } else {
                    run.participants
                        .get_mut(&instance.vcpu_id)
                        .expect("validated CPU_OFF owner")
                        .cpu_off = true;
                    0
                };
                completion.finish(Ok(result));
            }
            GuestRequest::Reset => {
                // The caller remains unbound and exits after its reply. The
                // accepted reset is a normal owner command; no callback waits
                // synchronously for a command whose completion needs this task.
                if self.state != VmStatus::Running {
                    let operation = completion.id();
                    completion.finish(Err(AxVmError::OperationCancelled { operation }));
                } else {
                    let _result =
                        self.shared
                            .dispatch_command(crate::manager::Command::GuestReset {
                                run: instance.run,
                                completion,
                            });
                }
            }
            GuestRequest::NestedFault { addr, access_flags } => {
                // Linear RAM mappings are populated eagerly. A permission or
                // missing-mapping fault cannot be repaired by granting access.
                let mapping =
                    NestedPageTableOps::query(self.vm.resources.address_space.page_table(), addr);
                let result = match mapping {
                    Ok((_, flags, _)) if flags.contains(access_flags) => Ok(0),
                    _ => Err(AxVmError::memory(
                        "unhandled guest translation fault",
                        format_args!("address={addr:?}, access={access_flags:?}"),
                    )),
                };
                completion.finish(result);
            }
            GuestRequest::Hypercall {
                code: HyperCallCode::PSCIAffinityInfo | HyperCallCode::PSCIAffinityInfo64,
                args,
            } => {
                let run = self.run.as_ref().expect("validated affinity run");
                let result = hvc::psci_affinity_info_result_for_domain(
                    args[0],
                    args[1],
                    run.topology.iter().map(|(id, mpidr)| {
                        let state = if run.startup_replies.contains_key(id) {
                            VmVcpuState::Starting
                        } else if run
                            .participants
                            .get(id)
                            .is_some_and(|member| !member.returned && !member.cpu_off)
                        {
                            VmVcpuState::Ready
                        } else {
                            VmVcpuState::Free
                        };
                        (*mpidr, state)
                    }),
                );
                completion.finish(Ok(result));
            }
            GuestRequest::Hypercall { code, args } => {
                let result = self.hypercall(completion.id(), code, args);
                completion.finish(result);
            }
        }
    }

    fn cpu_on(
        &mut self,
        target: usize,
        args: CpuOnArgs,
        abi: HyperCallAbi,
        completion: OperationCompletion<usize>,
    ) {
        if self.state != VmStatus::Running {
            completion.finish(Ok(if abi == HyperCallAbi::AArch64 {
                hvc::PSCI_RET_DENIED
            } else {
                (-1isize) as usize
            }));
            return;
        }
        let run = self.run.as_mut().expect("CPU_ON run");
        if target >= run.activations.len() {
            completion.finish(Ok(if abi == HyperCallAbi::AArch64 {
                hvc::PSCI_RET_INVALID_PARAMETERS
            } else {
                (-3isize) as usize
            }));
            return;
        }
        if run.startup_replies.contains_key(&target) {
            completion.finish(Ok(cpu_on_error(abi, VcpuOnError::OnPending)));
            return;
        }
        if run
            .participants
            .get(&target)
            .is_some_and(|member| !member.cpu_off && !member.returned)
        {
            completion.finish(Ok(cpu_on_error(abi, VcpuOnError::AlreadyOn)));
            return;
        }
        let operation = completion.id();
        run.startup_replies.insert(
            target,
            StartupReply {
                operation,
                completion,
                abi,
                args,
            },
        );
        // A CPU_OFF activation must return its backend, retire its original
        // stack and join before a new task can consume that backend.
        self.start_retired_reservations();
    }

    pub(super) fn start_retired_reservations(&mut self) {
        if self.state != VmStatus::Running {
            return;
        }
        let Some(run) = &self.run else {
            return;
        };
        let candidates = run
            .startup_replies
            .iter()
            .filter(|(id, _)| !run.participants.contains_key(id))
            .map(|(id, reply)| {
                (
                    *id,
                    reply.operation,
                    CpuOnArgs {
                        entry: reply.args.entry,
                        argument: reply.args.argument,
                    },
                )
            })
            .collect::<Vec<_>>();
        for (id, operation, args) in candidates {
            if let Err(error) = self.spawn_vcpu(id, operation, Some(args)) {
                warn!("CPU_ON startup failed: {error}");
                if let Some(reply) = self
                    .run
                    .as_mut()
                    .expect("CPU_ON run retained")
                    .startup_replies
                    .remove(&id)
                {
                    reply.completion.finish(Ok(cpu_on_failure(reply.abi)));
                }
                // A TASK_NEW cancellation emits no vCPU event. Drive its
                // retained transfer now; on failure freeze admission and let
                // the owner perform the normal stop/retirement transaction.
                if let Err(cleanup) = self.reap_participants() {
                    self.record_failure(cleanup);
                    self.guest_stop = self.run.as_ref().map(|run| run.id);
                    break;
                }
            }
        }
    }

    fn ivc_endpoint_plan(&self) -> AxVmResult<IvcEndpointPlan> {
        let run = self.run.as_ref().expect("IVC request run");
        let devices = run.services.devices();
        let aperture = devices
            .service::<ivc::IvcApertureAllocatorKey>()
            .map_err(|error| AxVmError::device("resolve IVC aperture", error))?;
        let notify = match devices.service::<ivc::IvcNotifyEndpointKey>() {
            Ok(endpoint) => Some(endpoint),
            Err(axdevice::DeviceManagerError::ResourceNotFound { .. }) => None,
            Err(error) => return Err(AxVmError::device("resolve IVC notify endpoint", error)),
        };
        Ok(IvcEndpointPlan {
            owner: self.shared.key(),
            run: run.id,
            aperture,
            notify,
            signals: run.signals.clone(),
        })
    }

    fn hypercall(
        &mut self,
        operation: OperationId,
        code: HyperCallCode,
        args: [u64; 6],
    ) -> AxVmResult<usize> {
        debug!("owner guest hypercall {code:?}, operation={operation:?}");
        match code {
            HyperCallCode::HIVCPublishChannel | HyperCallCode::HIVCSubscribChannel => {
                let subscribing = code == HyperCallCode::HIVCSubscribChannel;
                let (base_ptr, size_ptr) = if subscribing {
                    (args[2], args[3])
                } else {
                    (args[1], args[2])
                };
                let memory = self.run.as_ref().expect("IVC run").services.memory();
                let requested = if subscribing {
                    ivc::MAX_IVC_CHANNEL_SIZE
                } else {
                    let mut bytes = [0u8; size_of::<usize>()];
                    memory
                        .clone()
                        .read((size_ptr as usize).into(), &mut bytes)
                        .map_err(|error| AxVmError::device("read IVC requested size", error))?;
                    usize::from_le_bytes(bytes)
                };
                let attach = if subscribing {
                    self.shared.ivc.prepare_subscribe(
                        args[0] as usize,
                        args[1] as usize,
                        requested,
                        self.ivc_endpoint_plan()?,
                    )?
                } else {
                    self.shared.ivc.prepare_publish(
                        args[0] as usize,
                        requested,
                        1,
                        self.ivc_endpoint_plan()?,
                    )?
                };
                let range = attach.mapping().range;
                let revision =
                    match self.update_memory(operation, MemoryUpdate::Map(attach.mapping())) {
                        Ok(revision) => revision,
                        Err(error) => {
                            let installed = self
                                .vm
                                .resources
                                .memory_leases
                                .iter()
                                .any(|lease| lease.range == range)
                                || self
                                    .run
                                    .as_ref()
                                    .is_some_and(|run| run.pending_memory.is_some());
                            if !installed {
                                attach.cancel_uninstalled()?;
                            }
                            // Installed reservations remain in the manager's table
                            // and are retired by stop through prepare_teardown.
                            return Err(error);
                        }
                    };
                let address = attach.guest_addr().as_usize();
                let size = attach.size();
                let binding = attach.commit(revision)?;
                self.ivc_bindings.push(binding);
                // The memory port is reacquired after the owner publishes the
                // new revision. This keeps the result copy coupled to the
                // active run service instead of a port captured before the
                // translation transaction.
                let result_memory = self
                    .run
                    .as_ref()
                    .expect("IVC result memory run")
                    .services
                    .memory();
                let write = result_memory
                    .with_access(|access| {
                        access
                            .write((base_ptr as usize).into(), &address.to_le_bytes())
                            .and_then(|()| {
                                access.write((size_ptr as usize).into(), &size.to_le_bytes())
                            })
                    })
                    .map_err(|error| AxVmError::device("acquire IVC result memory", error))?
                    .map_err(|error| AxVmError::device("write IVC mapping result", error));
                if let Err(error) = write {
                    let detach = if subscribing {
                        self.shared.ivc.prepare_unsubscribe(
                            args[0] as usize,
                            args[1] as usize,
                            self.shared.key(),
                        )?
                    } else {
                        self.shared
                            .ivc
                            .prepare_unpublish(self.shared.key(), args[0] as usize)?
                    };
                    self.retire_ivc(operation, detach)?;
                    return Err(error);
                }
                Ok(0)
            }
            HyperCallCode::HIVCUnPublishChannel => {
                let detach = self
                    .shared
                    .ivc
                    .prepare_unpublish(self.shared.key(), args[0] as usize)?;
                self.retire_ivc(operation, detach)?;
                self.ivc_bindings.retain(|binding| {
                    !(binding.key().publisher == self.shared.key()
                        && binding.key().key == args[0] as usize)
                });
                Ok(0)
            }
            HyperCallCode::HIVCUnSubscribChannel => {
                let detach = self.shared.ivc.prepare_unsubscribe(
                    args[0] as usize,
                    args[1] as usize,
                    self.shared.key(),
                )?;
                self.retire_ivc(operation, detach)?;
                self.ivc_bindings.retain(|binding| {
                    !(binding.key().publisher.vm_id() == args[0] as usize
                        && binding.key().key == args[1] as usize)
                });
                Ok(0)
            }
            HyperCallCode::HIVCNotify => {
                self.shared.ivc.notify_channel(
                    args[0] as usize,
                    args[1] as usize,
                    self.shared.key(),
                    args[2] as usize,
                )?;
                Ok(0)
            }
            _ => Err(AxVmError::unsupported(
                "guest hypercall",
                format_args!("code={:#x}", code as u32),
            )),
        }
    }

    fn retire_ivc(
        &mut self,
        operation: OperationId,
        detach: ivc::IvcDetach<crate::HostPagingHandler>,
    ) -> AxVmResult {
        let run = self.run.as_ref().expect("IVC retirement run");
        if detach.vm() != self.shared.key() || detach.run() != run.id {
            return Err(AxVmError::StaleRun {
                expected: detach.run(),
                current: Some(run.id),
            });
        }
        let range = detach.range();
        let mapped = self
            .vm
            .resources
            .memory_leases
            .iter()
            .any(|lease| lease.range == range);
        let revision = if mapped {
            self.update_memory(operation, MemoryUpdate::Unmap(range))?
        } else {
            // Uninstalled reservations become eligible only during the
            // stop transaction, after both candidate roots were retired.
            if self.state != VmStatus::Stopping {
                return Err(AxVmError::invalid_state(
                    "retire IVC reservation",
                    "mapping retirement is not established",
                ));
            }
            self.run.as_ref().expect("retiring IVC run").revision
        };
        detach.commit(revision)
    }

    pub(super) fn teardown_ivc(&mut self, operation: OperationId) -> AxVmResult {
        let detached = self.shared.ivc.prepare_teardown(self.shared.key())?;
        for detach in detached {
            self.retire_ivc(operation, detach)?;
        }
        self.ivc_bindings.clear();
        Ok(())
    }
}
