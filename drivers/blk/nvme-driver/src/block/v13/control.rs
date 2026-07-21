use super::*;

pub(super) struct NvmeV13Control {
    name: &'static str,
    identity: NonZeroUsize,
    initialization: NvmeInitialization,
    hardware: NvmeV13Hardware,
    publication_failure: Option<NvmeV13PublicationFailure>,
    published: bool,
    recovery: NvmeV13Recovery,
}

struct NvmeV13Hardware {
    nvme: Nvme,
    irq: Arc<NvmeIrqState>,
    control_ledger: Arc<NvmeEvidenceLedger>,
    domains: Option<Vec<PreparedNvmeDomain>>,
    recovery_epochs: Vec<Arc<NvmeDomainRecoveryEpoch>>,
    reinitialize_queues: Vec<NvmeV13QueueReinitializeInfo>,
    queue_depth: NonZeroU16,
    source_id: IrqSourceId,
    source_bits: u64,
    admin_command_pending: bool,
    admin_completion_consumed: bool,
}

#[derive(Clone, Copy)]
struct NvmeV13QueueReinitializeInfo {
    queue: NvmeQueueReinitializeInfo,
    vector: u16,
}

enum NvmeV13PublicationFailure {
    Domain {
        failure: NvmeDomainBuildFailure,
        irq_owner: PreparedNvmeIrqOwner,
        built: Vec<IoDomainPart>,
        remaining: Vec<PreparedNvmeDomain>,
    },
    IoDomain {
        failure: IoDomainBuildFailure,
        built: Vec<IoDomainPart>,
        remaining: Vec<PreparedNvmeDomain>,
    },
    Publication(PublicationBuildFailure),
}

impl NvmeV13PublicationFailure {
    fn init_error(&self) -> InitError {
        match self {
            Self::Domain {
                failure,
                irq_owner,
                built,
                remaining,
            } => {
                let _ = (failure.error(), failure.retained_queue_count());
                let _ = (
                    matches!(irq_owner, PreparedNvmeIrqOwner::Independent(_)),
                    built.len(),
                    remaining.len(),
                );
                InitError::Hardware("NVMe ownership-domain publication is quarantined")
            }
            Self::IoDomain {
                failure,
                built,
                remaining,
            } => {
                let _ = failure.error();
                let _ = (built.len(), remaining.len());
                InitError::Hardware("NVMe I/O-domain publication is quarantined")
            }
            Self::Publication(failure) => {
                let _ = failure.error();
                InitError::Hardware("NVMe logical-device publication is quarantined")
            }
        }
    }
}

