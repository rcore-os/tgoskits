//! Freeze, source stop, DMA quiesce, reclaim, and source re-arm phases.

use super::*;

impl ControlLifecycle {
    pub(in crate::block::activation_v13::owner) fn accept_contained_source_fault(
        &mut self,
        published: &RdifBlockPublishedOwner,
        control_io: &ControlIoRuntime,
        sources: &mut Vec<BoundEvidenceSource>,
        source_index: usize,
        fault: PendingSourceFault,
    ) -> Result<(), ControlLifecycleError> {
        let phase = self.coordinator.snapshot().phase();
        if !matches!(
            phase,
            ShutdownPhase::Running
                | ShutdownPhase::Freezing
                | ShutdownPhase::DispatchStopped
                | ShutdownPhase::DeviceMasked
        ) {
            return Err(ControlLifecycleError::LateSourceFault {
                _fault: Box::new(fault),
            });
        }
        if phase == ShutdownPhase::Running {
            self.request_recovery(control_io, ControllerFault::Ownership)?;
        }
        let route = control_io.evidence_route(fault.source);
        let source = sources.remove(source_index);
        let quiesced = source
            .suspend_contained_fault_after_mask(
                fault,
                published.published().control().controller_identity(),
                self.next_quiesce_epoch,
                route,
            )
            .map_err(|failure| ControlLifecycleError::ContainedSourceSuspend(Box::new(failure)))?;
        self.contained_fault_sources.push(quiesced);
        Ok(())
    }

    pub(super) fn advance_freezing(
        &mut self,
        control_io: &ControlIoRuntime,
    ) -> Result<ControlLifecycleAdvance, ControlLifecycleError> {
        let coordinator = Arc::clone(&self.coordinator);
        let participant = self.participant;
        let transaction = self.active_transaction_mut("freezing")?;
        let ControlStage::Freezing { acknowledged } = &mut transaction.stage else {
            return Err(ControlLifecycleError::LocalStateMismatch("freezing"));
        };
        if !*acknowledged
            && control_io
                .try_commit_quiesced()
                .map_err(ControlLifecycleError::RequestLifecycle)?
        {
            coordinator
                .ack_dispatch_cutoff(participant)
                .map_err(ControlLifecycleError::Coordinator)?;
            *acknowledged = true;
        }
        let snapshot = coordinator.snapshot();
        if acknowledgement_complete(snapshot.dispatch_cutoff(), snapshot.participant_count()) {
            coordinator
                .finish_dispatch_stopped(participant)
                .map_err(ControlLifecycleError::Coordinator)?;
        }
        Ok(ControlLifecycleAdvance::Pending)
    }

    pub(super) fn mask_device_sources(
        &mut self,
        published: &mut RdifBlockPublishedOwner,
    ) -> Result<ControlLifecycleAdvance, ControlLifecycleError> {
        let intent = self.active_transaction("mask device sources")?.intent;
        published
            .published_mut()
            .control_mut()
            .disable_irq()
            .map_err(ControlLifecycleError::Driver)?;
        published
            .disable_binding_irq()
            .map_err(ControlLifecycleError::Binding)?;
        self.coordinator
            .mark_device_masked(self.participant)
            .map_err(ControlLifecycleError::Coordinator)?;
        let contained_faults = core::mem::take(&mut self.contained_fault_sources);
        let sources = match intent {
            QuiesceIntent::Shutdown => SourceStopState::Shutdown(ShutdownSourceStop {
                closed_recovery: Vec::new(),
                contained_faults,
            }),
            QuiesceIntent::Recovery(_) => SourceStopState::Recovery(contained_faults),
            QuiesceIntent::OwnershipTransfer => {
                return Err(ControlLifecycleError::UnsupportedIntent(intent));
            }
        };
        self.active_transaction_mut("start source stop")?.stage = ControlStage::StoppingSources {
            sources,
            acknowledged: false,
        };
        self.wake_children()?;
        Ok(ControlLifecycleAdvance::Pending)
    }

