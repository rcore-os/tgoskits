//! Prepare translations in task context and retain both roots until retirement.

use std::sync::Arc;

use axaddrspace::{AddrSpace, LinearMapping};
use axvm_types::NestedPagingConfig;

use super::{Confirmation, Owner};
use crate::{
    AxVmError, AxVmResult, OperationId, VmStatus,
    arch::current::{ArchNestedPageTable, CurrentArch},
    architecture::ArchOps,
    guest_memory::{DecodeMemory, GuestRange, MappingLease, MemoryRevision, MemoryUpdate},
    runtime::vcpus::VcpuCommand,
};

/// Installation may have reached only some owners when it fails. This value
/// remains attached to the run, preserving its root and all new backing leases.
pub(super) struct PreparedMemoryUpdate {
    space: AddrSpace<ArchNestedPageTable>,
    leases: Vec<MappingLease>,
    decode: Arc<DecodeMemory>,
    revision: MemoryRevision,
    root: NestedPagingConfig,
}

impl Owner {
    fn prepare_memory_update(&self, update: MemoryUpdate) -> AxVmResult<PreparedMemoryUpdate> {
        let run = self
            .run
            .as_ref()
            .ok_or_else(|| AxVmError::invalid_state("update guest memory", "no active run"))?;
        if run.pending_memory.is_some() || run.memory_operation.is_some() {
            return Err(AxVmError::invalid_state(
                "update guest memory",
                "an installed translation transaction still needs cleanup",
            ));
        }
        let mut mappings = self
            .vm
            .resources
            .address_space
            .linear_mappings()
            .map_err(|error| AxVmError::from_addrspace("snapshot guest translations", error))?;
        let mut leases = self.vm.resources.memory_leases.clone();
        match update {
            MemoryUpdate::Map(lease) => {
                require_page_range(lease.range)?;
                if mappings
                    .iter()
                    .any(|mapping| overlaps(mapping.guest.as_usize(), mapping.size, lease.range))
                {
                    return Err(AxVmError::resource_conflict(
                        "guest mapping",
                        "new lease overlaps existing translations",
                    ));
                }
                mappings.push(LinearMapping {
                    guest: lease.range.start,
                    host: lease.host,
                    size: lease.range.length,
                    flags: lease.flags,
                });
                leases.push(lease);
            }
            MemoryUpdate::Unmap(range) => {
                require_page_range(range)?;
                let end = range.start.as_usize() + range.length;
                let covered = leases
                    .iter()
                    .filter(|lease| {
                        overlaps(lease.range.start.as_usize(), lease.range.length, range)
                    })
                    .map(|lease| {
                        let start = lease.range.start.as_usize().max(range.start.as_usize());
                        let stop = (lease.range.start.as_usize() + lease.range.length).min(end);
                        stop - start
                    })
                    .sum::<usize>();
                if covered != range.length {
                    return Err(AxVmError::invalid_config(
                        "unmap range must be fully backed by owned RAM leases",
                    ));
                }
                mappings = mappings
                    .into_iter()
                    .flat_map(|mapping| trim_mapping(mapping, range))
                    .collect();
                let mut retained = Vec::new();
                for lease in leases {
                    if !overlaps(lease.range.start.as_usize(), lease.range.length, range) {
                        retained.push(lease);
                        continue;
                    }
                    let start = lease.range.start.as_usize();
                    let stop = start + lease.range.length;
                    if start < range.start.as_usize() {
                        retained.push(lease.subrange(GuestRange::new(
                            start.into(),
                            range.start.as_usize() - start,
                        )?)?);
                    }
                    if stop > end {
                        retained.push(lease.subrange(GuestRange::new(end.into(), stop - end)?)?);
                    }
                }
                leases = retained;
            }
        }
        let old = self.vm.resources.nested_paging;
        let table = ArchNestedPageTable::new(old.levels)?;
        let mut space = AddrSpace::new_empty(
            table,
            self.vm.resources.address_space.base(),
            self.vm.resources.address_space.size(),
        )
        .map_err(|error| AxVmError::from_addrspace("prepare translation root", error))?;
        for mapping in mappings {
            space
                .map_linear(mapping.guest, mapping.host, mapping.size, mapping.flags)
                .map_err(|error| AxVmError::from_addrspace("prepare guest mapping", error))?;
        }
        let revision = MemoryRevision {
            run: run.id,
            sequence: run.revision.sequence.checked_add(1).ok_or_else(|| {
                AxVmError::resource_unavailable("memory revision", "revision generation exhausted")
            })?,
        };
        let root = NestedPagingConfig {
            root_paddr: space.page_table_root(),
            ..old
        };
        Ok(PreparedMemoryUpdate {
            space,
            decode: Arc::new(DecodeMemory::new(leases.clone())),
            leases,
            revision,
            root,
        })
    }

