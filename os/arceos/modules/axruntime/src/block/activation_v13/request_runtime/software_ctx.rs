//! Frozen ctx-to-hctx ingress and publication ownership.

use alloc::{boxed::Box, collections::VecDeque, sync::Arc};

use ax_kspin::SpinNoPreempt;

use super::{mq::V13SubmitErrorKind, table::RequestToken};
use crate::maintenance::MaintenanceSubmitError;

pub(super) struct FrozenSoftwareCtxMap {
    by_hctx: Box<[Box<[Arc<SoftwareCtxIngress>]>]>,
}

impl FrozenSoftwareCtxMap {
    pub(super) fn new(by_hctx: Box<[Box<[Arc<SoftwareCtxIngress>]>]>) -> Self {
        Self { by_hctx }
    }

    pub(super) fn hctx_count(&self) -> usize {
        self.by_hctx.len()
    }

    pub(super) fn pop(&self, hctx_index: usize, cursor: &mut usize) -> Option<RequestToken> {
        let contexts = self.by_hctx.get(hctx_index)?;
        claim_round_robin(contexts.len(), cursor, |index| contexts[index].pop())
    }

    pub(super) fn has_pending(&self) -> bool {
        self.by_hctx
            .iter()
            .flatten()
            .any(|context| context.has_pending())
    }
}

/// One CPU's bounded request ingress before its hardware owner claims a token.
pub(super) struct SoftwareCtxIngress {
    cpu: usize,
    pending: SpinNoPreempt<VecDeque<RequestToken>>,
}

impl SoftwareCtxIngress {
    pub(super) fn new(cpu: usize, capacity: usize) -> Self {
        Self {
            cpu,
            pending: SpinNoPreempt::new(VecDeque::with_capacity(capacity)),
        }
    }

    pub(super) fn publish(
        &self,
        token: RequestToken,
    ) -> Result<PendingSoftwareCtxPublication<'_>, V13SubmitErrorKind> {
        let mut pending = self.pending.lock();
        if pending.len() == pending.capacity() {
            return Err(V13SubmitErrorKind::SoftwareCtxFull { cpu: self.cpu });
        }
        pending.push_back(token);
        Ok(PendingSoftwareCtxPublication {
            ingress: self,
            token,
        })
    }

    fn pop(&self) -> Option<RequestToken> {
        self.pending.lock().pop_front()
    }

    fn has_pending(&self) -> bool {
        !self.pending.lock().is_empty()
    }

    fn try_retract(&self, token: RequestToken) -> bool {
        let mut pending = self.pending.lock();
        let Some(index) = pending.iter().position(|candidate| *candidate == token) else {
            return false;
        };
        pending.remove(index);
        true
    }
}

/// Linearizes software-context publication against an owner claim.
#[must_use = "resolve the owner wake before returning request ownership"]
pub(super) struct PendingSoftwareCtxPublication<'ingress> {
    ingress: &'ingress SoftwareCtxIngress,
    token: RequestToken,
}

impl PendingSoftwareCtxPublication<'_> {
    pub(super) fn finish_after_wake(
        self,
        wake: Result<(), MaintenanceSubmitError>,
    ) -> Result<RequestToken, RetractedSoftwareCtxPublication> {
        let Err(error) = wake else {
            return Ok(self.token);
        };
        if !self.ingress.try_retract(self.token) {
            // The fixed owner already claimed the token, so publication won
            // the race even though this particular wake could not be delivered.
            return Ok(self.token);
        }
        Err(RetractedSoftwareCtxPublication {
            error,
            token: self.token,
        })
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct RetractedSoftwareCtxPublication {
    error: MaintenanceSubmitError,
    token: RequestToken,
}

impl RetractedSoftwareCtxPublication {
    pub(super) fn into_parts(self) -> (MaintenanceSubmitError, RequestToken) {
        (self.error, self.token)
    }
}

fn claim_round_robin<T>(
    context_count: usize,
    cursor: &mut usize,
    mut claim: impl FnMut(usize) -> Option<T>,
) -> Option<T> {
    if context_count == 0 {
        return None;
    }
    let start = *cursor % context_count;
    for offset in 0..context_count {
        let index = (start + offset) % context_count;
        if let Some(value) = claim(index) {
            *cursor = (index + 1) % context_count;
            return Some(value);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_owner_wake_retracts_publication_once() {
        let ingress = SoftwareCtxIngress::new(3, 1);
        let token = request_token(1);
        let publication = ingress.publish(token).unwrap();

        let failure = publication
            .finish_after_wake(Err(MaintenanceSubmitError::Closed))
            .unwrap_err();
        let (error, retracted) = failure.into_parts();

        assert_eq!(error, MaintenanceSubmitError::Closed);
        assert_eq!(retracted, token);
        assert!(ingress.pop().is_none());
        assert!(!ingress.try_retract(token));
    }

    #[test]
    fn owner_claim_wins_against_a_late_wake_failure() {
        let ingress = SoftwareCtxIngress::new(1, 1);
        let token = request_token(1);
        let publication = ingress.publish(token).unwrap();

        assert_eq!(ingress.pop(), Some(token));
        assert_eq!(
            publication.finish_after_wake(Err(MaintenanceSubmitError::Closed)),
            Ok(token)
        );
    }

    #[test]
    fn one_hctx_rotates_between_cpu_software_contexts() {
        let cpu0 = Arc::new(SoftwareCtxIngress::new(0, 2));
        let cpu1 = Arc::new(SoftwareCtxIngress::new(1, 1));
        let cpu0_first = request_token(1);
        let cpu0_second = request_token(2);
        let cpu1_first = request_token(3);
        publish(&cpu0, cpu0_first);
        publish(&cpu0, cpu0_second);
        publish(&cpu1, cpu1_first);
        let hctx_contexts: Box<[Arc<SoftwareCtxIngress>]> = Box::new([cpu0, cpu1]);
        let map = FrozenSoftwareCtxMap::new(Box::new([hctx_contexts]));
        let mut cursor = 0;

        assert_eq!(map.pop(0, &mut cursor), Some(cpu0_first));
        assert_eq!(map.pop(0, &mut cursor), Some(cpu1_first));
        assert_eq!(map.pop(0, &mut cursor), Some(cpu0_second));
    }

    fn request_token(id: usize) -> RequestToken {
        RequestToken {
            id: rdif_block::RequestId::try_new(id).unwrap(),
            slot: id,
            generation: 1,
        }
    }

    fn publish(ingress: &SoftwareCtxIngress, token: RequestToken) {
        ingress
            .publish(token)
            .unwrap()
            .finish_after_wake(Ok(()))
            .unwrap();
    }
}