    pub(super) fn advance_source_stop(
        &mut self,
        published: &RdifBlockPublishedOwner,
        control_io: &ControlIoRuntime,
        sources: &mut Vec<BoundEvidenceSource>,
    ) -> Result<ControlLifecycleAdvance, ControlLifecycleError> {
        if !matches!(
            self.active_transaction("stop IRQ sources")?.stage,
            ControlStage::StoppingSources { .. }
        ) {
            return Err(ControlLifecycleError::LocalStateMismatch(
                "stop IRQ sources",
            ));
        }
        let mut newly_contained = core::mem::take(&mut self.contained_fault_sources);
        let controller_identity = published.published().control().controller_identity();
        let quiesce_epoch = self.next_quiesce_epoch;
        let participant = self.participant;
        let coordinator = Arc::clone(&self.coordinator);
        let transaction = self.active_transaction_mut("stop IRQ sources")?;
        let ControlStage::StoppingSources {
            sources: stopped,
            acknowledged,
        } = &mut transaction.stage
        else {
            return Err(ControlLifecycleError::LocalStateMismatch(
                "stop IRQ sources",
            ));
        };
        match stopped {
            SourceStopState::Shutdown(state) => state.contained_faults.append(&mut newly_contained),
            SourceStopState::Recovery(quiesced) => quiesced.append(&mut newly_contained),
        }
        if !*acknowledged {
            let mut serviced = 0;
            while serviced < LIFECYCLE_BUDGET {
                let Some(source) = sources.pop() else {
                    break;
                };
                if let Some(fault) = source.take_fault() {
                    let route = control_io.evidence_route(fault.source);
                    let quiesced = source
                        .suspend_contained_fault_after_mask(
                            fault,
                            controller_identity,
                            quiesce_epoch,
                            route,
                        )
                        .map_err(|failure| {
                            ControlLifecycleError::ContainedSourceSuspend(Box::new(failure))
                        })?;
                    match stopped {
                        SourceStopState::Shutdown(state) => state.contained_faults.push(quiesced),
                        SourceStopState::Recovery(sources) => sources.push(quiesced),
                    }
                    serviced += 1;
                    continue;
                }
                let result = match stopped {
                    SourceStopState::Shutdown(state) => match source.close_after_mask() {
                        Ok(ClosedSourceDisposition::Closed) => Ok(()),
                        Ok(ClosedSourceDisposition::Recovery(source)) => {
                            state.closed_recovery.push(source);
                            Ok(())
                        }
                        Err(failure) => Err(failure),
                    },
                    SourceStopState::Recovery(quiesced) => match source.suspend_after_mask(
                        controller_identity,
                        quiesce_epoch,
                    ) {
                        Ok(source) => {
                            quiesced.push(source);
                            Ok(())
                        }
                        Err(failure) => Err(failure),
                    },
                };
                if let Err(failure) = result {
                    let (reason, source) = failure.into_parts();
                    sources.push(source);
                    return Err(ControlLifecycleError::SourceClose(reason));
                }
                serviced += 1;
            }
            if !sources.is_empty() {
                return Ok(ControlLifecycleAdvance::Pending);
            }
            coordinator
                .ack_sources_closed(participant)
                .map_err(ControlLifecycleError::Coordinator)?;
            *acknowledged = true;
        }
        let snapshot = coordinator.snapshot();
        if acknowledgement_complete(snapshot.sources_closed(), snapshot.participant_count()) {
            coordinator
                .finish_sources_closed(participant)
                .map_err(ControlLifecycleError::Coordinator)?;
            let stopped = match &mut self.active_transaction_mut("commit source stop")?.stage {
                ControlStage::StoppingSources { sources, .. } => {
                    match core::mem::replace(
                        sources,
                        SourceStopState::Shutdown(ShutdownSourceStop {
                            closed_recovery: Vec::new(),
                            contained_faults: Vec::new(),
                        }),
                    ) {
                        SourceStopState::Shutdown(sources) => {
                            let contained_faults = (!sources.contained_faults.is_empty())
                                .then(|| QuiescedSourceBatch::new(sources.contained_faults));
                            StoppedSources::Shutdown(ShutdownStoppedSources {
                                closed_recovery: sources.closed_recovery,
                                contained_faults,
                                terminal_close: None,
                            })
                        }
                        SourceStopState::Recovery(sources) => {
                            StoppedSources::Recovery(QuiescedSourceBatch::new(sources))
                        }
                    }
                }
                _ => {
                    return Err(ControlLifecycleError::LocalStateMismatch(
                        "commit source stop",
                    ));
                }
            };
            self.active_transaction_mut("enter controller quiesce")?
                .stage = ControlStage::Quiescing {
                sources: stopped,
                started: false,
                schedule: None,
            };
        }
        Ok(ControlLifecycleAdvance::Pending)
    }