impl NvmeV13Control {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        name: &'static str,
        nvme: Nvme,
        irq: Arc<NvmeIrqState>,
        domains: Vec<PreparedNvmeDomain>,
        queue_depth: NonZeroU16,
        source_id: IrqSourceId,
    ) -> Self {
        let identity = nvme.controller_identity();
        let control_ledger = Arc::clone(
            &domains
                .first()
                .expect("a validated NVMe topology has a control domain")
                .ledger,
        );
        let domain_ids = domains.iter().map(|domain| domain.id).collect::<Vec<_>>();
        let recovery_epochs = domains
            .iter()
            .map(|domain| Arc::clone(&domain.recovery_epoch))
            .collect::<Vec<_>>();
        let source_bits = domains
            .iter()
            .fold(0_u64, |bits, domain| bits | (1_u64 << domain.source.get()));
        let mut reinitialize_queues = domains
            .iter()
            .flat_map(|domain| {
                let vector = domain.source.get() as u16;
                domain
                    .queues
                    .iter()
                    .map(move |queue| NvmeV13QueueReinitializeInfo {
                        queue: queue.reinitialize_info(),
                        vector,
                    })
            })
            .collect::<Vec<_>>();
        reinitialize_queues.sort_unstable_by_key(|queue| queue.queue.qid);
        Self {
            name,
            identity,
            initialization: NvmeInitialization::discovered(),
            hardware: NvmeV13Hardware {
                nvme,
                irq,
                control_ledger,
                domains: Some(domains),
                recovery_epochs,
                reinitialize_queues,
                queue_depth,
                source_id,
                source_bits,
                admin_command_pending: false,
                admin_completion_consumed: false,
            },
            publication_failure: None,
            published: false,
            recovery: NvmeV13Recovery::new(domain_ids),
        }
    }

    fn service_initialization_without_irq(
        &mut self,
        now_ns: u64,
        publication: &ControllerPublicationFactory<'_>,
    ) -> DriverControlPoll {
        let poll = self
            .initialization
            .poll(&mut self.hardware, InitInput::at(now_ns));
        DriverControlPoll::without_evidence(self.finish_init_poll(poll, publication))
    }

    fn service_initialization_irq(
        &mut self,
        now_ns: u64,
        evidence: IrqEvidenceId,
        publication: &ControllerPublicationFactory<'_>,
    ) -> DriverControlPoll {
        if self.published {
            return DriverControlPoll::after_irq(
                ControlProgress::Failed(InitError::InvalidState),
                EvidenceServiceResult::Recover(rdif_block::ControllerFault::Ownership),
            );
        }
        let ledger = Arc::clone(&self.hardware.control_ledger);
        let batch = match ledger.begin_service(evidence) {
            Ok(batch) => batch,
            Err(_) => {
                return DriverControlPoll::after_irq(
                    ControlProgress::Failed(InitError::InvalidState),
                    EvidenceServiceResult::Recover(rdif_block::ControllerFault::Ownership),
                );
            }
        };
        let facts = batch.facts();
        if facts.queue_bits() != 0 {
            let _ = ledger.finish_service(batch, facts);
            return DriverControlPoll::after_irq(
                ControlProgress::Failed(InitError::Hardware(
                    "NVMe I/O completion arrived before queue publication",
                )),
                EvidenceServiceResult::Recover(rdif_block::ControllerFault::Protocol),
            );
        }

        self.hardware.admin_completion_consumed = false;
        let mut sources = IdList::none();
        if facts.has_admin() {
            sources.insert(self.hardware.source_id.get());
        }
        let poll = self
            .initialization
            .poll(&mut self.hardware, InitInput::new(now_ns, sources));
        let retained = if facts.has_admin() && !self.hardware.admin_completion_consumed {
            NvmeEvidenceFacts::admin()
        } else {
            NvmeEvidenceFacts::default()
        };
        let disposition = ledger.finish_service(batch, retained);
        let evidence_result = match disposition {
            NvmeEvidenceDisposition::Drained => EvidenceServiceResult::Drained,
            NvmeEvidenceDisposition::Retained => EvidenceServiceResult::Retained,
            NvmeEvidenceDisposition::Invalid => {
                EvidenceServiceResult::Recover(rdif_block::ControllerFault::Ownership)
            }
        };
        if matches!(poll, InitPoll::Ready(_))
            && !matches!(evidence_result, EvidenceServiceResult::Drained)
        {
            return DriverControlPoll::after_irq(
                ControlProgress::Failed(InitError::Hardware(
                    "NVMe initialization completed with retained IRQ evidence",
                )),
                EvidenceServiceResult::Recover(rdif_block::ControllerFault::Protocol),
            );
        }
        DriverControlPoll::after_irq(self.finish_init_poll(poll, publication), evidence_result)
    }

    fn begin_control_quiesce(
        &mut self,
        now_ns: u64,
        _intent: QuiesceIntent,
        epoch: ControllerEpoch,
    ) -> DriverControlPoll {
        if !self.published || self.hardware.irq.delivery_enabled() {
            return DriverControlPoll::without_evidence(ControlProgress::Failed(
                InitError::InvalidState,
            ));
        }
        if let Err(error) = self.recovery.begin_quiesce(&mut self.hardware, epoch) {
            return DriverControlPoll::without_evidence(ControlProgress::Failed(error));
        }
        let poll = self
            .recovery
            .poll_quiesce(&mut self.hardware, InitInput::at(now_ns));
        DriverControlPoll::without_evidence(finish_quiesce_poll(poll))
    }

    fn begin_control_reinitialize(
        &mut self,
        now_ns: u64,
        quiesced: DmaQuiesced,
    ) -> DriverControlPoll {
        if !self.published {
            return DriverControlPoll::without_evidence(ControlProgress::Failed(
                InitError::InvalidState,
            ));
        }
        if let Err(error) = self
            .recovery
            .begin_reinitialize(&mut self.hardware, quiesced)
        {
            return DriverControlPoll::without_evidence(ControlProgress::Failed(error));
        }
        let poll = self
            .recovery
            .poll_reinitialize(&mut self.hardware, InitInput::at(now_ns));
        DriverControlPoll::without_evidence(self.finish_reinitialize_poll(poll))
    }

    fn service_published_without_irq(&mut self, now_ns: u64) -> DriverControlPoll {
        let progress = match self.recovery.phase() {
            NvmeV13RecoveryPhase::Quiescing => finish_quiesce_poll(
                self.recovery
                    .poll_quiesce(&mut self.hardware, InitInput::at(now_ns)),
            ),
            NvmeV13RecoveryPhase::Reinitializing => {
                let poll = self
                    .recovery
                    .poll_reinitialize(&mut self.hardware, InitInput::at(now_ns));
                self.finish_reinitialize_poll(poll)
            }
            NvmeV13RecoveryPhase::Running
            | NvmeV13RecoveryPhase::Quiesced
            | NvmeV13RecoveryPhase::GuestOwned
            | NvmeV13RecoveryPhase::Failed => ControlProgress::Failed(InitError::InvalidState),
        };
        DriverControlPoll::without_evidence(progress)
    }

    fn service_reinitialize_irq(
        &mut self,
        now_ns: u64,
        evidence: IrqEvidenceId,
    ) -> DriverControlPoll {
        if self.recovery.phase() != NvmeV13RecoveryPhase::Reinitializing {
            return DriverControlPoll::after_irq(
                ControlProgress::Failed(InitError::InvalidState),
                EvidenceServiceResult::Recover(rdif_block::ControllerFault::Ownership),
            );
        }
        let ledger = Arc::clone(&self.hardware.control_ledger);
        let batch = match ledger.begin_service(evidence) {
            Ok(batch) => batch,
            Err(_) => {
                return DriverControlPoll::after_irq(
                    ControlProgress::Failed(InitError::InvalidState),
                    EvidenceServiceResult::Recover(rdif_block::ControllerFault::Ownership),
                );
            }
        };
        let facts = batch.facts();
        if facts.queue_bits() != 0 || !facts.has_admin() {
            let _ = ledger.finish_service(batch, facts);
            return DriverControlPoll::after_irq(
                ControlProgress::Failed(InitError::Hardware(
                    "NVMe reinitialization received non-admin IRQ evidence",
                )),
                EvidenceServiceResult::Recover(rdif_block::ControllerFault::Protocol),
            );
        }

        self.hardware.admin_completion_consumed = false;
        let mut sources = IdList::none();
        sources.insert(self.hardware.source_id.get());
        let poll = self
            .recovery
            .poll_reinitialize(&mut self.hardware, InitInput::new(now_ns, sources));
        let retained = if self.hardware.admin_completion_consumed {
            NvmeEvidenceFacts::default()
        } else {
            NvmeEvidenceFacts::admin()
        };
        let disposition = ledger.finish_service(batch, retained);
        let evidence_result = evidence_result(disposition);
        if matches!(poll, InitPoll::Ready(_))
            && !matches!(evidence_result, EvidenceServiceResult::Drained)
        {
            return DriverControlPoll::after_irq(
                ControlProgress::Failed(InitError::Hardware(
                    "NVMe reinitialization completed with retained IRQ evidence",
                )),
                EvidenceServiceResult::Recover(rdif_block::ControllerFault::Protocol),
            );
        }
        DriverControlPoll::after_irq(self.finish_reinitialize_poll(poll), evidence_result)
    }

    fn finish_reinitialize_poll(
        &self,
        poll: InitPoll<rdif_block::ControllerReady>,
    ) -> ControlProgress {
        match poll {
            InitPoll::Ready(ready) => {
                match ControllerReinitialized::new(ready, self.recovery.domains()) {
                    Ok(reinitialized) => ControlProgress::Reinitialized(reinitialized),
                    Err(_) => ControlProgress::Failed(InitError::Hardware(
                        "NVMe reinitialization domain proof set is invalid",
                    )),
                }
            }
            InitPoll::Pending(schedule) => match control_schedule(schedule) {
                Ok(schedule) => ControlProgress::Pending(schedule),
                Err(error) => ControlProgress::Failed(error),
            },
            InitPoll::Failed(error) => ControlProgress::Failed(error),
        }
    }

    /// Consumes the controller-owned half of a shared INTx evidence after
    /// publication. The I/O domain consumes queue facts first and retains an
    /// admin fact under the same ID; this method never copies that identity.
    fn service_published_evidence(
        &mut self,
        evidence: IrqEvidenceId,
    ) -> Result<EvidenceServiceResult, BlkError> {
        let ledger = Arc::clone(&self.hardware.control_ledger);
        let batch = match ledger.begin_service(evidence) {
            Ok(batch) => batch,
            Err(_) => {
                return Ok(EvidenceServiceResult::Recover(
                    rdif_block::ControllerFault::Ownership,
                ));
            }
        };
        let facts = batch.facts();
        let retained_queues = NvmeEvidenceFacts::queues(facts.queue_bits());
        if !facts.has_admin() {
            let disposition = ledger.finish_service(batch, retained_queues);
            return Ok(evidence_result(disposition));
        }

        // No post-publication admin command is currently implemented. Still
        // consume the CQ fact so the shared evidence cannot remain permanently
        // retained, then fail closed instead of interpreting it as I/O.
        let completion = self.hardware.nvme.take_admin_completion();
        self.hardware.admin_command_pending = false;
        let retained = if completion.is_some() {
            retained_queues
        } else {
            retained_queues.with_admin()
        };
        let disposition = ledger.finish_service(batch, retained);
        let result = if completion.is_some() {
            EvidenceServiceResult::Recover(rdif_block::ControllerFault::Protocol)
        } else {
            evidence_result(disposition)
        };
        Ok(result)
    }

    fn finish_init_poll(
        &mut self,
        poll: InitPoll<()>,
        publication: &ControllerPublicationFactory<'_>,
    ) -> ControlProgress {
        match poll {
            InitPoll::Ready(()) => self.publish_ready(publication),
            InitPoll::Pending(schedule) => match control_schedule(schedule) {
                Ok(schedule) => ControlProgress::Pending(schedule),
                Err(error) => ControlProgress::Failed(error),
            },
            InitPoll::Failed(error) => ControlProgress::Failed(error),
        }
    }

    fn publish_ready(&mut self, publication: &ControllerPublicationFactory<'_>) -> ControlProgress {
        let Some(domains) = self.hardware.domains.take() else {
            return ControlProgress::Failed(InitError::InvalidState);
        };
        let (devices, routes) = match build_namespace_publication(
            self.name,
            self.hardware.nvme.namespace_if_ready(),
            self.hardware.nvme.dma_mask(),
            self.hardware.nvme.page_size(),
            self.hardware.nvme.max_transfer_bytes(),
        ) {
            Ok(publication) => publication,
            Err(error) => {
                self.hardware.domains = Some(domains);
                return ControlProgress::Failed(error);
            }
        };
        let mut all_descriptors = Vec::with_capacity(domains.len());
        for domain in &domains {
            let mut queue_descriptors = Vec::with_capacity(domain.queues.len());
            let mut sources = IdList::none();
            sources.insert(domain.source.get());
            for slot in 0..domain.queues.len() {
                let descriptor = match InterruptQueueDesc::new(
                    slot,
                    LogicalDeviceSelector::AllPublished,
                    domain.id,
                    QueueExecution::Tagged,
                    self.hardware.queue_depth,
                    sources,
                ) {
                    Ok(descriptor) => descriptor,
                    Err(_) => {
                        self.hardware.domains = Some(domains);
                        return ControlProgress::Failed(InitError::Hardware(
                            "NVMe final queue descriptor violated the selected activation plan",
                        ));
                    }
                };
                queue_descriptors.push(descriptor);
            }
            all_descriptors.push(queue_descriptors);
        }

        let mut built = Vec::with_capacity(domains.len());
        let mut remaining = domains.into_iter().zip(all_descriptors).peekable();
        while let Some((prepared, descriptors)) = remaining.next() {
            let PreparedNvmeDomain {
                id,
                source,
                ledger,
                recovery_epoch,
                irq_owner,
                queues,
            } = prepared;
            let queues = queues
                .into_iter()
                .map(PreparedNvmeOwnedQueue::into_owned)
                .collect::<Vec<NvmeOwnedQueue>>();
            let domain = match NvmeIoDomain::new(
                id,
                Arc::clone(&ledger),
                queues,
                routes.clone(),
                self.identity.get(),
                recovery_epoch,
            ) {
                Ok(domain) => domain,
                Err(failure) => {
                    self.publication_failure = Some(NvmeV13PublicationFailure::Domain {
                        failure,
                        irq_owner,
                        built,
                        remaining: remaining.map(|(domain, _)| domain).collect(),
                    });
                    return ControlProgress::Failed(InitError::Hardware(
                        "NVMe final ownership-domain topology is invalid",
                    ));
                }
            };
            let irq_sources = match irq_owner {
                PreparedNvmeIrqOwner::SharedControl => {
                    vec![IoDomainIrqSource::AlreadyBound(source)]
                }
                PreparedNvmeIrqOwner::Independent(source_owner) => {
                    vec![IoDomainIrqSource::New(DomainIrqSource::new(
                        source,
                        source_owner,
                    ))]
                }
            };
            let io_domain = match IoDomainPart::new(id, descriptors, irq_sources, Box::new(domain))
            {
                Ok(domain) => domain,
                Err(failure) => {
                    self.publication_failure = Some(NvmeV13PublicationFailure::IoDomain {
                        failure,
                        built,
                        remaining: remaining.map(|(domain, _)| domain).collect(),
                    });
                    return ControlProgress::Failed(InitError::Hardware(
                        "NVMe final I/O domain violated the selected activation plan",
                    ));
                }
            };
            built.push(io_domain);
        }
        match publication.publish(devices, built) {
            Ok(ready) => {
                self.published = true;
                ControlProgress::PublicationReady(ready)
            }
            Err(failure) => {
                self.publication_failure = Some(NvmeV13PublicationFailure::Publication(failure));
                ControlProgress::Failed(InitError::Hardware(
                    "NVMe final publication violated its activation contract",
                ))
            }
        }
    }
}

