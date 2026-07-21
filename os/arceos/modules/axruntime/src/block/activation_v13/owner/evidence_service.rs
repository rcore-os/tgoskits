//! Ready-state evidence routing between control and shared I/O owners.

use super::*;
use crate::block::activation_v13::request_runtime::RuntimeIoDomainPort;

pub(super) fn service_ready_evidence(
    published: &mut RdifBlockPublishedOwner,
    control_io: &mut ControlIoRuntime,
    pending: rdif_block::PendingBlockIrq,
) -> Result<(IrqServiceDecision, usize, ReadyEvidenceRoute), ReadyServiceFailure> {
    let source = pending.evidence_id().source();
    match control_io {
        ControlIoRuntime::Split(shared) if shared.requests.handles_source(source) => {
            return service_io_evidence(
                shared.domain.io_mut(),
                &mut shared.requests,
                pending,
                ReadyEvidenceRoute::SplitIo,
            );
        }
        ControlIoRuntime::Combined(requests) if requests.handles_source(source) => {
            let Some(mut domain) = published.published_mut().shared_io_domain_mut() else {
                return Err(ReadyServiceFailure {
                    phase: "borrow combined shared I/O owner for evidence",
                    retained: ReadyServiceRetained::CombinedUnavailable { _pending: pending },
                });
            };
            return service_io_evidence(
                &mut domain,
                requests,
                pending,
                ReadyEvidenceRoute::CombinedIo,
            );
        }
        ControlIoRuntime::None | ControlIoRuntime::Split(_) | ControlIoRuntime::Combined(_) => {}
    }
    published
        .published_mut()
        .control_mut()
        .service_evidence(pending)
        .map(|decision| (decision, 0, ReadyEvidenceRoute::Control))
        .map_err(|failure| ReadyServiceFailure {
            phase: "service controller evidence",
            retained: ReadyServiceRetained::Control {
                _failure: Box::new(failure),
            },
        })
}

fn service_io_evidence<D>(
    domain: &mut D,
    requests: &mut DomainRequestOwner,
    pending: rdif_block::PendingBlockIrq,
    route: ReadyEvidenceRoute,
) -> Result<(IrqServiceDecision, usize, ReadyEvidenceRoute), ReadyServiceFailure>
where
    D: RuntimeIoDomainPort + ?Sized,
{
    let result = domain.service_evidence(pending.evidence_id(), requests.completion_sink());
    let completed = match requests.finish_completions() {
        Ok(completed) => completed,
        Err(error) => {
            return Err(ReadyServiceFailure {
                phase: "publish shared-domain terminal completions",
                retained: ReadyServiceRetained::Requests {
                    _pending: pending,
                    _error: error,
                },
            });
        }
    };
    match result {
        Ok(EvidenceServiceResult::Drained) => Ok((pending.drain(), completed, route)),
        Ok(EvidenceServiceResult::Retained) => Ok((pending.retain(), completed, route)),
        Ok(EvidenceServiceResult::Recover(fault)) => {
            Ok((pending.recover(fault), completed, route))
        }
        Err(error) => Err(ReadyServiceFailure {
            phase: "service shared I/O evidence",
            retained: ReadyServiceRetained::Io {
                _pending: pending,
                _error: error,
            },
        }),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ReadyEvidenceRoute {
    SplitIo,
    CombinedIo,
    Control,
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU16;

    use rdif_block::{
        IdList, InterruptQueueDesc, LogicalDeviceSelector, OwnershipDomainId, QueueExecution,
    };

    use super::*;

    #[test]
    fn queue_evidence_mapping_excludes_control_only_sources() {
        let domain = OwnershipDomainId::new(3).unwrap();
        let mut queue_sources = IdList::none();
        queue_sources.insert(11);
        let queue = InterruptQueueDesc::new(
            0,
            LogicalDeviceSelector::AllPublished,
            domain,
            QueueExecution::Tagged,
            NonZeroU16::new(4).unwrap(),
            queue_sources,
        )
        .unwrap();
        let runtime = Arc::new(
            DomainRequestRuntime::new(
                domain,
                &[queue],
                crate::block::BlockRuntimeConfig::default(),
            )
            .unwrap(),
        );
        let owner = DomainRequestOwner::new(runtime);

        assert!(owner.handles_source(rdif_block::IrqSourceId::new(11).unwrap()));
        assert!(!owner.handles_source(rdif_block::IrqSourceId::new(12).unwrap()));
    }
}