    pub(super) fn advance_quiesce(
        &mut self,
        published: &mut RdifBlockPublishedOwner,
    ) -> Result<ControlLifecycleAdvance, ControlLifecycleError> {
        for _ in 0..LIFECYCLE_BUDGET {
            let now_ns = ax_hal::time::monotonic_time_nanos();
            let intent = self.active_transaction("drive controller quiesce")?.intent;
            let trigger = {
                let transaction = self.active_transaction_mut("drive controller quiesce")?;
                let ControlStage::Quiescing {
                    started, schedule, ..
                } = &mut transaction.stage
                else {
                    return Err(ControlLifecycleError::LocalStateMismatch(
                        "drive controller quiesce",
                    ));
                };
                if !*started {
                    *started = true;
                    ControlTrigger::BeginQuiesce {
                        now_ns,
                        intent,
                        epoch: self.next_quiesce_epoch,
                    }
                } else {
                    let Some(schedule) = *schedule else {
                        return Ok(ControlLifecycleAdvance::Pending);
                    };
                    if !schedule.irq_sources().is_empty() {
                        return Err(ControlLifecycleError::IrqWaitAfterSourcesStopped(schedule));
                    }
                    if schedule.internal_progress_ready() {
                        ControlTrigger::InternalProgress { now_ns }
                    } else if schedule.wake_at_ns().is_some_and(|at| at <= now_ns) {
                        ControlTrigger::ProtocolDeadline { now_ns }
                    } else {
                        return Ok(ControlLifecycleAdvance::Pending);
                    }
                }
            };
            let poll = published
                .published_mut()
                .control_mut()
                .service_control(trigger)
                .map_err(|failure| ControlLifecycleError::ControlService(Box::new(failure)))?;
            let (progress, evidence) = poll.into_parts();
            if let Some(evidence) = evidence {
                return Err(ControlLifecycleError::UnexpectedEvidence(evidence));
            }
            match progress {
                ControlProgress::Pending(schedule) => {
                    let transaction = self.active_transaction_mut("record quiesce schedule")?;
                    let ControlStage::Quiescing {
                        schedule: current, ..
                    } = &mut transaction.stage
                    else {
                        return Err(ControlLifecycleError::LocalStateMismatch(
                            "record quiesce schedule",
                        ));
                    };
                    *current = Some(schedule);
                }
                ControlProgress::DmaQuiesced(proof) => {
                    self.coordinator
                        .publish_dma_quiesced(self.participant, proof)
                        .map_err(ControlLifecycleError::PublishDmaProof)?;
                    let sources = match &mut self
                        .active_transaction_mut("publish controller DMA proof")?
                        .stage
                    {
                        ControlStage::Quiescing { sources, .. } => {
                            let placeholder = StoppedSources::Shutdown(ShutdownStoppedSources {
                                closed_recovery: Vec::new(),
                                contained_faults: None,
                                terminal_close: None,
                            });
                            core::mem::replace(sources, placeholder)
                        }
                        _ => {
                            return Err(ControlLifecycleError::LocalStateMismatch(
                                "publish controller DMA proof",
                            ));
                        }
                    };
                    self.active_transaction_mut("enter owner reclaim")?.stage =
                        ControlStage::Reclaiming {
                            sources: Some(sources),
                            lease: None,
                            reclaimed: None,
                            io_reclaimed: false,
                            acknowledged: false,
                        };
                    self.wake_children()?;
                    return Ok(ControlLifecycleAdvance::Pending);
                }
                ControlProgress::Failed(error) => {
                    return Err(ControlLifecycleError::DriverControl(error));
                }
                progress @ (ControlProgress::PublicationReady(_)
                | ControlProgress::Reinitialized(_)) => {
                    return Err(ControlLifecycleError::UnexpectedProgress(Box::new(
                        progress,
                    )));
                }
            }
        }
        Ok(ControlLifecycleAdvance::Pending)
    }