impl DriverGeneric for NvmeV13Control {
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

impl ControllerControl for NvmeV13Control {
    fn controller_identity(&self) -> NonZeroUsize {
        self.identity
    }

    fn service_control(
        &mut self,
        trigger: DriverControlTrigger,
        publication: &ControllerPublicationFactory<'_>,
    ) -> DriverControlPoll {
        if let Some(failure) = &self.publication_failure {
            return match trigger {
                DriverControlTrigger::Irq { .. } => DriverControlPoll::after_irq(
                    ControlProgress::Failed(failure.init_error()),
                    EvidenceServiceResult::Recover(rdif_block::ControllerFault::Ownership),
                ),
                _ => DriverControlPoll::without_evidence(ControlProgress::Failed(
                    failure.init_error(),
                )),
            };
        }
        match trigger {
            DriverControlTrigger::Start { now_ns }
            | DriverControlTrigger::InternalProgress { now_ns }
            | DriverControlTrigger::ProtocolDeadline { now_ns } => {
                if self.published {
                    self.service_published_without_irq(now_ns)
                } else {
                    self.service_initialization_without_irq(now_ns, publication)
                }
            }
            DriverControlTrigger::Irq { now_ns, evidence } => {
                if self.published {
                    self.service_reinitialize_irq(now_ns, evidence)
                } else {
                    self.service_initialization_irq(now_ns, evidence, publication)
                }
            }
            DriverControlTrigger::BeginQuiesce {
                now_ns,
                intent,
                epoch,
            } => self.begin_control_quiesce(now_ns, intent, epoch),
            DriverControlTrigger::BeginReinitialize { now_ns, quiesced } => {
                self.begin_control_reinitialize(now_ns, quiesced)
            }
        }
    }

