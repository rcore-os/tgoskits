//! Two-phase NVMe controller activation for the rdif-block v0.13 boundary.

use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};
use core::{
    any::Any,
    num::{NonZeroU16, NonZeroU64, NonZeroUsize},
};

use dma_api::{DmaDomainId, DmaOp};
use mmio_api::{MmioAddr, MmioOp};
use rdif_block::{
    ActivationError, ActivationFailure, ActivationPlan, BlkError, BlockEvidenceSource,
    ControlDomainCapability, ControlProgress, ControlSchedule, ControllerActivator,
    ControllerCapabilities, ControllerControl, ControllerControlPart, ControllerEpoch,
    ControllerPublicationFactory, ControllerReinitialized, DmaQuiesced, DomainIrqSource,
    DriverControlPoll, DriverControlTrigger, DriverDeviceKey, DriverEvidenceRetirement,
    DriverGeneric, DriverLogicalDeviceDesc, DriverPrepareErrorCode, EvidenceServiceResult,
    HardwareQueueDepth, IdList, InitError, InitInput, InitPoll, InitSchedule, InterruptLifecycle,
    InterruptQueueDesc, IoDomainBuildFailure, IoDomainIrqSource, IoDomainPart, IrqEvidenceId,
    IrqSourceId, LifecycleEndpoint, LogicalDeviceConstraints, LogicalDeviceSelector,
    OwnershipDomainCapability, OwnershipDomainId, OwnershipDomainIds, PreparedControllerParts,
    PublicationBuildFailure, QueueExecution, QuiesceIntent, RecoveryCause,
    RecoveryEvidenceRetireFailure, RecoveryEvidenceRetirePermit, RecoveryEvidenceRetired,
};

use super::{
    NvmeIrqState, NvmeOwnedQueue, NvmeQueueReinitializeInfo, PreparedNvmeOwnedQueue,
    alloc_prp_lists, device_info,
    evidence_ledger::{NvmeEvidenceDisposition, NvmeEvidenceFacts, NvmeEvidenceLedger},
    hardware_limits,
    io_domain::{
        NvmeDomainBuildFailure, NvmeDomainRecoveryEpoch, NvmeIoDomain, NvmeNamespaceRoute,
    },
    new_vector_evidence_source,
};
use crate::{
    Config, Namespace,
    command::Feature,
    err::{Error, Result as NvmeResult},
    initialization::{InitialAdminCommand, InitialHardware, NvmeInitialization},
    lifecycle::{AdminCommand, AdminCompletion, LifecycleHardware, queue_count_supported},
    nvme::Nvme,
    queue::CommandSet,
};

mod control;
mod recovery;
mod topology;

use control::NvmeV13Control;
use recovery::{NvmeV13Recovery, NvmeV13RecoveryPhase};
use topology::*;

const DEFAULT_QUEUE_DEPTH: usize = 64;
const SHARED_INTX_SOURCE: usize = 0;