    pub(super) fn advance_reclaim(
        &mut self,
        published: &mut RdifBlockPublishedOwner,
        control_io: &mut ControlIoRuntime,
    ) -> Result<ControlLifecycleAdvance, ControlLifecycleError> {
        let participant = self.participant;
        let coordinator = Arc::clone(&self.coordinator);
        let transaction = self.active_transaction_mut("reclaim controller owner")?;
        let ControlStage::Reclaiming {
            sources,
            lease,
            reclaimed,
            io_reclaimed,
            acknowledged,
        } = &mut transaction.stage
        else {
            return Err(ControlLifecycleError::LocalStateMismatch(
                "reclaim controller owner",
            ));
        };
        if lease.is_none() {
            *lease = Some(
                coordinator
                    .borrow_dma_quiesced(participant)
                    .map_err(ControlLifecycleError::Coordinator)?,
            );
        }
        let proof = lease
            .as_ref()
            .ok_or(ControlLifecycleError::MissingDmaLease)?
            .proof();
        if reclaimed.is_none() {
            let stopped = sources
                .take()
                .ok_or(ControlLifecycleError::MissingStoppedSources)?;
            match stopped {
                StoppedSources::Shutdown(mut stopped) => {
                    if !retire_domain_recovery_sources(
                        &mut stopped.closed_recovery,
                        proof,
                        |route, permit| {
                            control_io.retire_recovery_evidence(published, route, permit)
                        },
                    )
                        .map_err(ControlLifecycleError::RecoveryRetire)?
                    {
                        *sources = Some(StoppedSources::Shutdown(stopped));
                        return Ok(ControlLifecycleAdvance::Pending);
                    }
                    if let Some(batch) = stopped.contained_faults.take() {
                        match batch.advance(proof, lifecycle_budget(), |route, permit| {
                            control_io.retire_recovery_evidence(published, route, permit)
                        }) {
                            Ok(QuiescedSourceBatchProgress::More(batch)) => {
                                stopped.contained_faults = Some(batch);
                                *sources = Some(StoppedSources::Shutdown(stopped));
                                return Ok(ControlLifecycleAdvance::Pending);
                            }
                            Ok(QuiescedSourceBatchProgress::Ready(batch)) => {
                                stopped.terminal_close =
                                    Some(batch.choose_terminal_close().map_err(|failure| {
                                        ControlLifecycleError::TerminalSourceChoice(Box::new(
                                            failure,
                                        ))
                                    })?);
                            }
                            Err(failure) => {
                                let (reason, batch) = failure.into_parts();
                                stopped.contained_faults = Some(batch);
                                *sources = Some(StoppedSources::Shutdown(stopped));
                                return Err(ControlLifecycleError::RecoveryRetire(reason));
                            }
                        }
                    }
                    if let Some(batch) = stopped.terminal_close.take() {
                        match batch.advance(lifecycle_budget()) {
                            Ok(SourceCloseBatchProgress::More(batch)) => {
                                stopped.terminal_close = Some(batch);
                                *sources = Some(StoppedSources::Shutdown(stopped));
                                return Ok(ControlLifecycleAdvance::Pending);
                            }
                            Ok(SourceCloseBatchProgress::Closed) => {}
                            Err(failure) => {
                                return Err(ControlLifecycleError::TerminalSourceClose(Box::new(
                                    failure,
                                )));
                            }
                        }
                    }
                    *reclaimed = Some(ReclaimedSources::Shutdown);
                }
                StoppedSources::Recovery(batch) => match batch.advance(
                    proof,
                    lifecycle_budget(),
                    |route, permit| {
                        control_io.retire_recovery_evidence(published, route, permit)
                    },
                ) {
                    Ok(QuiescedSourceBatchProgress::More(batch)) => {
                        *sources = Some(StoppedSources::Recovery(batch));
                        return Ok(ControlLifecycleAdvance::Pending);
                    }
                    Ok(QuiescedSourceBatchProgress::Ready(batch)) => {
                        *reclaimed = Some(ReclaimedSources::Recovery(batch));
                    }
                    Err(failure) => {
                        let (reason, batch) = failure.into_parts();
                        *sources = Some(StoppedSources::Recovery(batch));
                        return Err(ControlLifecycleError::RecoveryRetire(reason));
                    }
                },
            }
        }
        if !*io_reclaimed {
            control_io
                .reclaim_for_recovery(published, proof)
                .map_err(ControlLifecycleError::ControlIoReclaim)?;
            if matches!(reclaimed, Some(ReclaimedSources::Shutdown)) {
                control_io
                    .close_reclaimed(published)
                    .map_err(ControlLifecycleError::ControlIoReclaim)?;
            }
            *io_reclaimed = true;
        }
        if !*acknowledged {
            let lease = lease.take().ok_or(ControlLifecycleError::MissingDmaLease)?;
            coordinator
                .ack_reclaimed(lease)
                .map_err(ControlLifecycleError::ReclaimAck)?;
            *acknowledged = true;
        }
        let snapshot = coordinator.snapshot();
        if acknowledgement_complete(snapshot.reclaimed(), snapshot.participant_count()) {
            coordinator
                .finish_reclaimed(participant)
                .map_err(ControlLifecycleError::Coordinator)?;
            let sources = match &mut self.active_transaction_mut("commit owner reclaim")?.stage {
                ControlStage::Reclaiming { reclaimed, .. } => reclaimed
                    .take()
                    .ok_or(ControlLifecycleError::MissingReclaimedSources)?,
                _ => {
                    return Err(ControlLifecycleError::LocalStateMismatch(
                        "commit owner reclaim",
                    ));
                }
            };
            self.active_transaction_mut("enter reclaimed milestone")?
                .stage = ControlStage::Reclaimed {
                sources: Some(sources),
            };
        }
        Ok(ControlLifecycleAdvance::Pending)
    }