    fn service_ready_evidence(
        &mut self,
        evidence: IrqEvidenceId,
    ) -> Result<EvidenceServiceResult, BlkError> {
        if !self.published || self.publication_failure.is_some() {
            return Ok(EvidenceServiceResult::Recover(
                rdif_block::ControllerFault::Ownership,
            ));
        }
        self.service_published_evidence(evidence)
    }

    fn commit_drained_evidence(
        &mut self,
        evidence: IrqEvidenceId,
    ) -> Result<DriverEvidenceRetirement, BlkError> {
        self.hardware
            .control_ledger
            .commit_drained_evidence(evidence)
            .map_err(|_| BlkError::Other("NVMe control evidence commit is invalid"))
    }

    fn retire_recovery_evidence(
        &mut self,
        permit: RecoveryEvidenceRetirePermit,
    ) -> Result<RecoveryEvidenceRetired, RecoveryEvidenceRetireFailure> {
        self.hardware
            .control_ledger
            .retire_after_quiesce(permit, self.identity)
    }

    fn lifecycle(&mut self) -> LifecycleEndpoint<'_> {
        LifecycleEndpoint::Interrupt(self)
    }

    fn enable_irq(&mut self) -> Result<(), BlkError> {
        if !self.published {
            if !self
                .hardware
                .irq
                .queue_source_live(self.hardware.source_id.get())
            {
                return Err(BlkError::Offline);
            }
            self.hardware.irq.enable_delivery();
            return Ok(());
        }
        if !self
            .hardware
            .irq
            .all_queue_sources_live(self.hardware.source_bits)
        {
            return Err(BlkError::Offline);
        }
        self.hardware.irq.arm_io_sources(self.hardware.source_bits);
        Ok(())
    }