/// Discovered NVMe controller awaiting one runtime-selected ownership plan.
pub struct NvmeBlockActivator {
    name: &'static str,
    capabilities: ControllerCapabilities,
    nvme: Nvme,
    irq: Arc<NvmeIrqState>,
    topology: NvmeActivationTopology,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NvmeActivationTopology {
    mode: NvmeInterruptMode,
    domains: Vec<NvmeDomainSpec>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NvmeInterruptMode {
    SharedIntx,
    Msix,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NvmeDomainSpec {
    id: OwnershipDomainId,
    source: IrqSourceId,
    global_queue_slots: Vec<usize>,
}

struct SelectedNvmeTopology {
    queue_depth: usize,
    queue_count: usize,
    domains: Vec<NvmeDomainSpec>,
}

struct PreparedNvmeDomain {
    id: OwnershipDomainId,
    source: IrqSourceId,
    ledger: Arc<NvmeEvidenceLedger>,
    recovery_epoch: Arc<NvmeDomainRecoveryEpoch>,
    irq_owner: PreparedNvmeIrqOwner,
    queues: Vec<PreparedNvmeOwnedQueue>,
}

enum PreparedNvmeIrqOwner {
    SharedControl,
    Independent(BlockEvidenceSource),
}

impl NvmeBlockActivator {
    /// Discovers controller-wide resources without issuing a hardware command.
    pub fn discover(
        name: &'static str,
        bar_addr: impl Into<MmioAddr>,
        bar_size: usize,
        dma_mask: u64,
        dma_op: &'static dyn DmaOp,
        mmio_op: &'static dyn MmioOp,
        config: Config,
    ) -> NvmeResult<Self> {
        let nvme =
            Nvme::discover_for_activation(bar_addr, bar_size, dma_mask, dma_op, mmio_op, config)?;
        if !nvme.io_queue_interrupts_enabled() {
            return Err(Error::Unknown(
                "rdif-block v0.13 NVMe activation requires an interrupt source",
            ));
        }
        let max_queue_depth =
            super::controller::effective_queue_depth(DEFAULT_QUEUE_DEPTH, nvme.io_queue_entries())
                .ok_or(Error::Unknown(
                    "NVMe I/O queues cannot retain one in-flight request",
                ))?;
        let max_queue_depth = u16::try_from(max_queue_depth.get())
            .ok()
            .and_then(NonZeroU16::new)
            .ok_or(Error::Unknown("NVMe queue depth exceeds the v0.13 ABI"))?;
        let queue_depth = HardwareQueueDepth::new(NonZeroU16::MIN, max_queue_depth)?;
        let queue_count = nvme.io_queue_pair_capacity();
        let max_queues = u16::try_from(queue_count)
            .ok()
            .and_then(NonZeroU16::new)
            .ok_or(Error::Unknown("NVMe exposes an invalid I/O queue count"))?;
        let interrupt_mode = if nvme.msix_interrupts_enabled() {
            NvmeInterruptMode::Msix
        } else {
            NvmeInterruptMode::SharedIntx
        };
        let (capabilities, topology) = build_interrupt_capabilities(
            nvme.controller_identity(),
            nvme.dma_mask(),
            queue_depth,
            max_queues,
            interrupt_mode,
            nvme.interrupt_vectors(),
        )?;
        let configured_vectors = topology
            .domains
            .iter()
            .map(|domain| domain.source.get() as u16)
            .collect::<Vec<_>>();
        let irq = Arc::new(NvmeIrqState::new(
            nvme.interrupt_port(),
            &configured_vectors,
            matches!(interrupt_mode, NvmeInterruptMode::Msix),
        ));

        Ok(Self {
            name,
            capabilities,
            nvme,
            irq,
            topology,
        })
    }
}

impl DriverGeneric for NvmeBlockActivator {
    fn name(&self) -> &str {
        self.name
    }

    fn raw_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
}

impl ControllerActivator for NvmeBlockActivator {
    fn capabilities(&self) -> &ControllerCapabilities {
        &self.capabilities
    }

    fn activate(
        mut self: Box<Self>,
        plan: ActivationPlan,
    ) -> Result<PreparedControllerParts, ActivationFailure> {
        if plan.controller_identity() != self.capabilities.controller_identity() {
            return Err(ActivationFailure::new(
                ActivationError::ControllerIdentityMismatch,
                self,
            ));
        }
        let selected = match select_activation_topology(&self.topology, &plan) {
            Ok(selected) => selected,
            Err(error) => return Err(ActivationFailure::new(error, self)),
        };

        // Resource preparation occurs only after the runtime selected the
        // topology, but before that choice mutates the retained controller.
        // A failure therefore drops only hardware-invisible memory and can
        // return the original activator for retry or explicit quarantine.
        let mut queue_prp_lists = Vec::with_capacity(selected.queue_count);
        for _ in 0..selected.queue_count {
            let prp_lists = match alloc_prp_lists(&self.nvme, selected.queue_depth) {
                Ok(prp_lists) => prp_lists,
                Err(error) => {
                    return Err(ActivationFailure::new(
                        ActivationError::DriverPreparationFailed {
                            code: prepare_error_code(&error),
                        },
                        self,
                    ));
                }
            };
            queue_prp_lists.push(prp_lists);
        }
        let prepared_hardware_queues = match self
            .nvme
            .prepare_selected_io_queues(selected.queue_count, selected.queue_depth)
        {
            Ok(queues) => queues,
            Err(error) => {
                return Err(ActivationFailure::new(
                    ActivationError::DriverPreparationFailed {
                        code: prepare_error_code(&error),
                    },
                    self,
                ));
            }
        };
        for (slot, queue) in prepared_hardware_queues.queues().iter().enumerate() {
            if domain_slot_for_hardware_qid(queue.qid) != Some(slot) {
                return Err(ActivationFailure::new(
                    ActivationError::DriverPreparationFailed {
                        code: DriverPrepareErrorCode::InvalidState,
                    },
                    self,
                ));
            }
        }

        let admin_probe = self.nvme.admin_completion_probe();
        let queue_probes = prepared_hardware_queues
            .queues()
            .iter()
            .map(|queue| queue.completion_probe())
            .collect::<Vec<_>>();
        let mut prepared_sources = Vec::with_capacity(selected.domains.len());
        for (domain_index, domain) in selected.domains.iter().enumerate() {
            let probes = domain
                .global_queue_slots
                .iter()
                .copied()
                .enumerate()
                .map(|(local_slot, global_slot)| (local_slot, queue_probes[global_slot].clone()))
                .collect();
            let source = match new_vector_evidence_source(
                Arc::clone(&self.irq),
                domain.source,
                domain_index as u16,
                (domain_index == 0).then(|| admin_probe.clone()),
                probes,
            ) {
                Ok(source) => source,
                Err(error) => {
                    return Err(ActivationFailure::new(
                        ActivationError::DriverPreparationFailed {
                            code: block_prepare_error_code(error),
                        },
                        self,
                    ));
                }
            };
            prepared_sources.push(Some(source));
        }
        let hardware_queues = prepared_hardware_queues.commit(&mut self.nvme);
        let mut queue_owners = hardware_queues
            .into_iter()
            .zip(queue_prp_lists)
            .map(Some)
            .collect::<Vec<_>>();
        let mut prepared_domains = Vec::with_capacity(selected.domains.len());
        let mut control_source = None;
        for (domain_index, domain) in selected.domains.iter().enumerate() {
            let (irq_source, ledger) = prepared_sources[domain_index]
                .take()
                .expect("one evidence source was prepared for each NVMe domain");
            let irq_owner = if domain_index == 0 {
                control_source = Some(irq_source);
                PreparedNvmeIrqOwner::SharedControl
            } else {
                PreparedNvmeIrqOwner::Independent(irq_source)
            };
            let mut queues = Vec::with_capacity(domain.global_queue_slots.len());
            let mut queue_sources = IdList::none();
            queue_sources.insert(domain.source.get());
            for (local_slot, global_slot) in domain.global_queue_slots.iter().copied().enumerate() {
                let (queue, prp_lists) = queue_owners[global_slot]
                    .take()
                    .expect("a selected NVMe queue belongs to exactly one domain");
                queues.push(PreparedNvmeOwnedQueue::new(
                    local_slot,
                    selected.queue_depth,
                    self.name,
                    self.nvme.dma_mask(),
                    self.nvme.page_size(),
                    queue_sources,
                    queue,
                    prp_lists,
                ));
            }
            prepared_domains.push(PreparedNvmeDomain {
                id: domain.id,
                source: domain.source,
                ledger,
                recovery_epoch: Arc::new(NvmeDomainRecoveryEpoch::new()),
                irq_owner,
                queues,
            });
        }
        debug_assert!(queue_owners.iter().all(Option::is_none));
        let control_domain = prepared_domains
            .first()
            .expect("a selected NVMe topology has a control domain")
            .id;
        let control_source_id = prepared_domains[0].source;
        let control_source =
            control_source.expect("the shared control domain owns its IRQ endpoint");
        let Self {
            name,
            capabilities: _,
            nvme,
            irq,
            topology: _,
        } = *self;
        let control = NvmeV13Control::new(
            name,
            nvme,
            irq,
            prepared_domains,
            NonZeroU16::new(selected.queue_depth as u16)
                .expect("the activation plan selected nonzero depth"),
            control_source_id,
        );
        let control_part = match ControllerControlPart::new_shared(
            control_domain,
            vec![DomainIrqSource::new(control_source_id, control_source)],
            Box::new(control),
        ) {
            Ok(control) => control,
            Err(failure) => return Err(ActivationFailure::control_part(plan, failure)),
        };
        PreparedControllerParts::new(plan, control_part).map_err(ActivationFailure::prepared)
    }
}

#[cfg(test)]
mod tests;