    pub(super) fn advance_reclaimed(
        &mut self,
    ) -> Result<ControlLifecycleAdvance, ControlLifecycleError> {
        let participant = self.participant;
        let sources = match &mut self.active_transaction_mut("finish reclaim phase")?.stage {
            ControlStage::Reclaimed { sources } => sources
                .take()
                .ok_or(ControlLifecycleError::MissingReclaimedSources)?,
            _ => {
                return Err(ControlLifecycleError::LocalStateMismatch(
                    "finish reclaim phase",
                ));
            }
        };
        let proof = match self.coordinator.take_dma_quiesced(participant) {
            Ok(proof) => proof,
            Err(error) => {
                self.active_transaction_mut("restore reclaimed sources")?
                    .stage = ControlStage::Reclaimed {
                    sources: Some(sources),
                };
                return Err(ControlLifecycleError::Coordinator(error));
            }
        };
        match sources {
            ReclaimedSources::Shutdown => {
                drop(proof);
                self.coordinator
                    .finish_closed(participant)
                    .map_err(ControlLifecycleError::Coordinator)?;
                self.state = ControlOwnerState::Closed;
                Ok(ControlLifecycleAdvance::Closed)
            }
            ReclaimedSources::Recovery(rearm) => {
                self.coordinator
                    .begin_reinit_sources(participant)
                    .map_err(ControlLifecycleError::Coordinator)?;
                self.active_transaction_mut("enter source re-arm")?.stage =
                    ControlStage::ReinitSources {
                        proof: Some(proof),
                        rearm: Some(rearm),
                        acknowledged: false,
                        binding_enabled: false,
                    };
                self.wake_children()?;
                Ok(ControlLifecycleAdvance::Pending)
            }
        }
    }