    fn disable_irq(&mut self) -> Result<(), BlkError> {
        self.hardware.irq.disable_all();
        Ok(())
    }

    fn is_irq_enabled(&self) -> bool {
        self.hardware.irq.delivery_enabled()
    }
}

impl InitialHardware for NvmeV13Hardware {
    fn controller_timeout_ns(&self) -> u64 {
        self.nvme.controller_timeout_ns()
    }

    fn begin_controller_disable(&mut self) {
        self.nvme.begin_controller_disable();
    }

    fn controller_ready(&self) -> bool {
        self.nvme.controller_ready()
    }

    fn controller_fatal(&self) -> bool {
        self.nvme.controller_fatal()
    }

    fn live_admin_irq_source(&self) -> Option<usize> {
        (self.irq.delivery_enabled() && self.irq.queue_source_live(self.source_id.get()))
            .then_some(self.source_id.get())
    }

    unsafe fn prepare_initial_enable(&mut self) -> Result<(), InitError> {
        self.admin_command_pending = false;
        unsafe {
            // SAFETY: NvmeInitialization reaches this transition only after
            // observing CC.RDY=0 with the shared IRQ action already bound.
            self.nvme.prepare_initial_enable();
        }
        self.irq.unmask_for_activation(self.source_id.get())
    }