    pub(super) fn update_memory(
        &mut self,
        operation: OperationId,
        update: MemoryUpdate,
    ) -> AxVmResult<MemoryRevision> {
        // Physical DMA needs a separate driver/IOMMU revocation capability. No
        // observation timeout or guest CPU stop is a proof that it ended.
        if !self.vm.config().pass_through_devices().is_empty() {
            return Err(AxVmError::unsupported(
                "update guest memory",
                "assigned host devices do not provide DMA quiescence",
            ));
        }
        let prepared = self.prepare_memory_update(update)?;
        self.run
            .as_mut()
            .expect("prepared memory transaction")
            .memory_operation = Some(operation);
        let result = self.install_memory_update(operation, prepared);
        if let Some(run) = &mut self.run {
            run.memory_operation = None;
        }
        result
    }

    fn install_memory_update(
        &mut self,
        operation: OperationId,
        prepared: PreparedMemoryUpdate,
    ) -> AxVmResult<MemoryRevision> {
        let was_ready = self.state == VmStatus::Ready;
        let was_running = self.state == VmStatus::Running;
        let stopping = self.state == VmStatus::Stopping;
        if !stopping
            && !matches!(
                self.state,
                VmStatus::Running | VmStatus::Paused | VmStatus::Ready
            )
        {
            return Err(self.transition_error(VmStatus::Paused, "update memory"));
        }
        if was_running {
            self.pause(operation)?;
        } else if was_ready {
            let run = self.run.as_ref().expect("quiesce prepared devices");
            let quiet = run.ports.suspend().and_then(|()| {
                run.services
                    .devices()
                    .suspend_lifecycle_devices()
                    .map_err(|error| AxVmError::device("suspend prepared memory producers", error))
            });
            if let Err(error) = quiet {
                let rollback = run
                    .services
                    .devices()
                    .resume_lifecycle_devices()
                    .map_err(|failure| AxVmError::device("undo prepared device suspend", failure))
                    .and_then(|()| run.ports.resume());
                if let Err(rollback) = rollback {
                    let failure =
                        AxVmError::lifecycle_rollback("quiesce prepared memory", error, rollback);
                    self.record_failure(failure.clone());
                    return Err(failure);
                }
                return Err(error);
            }
        }
        let run = self.run.as_ref().expect("memory transaction run");
        run.admission
            .store(false, std::sync::atomic::Ordering::Release);
        run.services.memory().close();
        self.pump_until(|owner| {
            owner
                .run
                .as_ref()
                .expect("memory access owner")
                .services
                .memory()
                .quiescent()
        })?;
        self.run.as_mut().expect("installing memory").pending_memory = Some(prepared);
        let result: AxVmResult<MemoryRevision> = (|| {
            let run = self.run.as_mut().expect("installing memory");
            let prepared = run.pending_memory.as_ref().expect("retained prepared root");
            let mut instances = Vec::new();
            for participant in run
                .participants
                .values_mut()
                .filter(|participant| !participant.returned)
            {
                instances.push(participant.instance);
                participant
                    .root_installed
                    .request(participant.instance, operation);
                participant.port.send(VcpuCommand::InstallRoot {
                    operation,
                    root: prepared.root,
                    revision: prepared.revision,
                    decode: prepared.decode.clone(),
                })?;
            }
            self.wait_confirm(operation, &instances, Confirmation::RootInstalled)?;
            self.invalidate_root(self.vm.resources.nested_paging)?;
            let run = self.run.as_mut().expect("publishing memory");
            let prepared = run
                .pending_memory
                .as_ref()
                .expect("translation installation confirmed");
            run.services
                .memory()
                .publish(prepared.revision, prepared.leases.clone())?;
            let prepared = run
                .pending_memory
                .take()
                .expect("published translation revision");
            let old_space = std::mem::replace(&mut self.vm.resources.address_space, prepared.space);
            let old_leases =
                std::mem::replace(&mut self.vm.resources.memory_leases, prepared.leases);
            self.vm.resources.nested_paging = prepared.root;
            run.revision = prepared.revision;
            let old_decode = std::mem::replace(&mut run.decode, prepared.decode);
            // All owners use the new root and every enabled host CPU retired its
            // old translations. The old decode views retired with owner acks.
            drop((old_space, old_leases, old_decode));
            if !stopping {
                run.services.memory().reopen(run.revision)?;
            }
            Ok(run.revision)
        })();
        match result {
            Ok(revision) => {
                if was_running {
                    if let Err(error) = self.resume(operation) {
                        self.record_failure(error.clone());
                        return Err(error);
                    }
                } else if was_ready {
                    let run = self.run.as_ref().expect("resume prepared devices");
                    let resumed = run
                        .services
                        .devices()
                        .resume_lifecycle_devices()
                        .map_err(|error| {
                            AxVmError::device("resume prepared memory producers", error)
                        })
                        .and_then(|()| run.ports.resume());
                    if let Err(error) = resumed {
                        self.record_failure(error.clone());
                        return Err(error);
                    }
                }
                Ok(revision)
            }
            Err(error) => {
                self.record_failure(error.clone());
                // Both roots remain owned if any installation or retirement
                // failed. stop/destroy retries retirement after owner exits.
                Err(error)
            }
        }
    }

