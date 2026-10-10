//! Confirmations name the exact activation and command they complete.

use crate::{OperationId, identity::VcpuInstance};

#[derive(Default)]
pub(super) struct ConfirmationReceipt {
    requested: Option<(VcpuInstance, OperationId)>,
    confirmed: bool,
}

impl ConfirmationReceipt {
    pub(super) fn request(&mut self, instance: VcpuInstance, operation: OperationId) {
        assert_eq!(instance.run.vm(), operation.vm(), "owner command identity");
        self.requested = Some((instance, operation));
        self.confirmed = false;
    }

    pub(super) fn accept(&mut self, instance: VcpuInstance, operation: OperationId) -> bool {
        if self.requested != Some((instance, operation)) || self.confirmed {
            return false;
        }
        self.confirmed = true;
        true
    }

    pub(super) fn completed(&self, instance: VcpuInstance, operation: OperationId) -> bool {
        self.confirmed && self.requested == Some((instance, operation))
    }
}

#[cfg(test)]
mod tests {
    use std::{
        future::Future,
        pin::Pin,
        task::{Context, Poll, Waker},
    };

    use super::*;
    use crate::{RunId, VmKey, VmOperation};

    #[test]
    fn superseded_and_foreign_confirmations_do_not_finish_the_operation() {
        let key = VmKey::new(1, 1);
        let instance = VcpuInstance {
            run: RunId::new(key, 1),
            vcpu_id: 0,
            activation: 2,
        };
        let old = OperationId::new(key, 1);
        let current = OperationId::new(key, 2);
        let (mut observation, completion) = VmOperation::new(current);
        completion.accept();
        let mut receipt = ConfirmationReceipt::default();
        receipt.request(instance, old);
        receipt.request(instance, current);
        let mut context = Context::from_waker(Waker::noop());
        for (source, operation) in [
            (instance, old),
            (
                VcpuInstance {
                    activation: 1,
                    ..instance
                },
                current,
            ),
            (
                VcpuInstance {
                    run: RunId::new(key, 2),
                    ..instance
                },
                current,
            ),
        ] {
            assert!(!receipt.accept(source, operation));
            assert!(Pin::new(&mut observation).poll(&mut context).is_pending());
        }
        assert!(receipt.accept(instance, current));
        assert!(!receipt.accept(instance, old));
        assert!(receipt.completed(instance, current));
        completion.finish(Ok(()));
        assert_eq!(
            Pin::new(&mut observation).poll(&mut context),
            Poll::Ready(Ok(()))
        );
        assert!(!receipt.accept(instance, current));
    }
}