    fn submit_initial_admin(&mut self, command: InitialAdminCommand) -> Result<u16, InitError> {
        if self.admin_command_pending {
            return Err(InitError::InvalidState);
        }
        let command = self.nvme.build_initial_admin_command(command)?;
        let command_id = command.command_id();
        self.admin_command_pending = true;
        self.nvme.submit_admin_command(command);
        Ok(command_id)
    }

    fn take_admin_completion(&mut self) -> Option<AdminCompletion> {
        if !self.admin_command_pending {
            return None;
        }
        let completion = self.nvme.take_admin_completion()?;
        self.admin_command_pending = false;
        self.admin_completion_consumed = true;
        Some(AdminCompletion {
            command_id: completion.command_id,
            success: completion.status.is_success(),
            result: completion.result,
        })
    }

    fn complete_initial_admin(
        &mut self,
        command: InitialAdminCommand,
        completion: AdminCompletion,
    ) -> Result<Option<InitialAdminCommand>, InitError> {
        self.nvme
            .complete_initial_admin_discovering(command, completion)
    }

    fn publish_ready(&mut self) -> Result<(), InitError> {
        Ok(())
    }
}

impl NvmeV13Hardware {
    fn queue_for_reinitialize(
        &self,
        index: usize,
    ) -> Result<NvmeV13QueueReinitializeInfo, InitError> {
        self.reinitialize_queues
            .get(index)
            .copied()
            .ok_or(InitError::Hardware("missing retained NVMe v0.13 I/O queue"))
    }