    pub(super) fn advance_source_rearm(
        &mut self,
        published: &mut RdifBlockPublishedOwner,
        sources: &mut Vec<BoundEvidenceSource>,
    ) -> Result<ControlLifecycleAdvance, ControlLifecycleError> {
        let participant = self.participant;
        let coordinator = Arc::clone(&self.coordinator);
        let transaction = self.active_transaction_mut("re-arm control IRQ sources")?;
        let ControlStage::ReinitSources {
            rearm,
            acknowledged,
            binding_enabled,
            ..
        } = &mut transaction.stage
        else {
            return Err(ControlLifecycleError::LocalStateMismatch(
                "re-arm control IRQ sources",
            ));
        };
        if !*acknowledged {
            let batch = rearm
                .take()
                .ok_or(ControlLifecycleError::MissingSourceRearmBatch)?;
            match batch.advance(lifecycle_budget()) {
                Ok(SourceRearmBatchProgress::More(batch)) => {
                    *rearm = Some(batch);
                    return Ok(ControlLifecycleAdvance::Pending);
                }
                Ok(SourceRearmBatchProgress::Armed(mut armed)) => {
                    if !sources.is_empty() {
                        return Err(ControlLifecycleError::UnexpectedLiveSources);
                    }
                    sources.append(&mut armed);
                }
                Err(failure) => {
                    let (error, batch) = failure.into_parts();
                    *rearm = Some(batch);
                    return Err(ControlLifecycleError::SourceRearm(error));
                }
            }
            coordinator
                .ack_reinit_sources_armed(participant)
                .map_err(ControlLifecycleError::Coordinator)?;
            *acknowledged = true;
        }
        let snapshot = coordinator.snapshot();
        if acknowledgement_complete(
            snapshot.reinit_sources_armed(),
            snapshot.participant_count(),
        ) {
            if !*binding_enabled {
                published
                    .enable_binding_irq()
                    .map_err(ControlLifecycleError::Binding)?;
                *binding_enabled = true;
            }
            coordinator
                .finish_reinit_sources(participant)
                .map_err(ControlLifecycleError::Coordinator)?;
            let proof = match &mut self
                .active_transaction_mut("enter controller reinit")?
                .stage
            {
                ControlStage::ReinitSources { proof, .. } => proof
                    .take()
                    .ok_or(ControlLifecycleError::MissingReinitProof)?,
                _ => {
                    return Err(ControlLifecycleError::LocalStateMismatch(
                        "enter controller reinit",
                    ));
                }
            };
            self.active_transaction_mut("enter controller reinit")?
                .stage = ControlStage::Reinitializing {
                proof: Some(proof),
                started: false,
                schedule: None,
                result: None,
                pending_commit: None,
                permits: None,
                control_permit: None,
                child_permits_published: 0,
            };
        }
        Ok(ControlLifecycleAdvance::Pending)
    }
}

fn lifecycle_budget() -> NonZeroUsize {
    NonZeroUsize::new(LIFECYCLE_BUDGET).unwrap_or(NonZeroUsize::MIN)
}