    pub(super) fn invalidate_root(&self, old_root: NestedPagingConfig) -> AxVmResult {
        let Some(run) = &self.run else {
            return Ok(());
        };
        struct Rendezvous<'a> {
            entry: &'a <CurrentArch as ArchOps>::Entry,
            root: NestedPagingConfig,
            result: AxVmResult,
        }
        unsafe fn retire(argument: *mut ()) {
            // SAFETY: run_on_cpu_sync retains the stack-owned rendezvous until
            // this callback returns. It invokes each callback synchronously and
            // sequentially, so the result slot is exclusively writable here.
            let rendezvous = unsafe { &mut *argument.cast::<Rendezvous<'_>>() };
            let irq = ax_std::os::arceos::guard::IrqSaveGuard::new();
            rendezvous.result =
                CurrentArch::invalidate_translations(rendezvous.entry, rendezvous.root);
            drop(irq);
        }
        let mask = crate::percpu::enabled_cpu_mask();
        for cpu in 0..usize::BITS as usize {
            if mask & (1usize << cpu) == 0 {
                continue;
            }
            let mut rendezvous = Rendezvous {
                entry: &run.architecture,
                root: old_root,
                result: Ok(()),
            };
            crate::host::task::run_on_cpu_sync(
                cpu,
                retire,
                (&mut rendezvous as *mut Rendezvous<'_>).cast(),
            )
            .map_err(|error| AxVmError::host("retire guest translations on CPU", error))?;
            rendezvous.result?;
        }
        Ok(())
    }

    pub(super) fn retire_failed_memory_update(&mut self) -> AxVmResult {
        let root = self
            .run
            .as_ref()
            .and_then(|run| run.pending_memory.as_ref())
            .map(|pending| pending.root);
        if let Some(root) = root {
            self.invalidate_root(root)?;
            // Every vCPU has exited; no backend can still consume this root.
            let pending = self
                .run
                .as_mut()
                .expect("retiring pending memory")
                .pending_memory
                .take();
            drop(pending);
        }
        Ok(())
    }
}

fn require_page_range(range: GuestRange) -> AxVmResult {
    if !range.start.as_usize().is_multiple_of(0x1000) || !range.length.is_multiple_of(0x1000) {
        return Err(AxVmError::invalid_config(
            "translation changes must use whole guest pages",
        ));
    }
    Ok(())
}

fn overlaps(start: usize, length: usize, range: GuestRange) -> bool {
    start < range.start.as_usize() + range.length && range.start.as_usize() < start + length
}

fn trim_mapping(mapping: LinearMapping, range: GuestRange) -> Vec<LinearMapping> {
    if !overlaps(mapping.guest.as_usize(), mapping.size, range) {
        return vec![mapping];
    }
    let start = mapping.guest.as_usize();
    let end = start + mapping.size;
    let cut_start = range.start.as_usize();
    let cut_end = cut_start + range.length;
    let mut kept = Vec::with_capacity(2);
    if start < cut_start {
        kept.push(LinearMapping {
            size: cut_start - start,
            ..mapping
        });
    }
    if cut_end < end {
        kept.push(LinearMapping {
            guest: cut_end.into(),
            host: mapping.host + (cut_end - start),
            size: end - cut_end,
            ..mapping
        });
    }
    kept
}