    fn build_reinitialize_command(&self, command: AdminCommand) -> Result<CommandSet, InitError> {
        match command {
            AdminCommand::IdentifyController
            | AdminCommand::IdentifyNamespaceList
            | AdminCommand::IdentifyNamespace { .. } => {
                self.nvme.build_reidentify_admin_command(command)
            }
            AdminCommand::SetQueueCount { count } => {
                let queue_count = u32::try_from(count)
                    .ok()
                    .and_then(|count| count.checked_sub(1))
                    .ok_or(InitError::Hardware("invalid NVMe I/O queue count"))?;
                Ok(CommandSet::set_features_with_cid(
                    Feature::NumberOfQueues {
                        nsq: queue_count,
                        ncq: queue_count,
                    },
                    V13_ADMIN_COMMAND_ID,
                ))
            }
            AdminCommand::CreateCompletionQueue { queue_index } => {
                let entry = self.queue_for_reinitialize(queue_index)?;
                Ok(CommandSet::create_io_completion_queue_with_cid(
                    entry.queue.qid,
                    u32::try_from(entry.queue.cq_len)
                        .map_err(|_| InitError::Hardware("NVMe completion queue is too large"))?,
                    entry.queue.cq_bus_addr,
                    true,
                    true,
                    u32::from(entry.vector),
                    V13_ADMIN_COMMAND_ID,
                ))
            }
            AdminCommand::CreateSubmissionQueue { queue_index } => {
                let entry = self.queue_for_reinitialize(queue_index)?;
                Ok(CommandSet::create_io_submission_queue_with_cid(
                    entry.queue.qid,
                    u32::try_from(entry.queue.sq_len)
                        .map_err(|_| InitError::Hardware("NVMe submission queue is too large"))?,
                    entry.queue.sq_bus_addr,
                    true,
                    0,
                    entry.queue.qid,
                    0,
                    V13_ADMIN_COMMAND_ID,
                ))
            }
        }
    }

    fn complete_reinitialize_command(
        &self,
        command: AdminCommand,
        completion: AdminCompletion,
    ) -> Result<Option<AdminCommand>, InitError> {
        let queue_count = self.reinitialize_queues.len();
        match command {
            AdminCommand::IdentifyController => {
                self.nvme.validate_reidentified_controller()?;
                Ok(Some(AdminCommand::SetQueueCount { count: queue_count }))
            }
            AdminCommand::SetQueueCount { count } => {
                if count != queue_count || !queue_count_supported(completion.result, count) {
                    return Err(InitError::Hardware(
                        "NVMe controller did not restore the required v0.13 queue count",
                    ));
                }
                Ok(Some(AdminCommand::CreateCompletionQueue { queue_index: 0 }))
            }
            AdminCommand::CreateCompletionQueue { queue_index } => {
                Ok(Some(AdminCommand::CreateSubmissionQueue { queue_index }))
            }
            AdminCommand::CreateSubmissionQueue { queue_index } => {
                let next = queue_index.saturating_add(1);
                if next < queue_count {
                    Ok(Some(AdminCommand::CreateCompletionQueue {
                        queue_index: next,
                    }))
                } else {
                    Ok(Some(AdminCommand::IdentifyNamespaceList))
                }
            }
            AdminCommand::IdentifyNamespaceList => self
                .nvme
                .validate_reidentified_namespace_list_discovering()
                .map(|namespace| {
                    namespace.map(|namespace_id| AdminCommand::IdentifyNamespace { namespace_id })
                }),
            AdminCommand::IdentifyNamespace { namespace_id } => {
                self.nvme.validate_reidentified_namespace(namespace_id)?;
                Ok(None)
            }
        }
    }
}

impl LifecycleHardware for NvmeV13Hardware {
    fn controller_cookie(&self) -> usize {
        self.nvme.controller_identity().get()
    }

    fn controller_timeout_ns(&self) -> u64 {
        self.nvme.controller_timeout_ns()
    }

    fn begin_controller_disable(&mut self) {
        self.nvme.begin_controller_disable();
    }

    fn controller_ready(&self) -> bool {
        self.nvme.controller_ready()
    }

    fn controller_fatal(&self) -> bool {
        self.nvme.controller_fatal()
    }

