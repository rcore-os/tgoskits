use alloc::sync::Arc;

use rdif_eth::WifiControlProgress;
use ringbuf::traits::Consumer;
use sdmmc_host::ProgressCause;
use sdmmc_protocol::{
    OperationProgress,
    sdio::{
        CardIrqControl, FunctionNumber, HostProgressWait, SdMmcIrqHost, SdioCard, SdioCardInfo,
        io::SdioInitRequest,
    },
};

use super::{ActiveOperation, OperationCompletion, output::OwnerOutputs};
use crate::{
    AicAction, AicDevice, AicError, AicEvent, AicInput, AicInputEvent, AicState, ChipVariant,
    MonotonicTime, SdioCompletion, SdioFailure,
    rdif::{
        device::{IrqLatch, MacAddressState, QueueOwnerPorts, WifiChannels},
        error::AicRdifError,
    },
};

const OWNER_STEP_BUDGET: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OwnerWait {
    Interrupt,
    RetryAt(u64),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OwnerProgress {
    Ready,
    Wait(OwnerWait),
}

/// Sole task-context owner of the SDIO card, controller transactions and AIC core.
pub(crate) struct AicOwner<H: SdMmcIrqHost + 'static> {
    card: SdioCard<H>,
    card_irq: Option<H::CardIrq>,
    init: Option<SdioInitRequest<H>>,
    device: AicDevice,
    active: Option<ActiveOperation<H>>,
    wifi_requests: crate::rdif::device::WifiRequestReceiver,
    outputs: OwnerOutputs,
    irq_latch: Arc<IrqLatch>,
    mac: Arc<MacAddressState>,
    started: bool,
}

