//! Controller reconstruction, permit routing, and owner resume phases.

use super::*;

impl ControlLifecycle {
    pub(super) fn advance_reinitialize(
        &mut self,
        published: &mut RdifBlockPublishedOwner,
        control_io: &ControlIoRuntime,
    ) -> Result<ControlLifecycleAdvance, ControlLifecycleError> {
        for _ in 0..LIFECYCLE_BUDGET {
            if self.reinit_result_ready()? {
                self.bind_and_route_reinit_result(published, control_io)?;
                return Ok(ControlLifecycleAdvance::Pending);
            }
            let now_ns = ax_hal::time::monotonic_time_nanos();
            let trigger = {
                let transaction = self.active_transaction_mut("drive controller reinit")?;
                let ControlStage::Reinitializing {
                    proof,
                    started,
                    schedule,
                    ..
                } = &mut transaction.stage
                else {
                    return Err(ControlLifecycleError::LocalStateMismatch(
                        "drive controller reinit",
                    ));
                };
                if !*started {
                    *started = true;
                    ControlTrigger::BeginReinitialize {
                        now_ns,
                        quiesced: proof
                            .take()
                            .ok_or(ControlLifecycleError::MissingReinitProof)?,
                    }
                } else {
                    let Some(schedule) = *schedule else {
                        return Ok(ControlLifecycleAdvance::Pending);
                    };
                    validate_control_schedule(schedule, &self.control_source_ids)?;
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
            self.record_reinit_progress(progress)?;
        }
        Ok(ControlLifecycleAdvance::Pending)
    }

    pub(in crate::block::activation_v13::owner) fn service_reinitialize_irq(
        &mut self,
        published: &mut RdifBlockPublishedOwner,
        source: &mut BoundEvidenceSource,
        pending: PendingBlockIrq,
    ) -> Result<(), ControlLifecycleError> {
        if self.coordinator.snapshot().phase() != ShutdownPhase::ControllerReinitializing {
            return Err(ControlLifecycleError::WrongIrqPhase);
        }
        let poll = published
            .published_mut()
            .control_mut()
            .service_control(ControlTrigger::Irq {
                now_ns: ax_hal::time::monotonic_time_nanos(),
                evidence: pending,
            })
            .map_err(|failure| ControlLifecycleError::ControlService(Box::new(failure)))?;
        let (progress, decision) = poll.into_parts();
        let decision = decision.ok_or(ControlLifecycleError::MissingIrqDisposition)?;
        let controller_identity = published.published().control().controller_identity();
        let completed = apply_domain_decision(
            source,
            decision,
            controller_identity,
            crate::block::activation_v13::source::recovery::DriverEvidenceRoute::Control,
            |evidence| {
                published
                    .published_mut()
                    .control_mut()
                    .commit_drained_evidence(evidence)
            },
        )
        .map_err(|failure| ControlLifecycleError::EvidenceDisposition(Box::new(failure)))?;
        if matches!(completed, DomainDecisionApplied::RecoveryRequired(_)) {
            let DomainDecisionApplied::RecoveryRequired(fault) = completed else {
                unreachable!()
            };
            return Err(ControlLifecycleError::RecoveryDuringReinitialize(fault));
        }
        if matches!(progress, ControlProgress::Reinitialized(_))
            && completed != DomainDecisionApplied::EvidenceDrained
        {
            return Err(ControlLifecycleError::RetainedEvidenceAcrossEpoch);
        }
        self.record_reinit_progress(progress)
    }

    fn record_reinit_progress(
        &mut self,
        progress: ControlProgress,
    ) -> Result<(), ControlLifecycleError> {
        match progress {
            ControlProgress::Pending(schedule) => {
                validate_control_schedule(schedule, &self.control_source_ids)?;
                let transaction = self.active_transaction_mut("record reinit schedule")?;
                let ControlStage::Reinitializing {
                    schedule: current, ..
                } = &mut transaction.stage
                else {
                    return Err(ControlLifecycleError::LocalStateMismatch(
                        "record reinit schedule",
                    ));
                };
                *current = Some(schedule);
            }
            ControlProgress::Reinitialized(result) => {
                let transaction = self.active_transaction_mut("record reinit result")?;
                let ControlStage::Reinitializing {
                    result: current,
                    schedule,
                    ..
                } = &mut transaction.stage
                else {
                    return Err(ControlLifecycleError::LocalStateMismatch(
                        "record reinit result",
                    ));
                };
                if current.is_some() {
                    return Err(ControlLifecycleError::DuplicateReinitResult(Box::new(
                        result,
                    )));
                }
                *schedule = None;
                *current = Some(result);
            }
            ControlProgress::Failed(error) => {
                return Err(ControlLifecycleError::DriverControl(error));
            }
            progress @ (ControlProgress::PublicationReady(_) | ControlProgress::DmaQuiesced(_)) => {
                return Err(ControlLifecycleError::UnexpectedProgress(Box::new(
                    progress,
                )));
            }
        }
        Ok(())
    }

    fn reinit_result_ready(&mut self) -> Result<bool, ControlLifecycleError> {
        let transaction = self.active_transaction_mut("inspect reinit result")?;
        let ControlStage::Reinitializing {
            result,
            pending_commit,
            permits,
            ..
        } = &transaction.stage
        else {
            return Err(ControlLifecycleError::LocalStateMismatch(
                "inspect reinit result",
            ));
        };
        Ok(result.is_some() || pending_commit.is_some() || permits.is_some())
    }

    fn bind_and_route_reinit_result(
        &mut self,
        published: &mut RdifBlockPublishedOwner,
        control_io: &ControlIoRuntime,
    ) -> Result<(), ControlLifecycleError> {
        self.bind_reinit_result(published)?;
        let control_domain = control_io.domain_id(published);
        let child_count = self.child_reinit_cells.len();
        let coordinator = Arc::clone(&self.coordinator);
        let participant = self.participant;
        let mut serviced = 0;
        while serviced < LIFECYCLE_BUDGET {
            let permit = {
                let transaction = self.active_transaction_mut("route reinit permits")?;
                let ControlStage::Reinitializing { permits, .. } = &mut transaction.stage else {
                    return Err(ControlLifecycleError::LocalStateMismatch(
                        "route reinit permits",
                    ));
                };
                permits.as_mut().and_then(Vec::pop)
            };
            let Some(permit) = permit else {
                break;
            };
            if Some(permit.domain()) == control_domain {
                let transaction = self.active_transaction_mut("retain control reinit permit")?;
                let ControlStage::Reinitializing { control_permit, .. } = &mut transaction.stage
                else {
                    return Err(ControlLifecycleError::LocalStateMismatch(
                        "retain control reinit permit",
                    ));
                };
                if control_permit.is_some() {
                    return Err(ControlLifecycleError::DuplicateControlPermit { _permit: permit });
                }
                *control_permit = Some(permit);
            } else {
                let Some(cell) = self
                    .child_reinit_cells
                    .iter()
                    .find(|cell| cell.domain() == permit.domain())
                else {
                    return Err(ControlLifecycleError::UnexpectedDomainPermit { _permit: permit });
                };
                cell.publish_permit(permit)
                    .map_err(ControlLifecycleError::PermitPublish)?;
                let transaction = self.active_transaction_mut("count child reinit permit")?;
                let ControlStage::Reinitializing {
                    child_permits_published,
                    ..
                } = &mut transaction.stage
                else {
                    return Err(ControlLifecycleError::LocalStateMismatch(
                        "count child reinit permit",
                    ));
                };
                *child_permits_published += 1;
            }
            serviced += 1;
        }
        let transaction = self.active_transaction_mut("finish permit routing")?;
        let ControlStage::Reinitializing {
            pending_commit,
            permits,
            control_permit,
            child_permits_published,
            ..
        } = &mut transaction.stage
        else {
            return Err(ControlLifecycleError::LocalStateMismatch(
                "finish permit routing",
            ));
        };
        if permits.as_ref().is_some_and(|permits| !permits.is_empty()) {
            return Ok(());
        }
        if control_domain.is_some() && control_permit.is_none() {
            return Err(ControlLifecycleError::MissingControlPermit);
        }
        if *child_permits_published != child_count {
            return Err(ControlLifecycleError::MissingChildPermit {
                expected: child_count,
                actual: *child_permits_published,
            });
        }
        coordinator
            .begin_owner_resume(participant)
            .map_err(ControlLifecycleError::Coordinator)?;
        let pending_commit = pending_commit
            .take()
            .ok_or(ControlLifecycleError::MissingEpochCommit)?;
        let control_permit = control_permit.take();
        self.active_transaction_mut("enter owner resume")?.stage = ControlStage::Resuming {
            pending_commit: Some(pending_commit),
            control_permit,
            acknowledged: false,
        };
        self.wake_children()
    }

    fn bind_reinit_result(
        &mut self,
        published: &RdifBlockPublishedOwner,
    ) -> Result<(), ControlLifecycleError> {
        let result = {
            let transaction = self.active_transaction_mut("bind reinit result")?;
            let ControlStage::Reinitializing {
                result,
                pending_commit,
                permits,
                ..
            } = &mut transaction.stage
            else {
                return Err(ControlLifecycleError::LocalStateMismatch(
                    "bind reinit result",
                ));
            };
            if pending_commit.is_some() || permits.is_some() {
                return Ok(());
            }
            result
                .take()
                .ok_or(ControlLifecycleError::MissingReinitResult)?
        };
        let bound = published
            .published()
            .control()
            .bind_reinitialized(result)
            .map_err(|failure| ControlLifecycleError::ReinitBinding(Box::new(failure)))?;
        let (pending_commit, permits) = bound.into_resume_parts();
        let next = pending_commit
            .epoch()
            .get()
            .checked_add(1)
            .ok_or(ControlLifecycleError::EpochExhausted)?;
        self.next_quiesce_epoch = ControllerEpoch::new(next);
        let transaction = self.active_transaction_mut("store bound reinit permits")?;
        let ControlStage::Reinitializing {
            pending_commit: current_commit,
            permits: current,
            ..
        } = &mut transaction.stage
        else {
            return Err(ControlLifecycleError::LocalStateMismatch(
                "store bound reinit permits",
            ));
        };
        *current_commit = Some(pending_commit);
        *current = Some(permits);
        Ok(())
    }

    pub(super) fn advance_owner_resume(
        &mut self,
        published: &mut RdifBlockPublishedOwner,
        control_io: &mut ControlIoRuntime,
    ) -> Result<ControlLifecycleAdvance, ControlLifecycleError> {
        self.collect_child_resume_proofs()?;
        self.resume_control_owner(published, control_io)?;
        self.commit_resumed_epoch(published, control_io)
    }

    fn collect_child_resume_proofs(&mut self) -> Result<(), ControlLifecycleError> {
        let resumed = self
            .child_reinit_cells
            .iter()
            .filter_map(|cell| cell.take_resumed())
            .collect::<Vec<_>>();
        if resumed.is_empty() {
            return Ok(());
        }
        let transaction = self.active_transaction_mut("collect resumed-domain proofs")?;
        let ControlStage::Resuming { pending_commit, .. } = &mut transaction.stage else {
            return Err(ControlLifecycleError::LocalStateMismatch(
                "collect resumed-domain proofs",
            ));
        };
        let pending = pending_commit
            .as_mut()
            .ok_or(ControlLifecycleError::MissingEpochCommit)?;
        for proof in resumed {
            pending
                .accept_resumed(proof)
                .map_err(|failure| ControlLifecycleError::ResumeProofJoin(Box::new(failure)))?;
        }
        Ok(())
    }

    fn resume_control_owner(
        &mut self,
        published: &mut RdifBlockPublishedOwner,
        control_io: &mut ControlIoRuntime,
    ) -> Result<(), ControlLifecycleError> {
        let permit = {
            let transaction = self.active_transaction_mut("resume control owner")?;
            let ControlStage::Resuming {
                control_permit,
                acknowledged,
                ..
            } = &mut transaction.stage
            else {
                return Err(ControlLifecycleError::LocalStateMismatch(
                    "resume control owner",
                ));
            };
            if *acknowledged {
                return Ok(());
            }
            control_permit.take()
        };
        let resumed = control_io
            .resume_after_reinitialize(published, permit)
            .map_err(ControlLifecycleError::ControlIoResume)?;
        if let Some(resumed) = resumed {
            let transaction = self.active_transaction_mut("join control-domain resume proof")?;
            let ControlStage::Resuming { pending_commit, .. } = &mut transaction.stage else {
                return Err(ControlLifecycleError::LocalStateMismatch(
                    "join control-domain resume proof",
                ));
            };
            pending_commit
                .as_mut()
                .ok_or(ControlLifecycleError::MissingEpochCommit)?
                .accept_resumed(resumed)
                .map_err(|failure| ControlLifecycleError::ResumeProofJoin(Box::new(failure)))?;
        }
        self.coordinator
            .ack_resumed(self.participant)
            .map_err(ControlLifecycleError::Coordinator)?;
        let transaction = self.active_transaction_mut("record resumed control owner")?;
        let ControlStage::Resuming { acknowledged, .. } = &mut transaction.stage else {
            return Err(ControlLifecycleError::LocalStateMismatch(
                "record resumed control owner",
            ));
        };
        *acknowledged = true;
        Ok(())
    }

    fn commit_resumed_epoch(
        &mut self,
        published: &mut RdifBlockPublishedOwner,
        control_io: &ControlIoRuntime,
    ) -> Result<ControlLifecycleAdvance, ControlLifecycleError> {
        let coordinator = Arc::clone(&self.coordinator);
        let snapshot = coordinator.snapshot();
        if !acknowledgement_complete(snapshot.resumed(), snapshot.participant_count()) {
            return Ok(ControlLifecycleAdvance::Pending);
        }
        self.collect_child_resume_proofs()?;
        let pending = {
            let transaction = self.active_transaction_mut("finish controller epoch commit")?;
            let ControlStage::Resuming { pending_commit, .. } = &mut transaction.stage else {
                return Err(ControlLifecycleError::LocalStateMismatch(
                    "finish controller epoch commit",
                ));
            };
            pending_commit
                .take()
                .ok_or(ControlLifecycleError::MissingEpochCommit)?
        };
        let next_quiesce_epoch = ControllerEpoch::new(
            pending
                .epoch()
                .get()
                .checked_add(1)
                .ok_or(ControlLifecycleError::EpochExhausted)?,
        );
        let commit = pending
            .finish()
            .map_err(|failure| ControlLifecycleError::IncompleteEpochCommit(Box::new(failure)))?;
        published
            .published_mut()
            .commit_reinitialized_epoch(commit)
            .map_err(|failure| ControlLifecycleError::EpochCommit(Box::new(failure)))?;
        self.next_quiesce_epoch = next_quiesce_epoch;
        coordinator
            .finish_recovered(self.participant)
            .map_err(ControlLifecycleError::Coordinator)?;
        self.participant = coordinator
            .participant(0)
            .map_err(ControlLifecycleError::Coordinator)?;
        self.state = ControlOwnerState::Running;
        if self.terminal_close_requested {
            self.begin_transaction(control_io, QuiesceIntent::Shutdown)?;
        }
        Ok(ControlLifecycleAdvance::Pending)
    }
}

fn validate_control_schedule(
    schedule: ControlSchedule,
    sources: &[rdif_block::IrqSourceId],
) -> Result<(), ControlLifecycleError> {
    let requested = schedule.irq_sources();
    for source in requested.iter() {
        if !sources.iter().any(|candidate| candidate.get() == source) {
            return Err(ControlLifecycleError::ForeignControlScheduleSource);
        }
    }
    Ok(())
}
