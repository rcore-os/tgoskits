use axdevice::{EndpointIrqTransitionPermit, PciEndpointContext};
use axdevice_base::{AccessWidth, DeviceError, DeviceResult};
use axvirtio_common::{
    DeviceContextMemory,
    pci::{
        InterruptPublicationRequest, InterruptTransition, InterruptTransitionRequest,
        VirtioDeviceCore, VirtioPciWriteOutcome,
    },
};

use super::VirtioPciFunction;

enum TransitionResult {
    Published,
    Suppressed,
    Failed(DeviceError),
}

impl<D: VirtioDeviceCore> VirtioPciFunction<D> {
    fn execute_permitted_transition(
        &self,
        permit: &mut EndpointIrqTransitionPermit,
        transition: InterruptTransition,
    ) -> DeviceResult {
        match transition {
            InterruptTransition::Assert => permit.assert(&self.irq_line),
            InterruptTransition::Deassert => permit.deassert(&self.irq_line),
            InterruptTransition::None => Ok(()),
        }
    }

    fn execute_transition(
        &self,
        context: &mut dyn PciEndpointContext,
        transition: InterruptTransition,
    ) -> TransitionResult {
        if transition == InterruptTransition::None {
            return TransitionResult::Published;
        }
        let result = context.with_irq_transition(&mut |permit| {
            self.execute_permitted_transition(permit, transition)
        });
        match result {
            Ok(()) => TransitionResult::Published,
            Err(DeviceError::InvalidState { .. }) => TransitionResult::Suppressed,
            Err(error) => TransitionResult::Failed(error),
        }
    }

    pub(super) fn finish_transition(
        &self,
        context: &mut dyn PciEndpointContext,
        transition: InterruptTransition,
    ) -> DeviceResult {
        let mut transition = transition;
        loop {
            match self.execute_transition(context, transition) {
                TransitionResult::Published => {
                    transition = self
                        .transport
                        .complete_interrupt_transition(transition, true);
                }
                TransitionResult::Suppressed => {
                    self.transport
                        .suppress_stale_interrupt_transition(transition);
                    transition = InterruptTransition::None;
                }
                TransitionResult::Failed(error) => {
                    self.transport
                        .complete_interrupt_transition(transition, false);
                    return Err(error);
                }
            }
            if transition == InterruptTransition::None {
                return Ok(());
            }
        }
    }

    pub(super) fn finish_transition_request(
        &self,
        context: &mut dyn PciEndpointContext,
        request: InterruptTransitionRequest,
    ) -> DeviceResult {
        let transition = request.transition();
        let result = self.finish_transition(context, transition);
        drop(request);
        result
    }

    pub(super) fn finish_read_transition(
        &self,
        context: &mut dyn PciEndpointContext,
        request: InterruptTransitionRequest,
    ) {
        // ISR is read-to-clear: the value already captured for the guest is
        // not revoked when the physical line backend fails. `finish_transition`
        // records that failure in the coordinator, leaving the next admitted
        // transition responsible for retrying the deassertion.
        let _ = self.finish_transition_request(context, request);
    }

    fn publish_interrupt_request(
        &self,
        request: InterruptPublicationRequest,
        context: &mut dyn PciEndpointContext,
    ) -> DeviceResult {
        if !request.requires_irq_permit() {
            request.cancel();
            return Ok(());
        }

        let mut request = Some(request);
        let mut callback_entered = false;
        let result = context.with_irq_transition(&mut |permit| {
            callback_entered = true;
            let Some(request) = request.take() else {
                return Err(DeviceError::InvalidState {
                    operation: "publish VirtIO PCI interrupt",
                    detail: "IRQ transition callback ran more than once".into(),
                });
            };
            request.publish(|transition| self.execute_permitted_transition(permit, transition))
        });
        if !callback_entered && matches!(result, Err(DeviceError::InvalidState { .. })) {
            // Binding teardown won the IRQ admission race. The request has
            // not recorded an ISR bit yet, so dropping it is a clean cancel.
            drop(request);
            return Ok(());
        }
        result
    }

    fn publish_queue_notification(
        &self,
        notification: axvirtio_common::pci::QueueNotification,
        context: &mut dyn PciEndpointContext,
    ) -> DeviceResult {
        self.publish_interrupt_request(notification.into_interrupt_publication(), context)
    }

    pub(super) fn write_transport(
        &self,
        offset: u64,
        width: AccessWidth,
        value: u64,
        command: axdevice::PciCommandState,
        context: &mut dyn PciEndpointContext,
    ) -> DeviceResult {
        let outcome = {
            let mut memory = DeviceContextMemory::new(context, &self.dma_grant);
            self.transport.write_bar_with_dma(
                offset,
                width,
                value,
                command.bus_master_enable(),
                &mut memory,
            )?
        };
        match outcome {
            VirtioPciWriteOutcome::None => Ok(()),
            VirtioPciWriteOutcome::Reset { interrupt } => {
                if let Err(error) = self.finish_transition(context, interrupt) {
                    self.transport.abort_reset();
                    return Err(error);
                }
                let transition = self
                    .apply_command_revision(command, true)
                    .filter(|intent| self.transport.queue_generation() == intent.generation())
                    .map(|intent| intent.transition())
                    .unwrap_or(InterruptTransition::None);
                if let Err(error) = self.finish_transition(context, transition) {
                    self.transport.abort_reset();
                    return Err(error);
                }
                self.transport.complete_reset();
                Ok(())
            }
            VirtioPciWriteOutcome::Fault { error, publication } => {
                self.publish_interrupt_request(publication, context)?;
                Err(error)
            }
            VirtioPciWriteOutcome::QueueNotified(notification) => {
                self.publish_queue_notification(notification, context)
            }
        }
    }
}