impl<H: SdMmcIrqHost + Send + 'static> AicOwner<H> {
    pub(crate) fn new(
        host: H,
        card_irq: Option<H::CardIrq>,
        chip: ChipVariant,
        queues: QueueOwnerPorts,
        wifi: WifiChannels,
        irq_latch: Arc<IrqLatch>,
        mac: Arc<MacAddressState>,
    ) -> Result<
        (
            Self,
            crate::rdif::device::WifiRequestSender,
            crate::rdif::device::WifiProgressReceiver,
        ),
        AicRdifError,
    > {
        let owner = Self {
            card: SdioCard::new(host),
            card_irq,
            init: None,
            device: AicDevice::new(chip)?,
            active: None,
            wifi_requests: wifi.requests_rx,
            outputs: OwnerOutputs::new(queues, wifi.progress_tx),
            irq_latch,
            mac,
            started: false,
        };
        Ok((owner, wifi.requests_tx, wifi.progress_rx))
    }

    pub(crate) fn start(&mut self, now_nanos: u64) -> Result<OwnerProgress, AicRdifError> {
        if self.init.is_some() || self.started {
            return Err(AicError::Busy.into());
        }
        self.card.host_mut().enable_completion_irq()?;
        self.init = Some(self.card.submit_init()?);
        self.advance_with_cause(now_nanos, ProgressCause::Submitted, true)
    }

    pub(crate) fn advance(&mut self, now_nanos: u64) -> Result<OwnerProgress, AicRdifError> {
        self.advance_with_rearm(now_nanos, true)
    }

    fn advance_with_rearm(
        &mut self,
        now_nanos: u64,
        rearm_after_step: bool,
    ) -> Result<OwnerProgress, AicRdifError> {
        if !self.outputs.flush()? {
            return self
                .finish_progress(OwnerProgress::Wait(OwnerWait::Interrupt), rearm_after_step);
        }
        let snapshot = self.irq_latch.take();
        let cause = if snapshot.is_some() {
            ProgressCause::AcknowledgedIrq
        } else {
            ProgressCause::RegisterRetry
        };
        if let Some(snapshot) = snapshot
            && self.started
        {
            let action = self.device.advance(AicInput {
                now: MonotonicTime::from_nanos(now_nanos),
                event: Some(AicInputEvent::Irq(snapshot)),
            });
            if let Some(progress) = self.consume_action(action, now_nanos)? {
                // A controller may latch CARD_INT together with command/data
                // completion. Preserve the card fact in the core, then still
                // advance the active host request with this same acknowledged
                // snapshot so neither half of the combined event is lost.
                if self.active.is_none() {
                    return self.finish_progress(progress, rearm_after_step);
                }
            }
        }
        self.advance_with_cause(now_nanos, cause, rearm_after_step)
    }

    pub(crate) fn quiesce(&mut self) -> Result<(), AicRdifError> {
        if let Some(card_irq) = &mut self.card_irq {
            card_irq.mask();
        }
        self.card.host_mut().disable_completion_irq()?;
        Ok(())
    }

    pub(crate) fn rearm_and_advance(
        &mut self,
        now_nanos: u64,
    ) -> Result<(OwnerProgress, bool), AicRdifError> {
        let progress = self.advance_with_rearm(now_nanos, false)?;
        let card_pending = self
            .card_irq
            .as_mut()
            .is_some_and(CardIrqControl::rearm_and_check);
        self.card.host_mut().enable_completion_irq()?;
        Ok((
            progress,
            card_pending || self.irq_latch.has_pending() || self.outputs.has_runnable_pending(),
        ))
    }

    pub(crate) fn shutdown(&mut self) -> Result<(), AicRdifError> {
        if let Some(active) = &mut self.active {
            active.abort(&mut self.card)?;
        }
        self.active = None;
        if let Some(init) = &mut self.init {
            self.card.abort_init_request(init)?;
        }
        self.init = None;
        if let Some(card_irq) = &mut self.card_irq {
            card_irq.disable();
        }
        self.card.host_mut().disable_completion_irq()?;
        Ok(())
    }

    fn advance_with_cause(
        &mut self,
        now_nanos: u64,
        mut cause: ProgressCause,
        rearm_after_step: bool,
    ) -> Result<OwnerProgress, AicRdifError> {
        for _ in 0..OWNER_STEP_BUDGET {
            if let Some(init) = &mut self.init {
                match self.card.advance_init_request(init, cause)? {
                    OperationProgress::Pending => {
                        return self
                            .finish_progress(self.protocol_wait(now_nanos), rearm_after_step);
                    }
                    OperationProgress::Complete(info) => {
                        self.validate_card_identity(info)?;
                        self.init = None;
                        self.device.start(MonotonicTime::from_nanos(now_nanos))?;
                        self.started = true;
                        cause = ProgressCause::Submitted;
                        continue;
                    }
                }
            }

            if let Some(active) = &mut self.active {
                match active.advance(&mut self.card, cause)? {
                    OperationProgress::Pending => {
                        return self
                            .finish_progress(self.protocol_wait(now_nanos), rearm_after_step);
                    }
                    OperationProgress::Complete(completion) => {
                        self.active = None;
                        let action = self.complete_operation(completion, now_nanos);
                        if let Some(progress) = self.consume_action(action, now_nanos)? {
                            return self.finish_progress(progress, rearm_after_step);
                        }
                        cause = ProgressCause::Submitted;
                        continue;
                    }
                }
            }

            if let Some(control) = self.wifi_requests.try_pop() {
                self.outputs.begin_control(&control);
                let action = self.device.advance(AicInput {
                    now: MonotonicTime::from_nanos(now_nanos),
                    event: Some(AicInputEvent::Control(control)),
                });
                if let Some(progress) = self.consume_action(action, now_nanos)? {
                    return self.finish_progress(progress, rearm_after_step);
                }
                continue;
            }
            if let Some(action) = self.submit_one_tx(now_nanos)?
                && let Some(progress) = self.consume_action(action, now_nanos)?
            {
                return self.finish_progress(progress, rearm_after_step);
            }
            let action = self
                .device
                .advance(AicInput::tick(MonotonicTime::from_nanos(now_nanos)));
            if let Some(progress) = self.consume_action(action, now_nanos)? {
                return self.finish_progress(progress, rearm_after_step);
            }
        }
        self.finish_progress(OwnerProgress::Wait(OwnerWait::Interrupt), rearm_after_step)
    }

    fn complete_operation(&mut self, completion: OperationCompletion, now_nanos: u64) -> AicAction {
        self.device.advance(AicInput {
            now: MonotonicTime::from_nanos(now_nanos),
            event: Some(AicInputEvent::Sdio(SdioCompletion {
                request_id: completion.request_id,
                result: Ok(completion.response),
            })),
        })
    }

    fn consume_action(
        &mut self,
        mut action: AicAction,
        now_nanos: u64,
    ) -> Result<Option<OwnerProgress>, AicRdifError> {
        loop {
            match action {
                AicAction::SubmitSdio(request) => {
                    let mut active = ActiveOperation::submit(&mut self.card, request)?;
                    match active.advance(&mut self.card, ProgressCause::Submitted)? {
                        OperationProgress::Pending => {
                            self.active = Some(active);
                            return Ok(Some(self.protocol_wait(now_nanos)));
                        }
                        OperationProgress::Complete(completion) => {
                            action = self.complete_operation(completion, now_nanos);
                        }
                    }
                }
                AicAction::AbortSdio { request_id } => {
                    let mut active = self.active.take().ok_or(AicError::CompletionMismatch)?;
                    active.abort(&mut self.card)?;
                    action = self.device.advance(AicInput {
                        now: MonotonicTime::from_nanos(now_nanos),
                        event: Some(AicInputEvent::Sdio(SdioCompletion {
                            request_id,
                            result: Err(SdioFailure::Aborted),
                        })),
                    });
                }
                AicAction::RetryAt(deadline) => {
                    self.outputs
                        .publish_wait_progress(WifiControlProgress::RetryAt {
                            deadline_nanos: deadline.as_nanos(),
                        });
                    return Ok(Some(OwnerProgress::Wait(OwnerWait::RetryAt(
                        deadline.as_nanos(),
                    ))));
                }
                AicAction::WaitForInterrupt => {
                    self.outputs
                        .publish_wait_progress(WifiControlProgress::WaitForInterrupt);
                    return Ok(Some(OwnerProgress::Wait(OwnerWait::Interrupt)));
                }
                AicAction::Event(AicEvent::Started { mac_address }) => {
                    self.mac.publish(mac_address);
                    return Ok(Some(OwnerProgress::Ready));
                }
                AicAction::Event(event) => {
                    if self.outputs.consume_event(event)? {
                        return Ok(Some(OwnerProgress::Wait(OwnerWait::Interrupt)));
                    }
                    return Ok(None);
                }
                AicAction::Idle => {
                    return Ok(Some(if self.device.state() == AicState::Ready {
                        OwnerProgress::Ready
                    } else {
                        OwnerProgress::Wait(OwnerWait::Interrupt)
                    }));
                }
            }
        }
    }

    fn validate_card_identity(&self, info: SdioCardInfo) -> Result<(), AicRdifError> {
        let common = info.common_cis;
        let function = FunctionNumber::new(1)
            .ok()
            .and_then(|number| self.card.function(number))
            .map(|function| function.cis);
        let manufacturer_id = function
            .and_then(|cis| cis.manufacturer_id)
            .or(common.manufacturer_id);
        let product_id = function
            .and_then(|cis| cis.product_id)
            .or(common.product_id);
        let detected = manufacturer_id
            .zip(product_id)
            .map(|(vid, did)| ChipVariant::from_vid_did(vid, did))
            .unwrap_or(ChipVariant::Unknown);
        if detected != self.device.chip() {
            return Err(AicError::UnsupportedChip.into());
        }
        Ok(())
    }

    fn submit_one_tx(&mut self, now_nanos: u64) -> Result<Option<AicAction>, AicRdifError> {
        if self.device.state() != AicState::Ready {
            return Ok(None);
        }
        let Some((token, frame)) = self.outputs.take_tx_frame() else {
            return Ok(None);
        };
        Ok(Some(self.device.advance(AicInput {
            now: MonotonicTime::from_nanos(now_nanos),
            event: Some(AicInputEvent::Tx { token, frame }),
        })))
    }

    fn protocol_wait(&self, now_nanos: u64) -> OwnerProgress {
        match self.card.progress_wait() {
            HostProgressWait::Irq => OwnerProgress::Wait(OwnerWait::Interrupt),
            HostProgressWait::Register { retry_after } => {
                let nanos = u64::try_from(retry_after.as_nanos()).unwrap_or(u64::MAX);
                OwnerProgress::Wait(OwnerWait::RetryAt(now_nanos.saturating_add(nanos)))
            }
        }
    }

    fn finish_progress(
        &mut self,
        progress: OwnerProgress,
        rearm_after_step: bool,
    ) -> Result<OwnerProgress, AicRdifError> {
        if !rearm_after_step {
            return Ok(progress);
        }
        if let Some(card_irq) = &mut self.card_irq {
            let _ = card_irq.rearm_and_check();
        }
        self.card.host_mut().enable_completion_irq()?;
        Ok(progress)
    }
}