    unsafe fn prepare_reinitialize(&mut self, quiesced: &DmaQuiesced) -> Result<(), InitError> {
        if quiesced.controller_cookie() != self.controller_cookie()
            || self
                .recovery_epochs
                .iter()
                .any(|receipt| !receipt.matches(quiesced.epoch()))
        {
            return Err(InitError::InvalidState);
        }
        self.admin_command_pending = false;
        self.admin_completion_consumed = false;
        unsafe {
            // SAFETY: every final domain published the exact proof epoch only
            // after resetting its SQ/CQ and CID state. The runtime consumed
            // that same linear proof here while queue admission and IRQ
            // actions remain closed.
            self.nvme.prepare_controller_reinitialize();
        }
        Ok(())
    }

    fn queue_count(&self) -> usize {
        self.reinitialize_queues.len()
    }

    fn admin_irq_source(&self) -> Option<usize> {
        self.irq
            .queue_source_live(self.source_id.get())
            .then_some(self.source_id.get())
    }

    fn submit_admin_command(&mut self, command: AdminCommand) -> Result<u16, InitError> {
        if self.admin_command_pending {
            return Err(InitError::InvalidState);
        }
        let command = self.build_reinitialize_command(command)?;
        let command_id = command.command_id();
        self.admin_command_pending = true;
        self.nvme.submit_admin_command(command);
        Ok(command_id)
    }

    fn take_admin_completion(&mut self) -> Option<AdminCompletion> {
        if !self.admin_command_pending {
            return None;
        }
        let completion = self.nvme.take_admin_completion()?;
        self.admin_command_pending = false;
        self.admin_completion_consumed = true;
        Some(AdminCompletion {
            command_id: completion.command_id,
            success: completion.status.is_success(),
            result: completion.result,
        })
    }

    fn complete_admin_command(
        &mut self,
        command: AdminCommand,
        completion: AdminCompletion,
    ) -> Result<Option<AdminCommand>, InitError> {
        self.complete_reinitialize_command(command, completion)
    }
}

impl InterruptLifecycle for NvmeV13Control {
    fn controller_cookie(&self) -> usize {
        self.identity.get()
    }

    fn begin_dma_quiesce(
        &mut self,
        epoch: ControllerEpoch,
        _cause: RecoveryCause,
    ) -> Result<(), InitError> {
        if self.hardware.irq.delivery_enabled() {
            return Err(InitError::InvalidState);
        }
        self.recovery.begin_quiesce(&mut self.hardware, epoch)
    }

    fn poll_dma_quiesce(&mut self, input: InitInput) -> InitPoll<rdif_block::DmaQuiesced> {
        self.recovery.poll_quiesce(&mut self.hardware, input)
    }

    fn enter_guest_owned(&mut self, quiesced: rdif_block::DmaQuiesced) -> Result<(), InitError> {
        self.recovery
            .enter_guest_owned(&mut self.hardware, quiesced)
    }

    fn begin_reinitialize(&mut self, quiesced: rdif_block::DmaQuiesced) -> Result<(), InitError> {
        self.recovery
            .begin_reinitialize(&mut self.hardware, quiesced)
    }

    fn poll_reinitialize(&mut self, input: InitInput) -> InitPoll<rdif_block::ControllerReady> {
        self.recovery.poll_reinitialize(&mut self.hardware, input)
    }
}

fn control_schedule(schedule: InitSchedule) -> Result<ControlSchedule, InitError> {
    ControlSchedule::new(
        schedule.run_again(),
        schedule.irq_sources(),
        schedule.wake_at_ns(),
    )
}

const fn evidence_result(disposition: NvmeEvidenceDisposition) -> EvidenceServiceResult {
    match disposition {
        NvmeEvidenceDisposition::Drained => EvidenceServiceResult::Drained,
        NvmeEvidenceDisposition::Retained => EvidenceServiceResult::Retained,
        NvmeEvidenceDisposition::Invalid => {
            EvidenceServiceResult::Recover(rdif_block::ControllerFault::Ownership)
        }
    }
}

fn finish_quiesce_poll(poll: InitPoll<DmaQuiesced>) -> ControlProgress {
    match poll {
        InitPoll::Ready(proof) => ControlProgress::DmaQuiesced(proof),
        InitPoll::Pending(schedule) => match control_schedule(schedule) {
            Ok(schedule) => ControlProgress::Pending(schedule),
            Err(error) => ControlProgress::Failed(error),
        },
        InitPoll::Failed(error) => ControlProgress::Failed(error),
    }
}

const V13_ADMIN_COMMAND_ID: u16 = 0;
