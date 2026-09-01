use alloc::sync::Arc;

use rdif_eth::WifiControlProgress;
use ringbuf::traits::Consumer;
use sdmmc_host::ProgressCause;
use sdmmc_protocol::{
    OperationProgress,
    sdio::{
        CardIrqControl, CisInfo, CompletionIrqRearm, FunctionNumber, HostProgressWait,
        SdMmcIrqHost, SdioCard, SdioCardInfo, io::SdioInitRequest,
    },
};

use super::{ActiveOperation, OperationCompletion, output::OwnerOutputs};
use crate::{
    AicAction, AicDevice, AicError, AicEvent, AicInput, AicInputEvent, AicState, ChipVariant,
    MonotonicTime, SdioCompletion, SdioFailure,
    profile::ChipProfile,
    rdif::{
        device::{IrqLatch, MacAddressState, QueueOwnerPorts, WifiChannels},
        error::{AicRdifError, AicSdioIdentity},
    },
};

const OWNER_STEP_BUDGET: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OwnerWait {
    Interrupt,
    InterruptUntil(u64),
    RetryAt(u64),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OwnerProgress {
    Ready,
    Wait(OwnerWait),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CardIrqWait {
    Masked,
    Armed,
}

/// Sole task-context owner of the SDIO card, controller transactions and AIC core.
pub(crate) struct AicOwner<H: SdMmcIrqHost + 'static> {
    card: SdioCard<H>,
    card_irq: Option<H::CardIrq>,
    init: Option<SdioInitRequest<H>>,
    device: Option<AicDevice>,
    active: Option<ActiveOperation<H>>,
    wifi_requests: crate::rdif::device::WifiRequestReceiver,
    outputs: OwnerOutputs,
    irq_latch: Arc<IrqLatch>,
    mac: Arc<MacAddressState>,
    started: bool,
    card_irq_wait: CardIrqWait,
}

impl<H: SdMmcIrqHost + Send + 'static> AicOwner<H> {
    pub(crate) fn new(
        host: H,
        card_irq: Option<H::CardIrq>,
        queues: QueueOwnerPorts,
        wifi: WifiChannels,
        irq_latch: Arc<IrqLatch>,
        mac: Arc<MacAddressState>,
    ) -> (
        Self,
        crate::rdif::device::WifiRequestSender,
        crate::rdif::device::WifiProgressReceiver,
    ) {
        let mut card_irq = card_irq;
        if let Some(card_irq) = &mut card_irq {
            card_irq.mask();
        }
        let owner = Self {
            card: SdioCard::new(host),
            card_irq,
            init: None,
            device: None,
            active: None,
            wifi_requests: wifi.requests_rx,
            outputs: OwnerOutputs::new(queues, wifi.progress_tx, wifi.progress_signal),
            irq_latch,
            mac,
            started: false,
            card_irq_wait: CardIrqWait::Masked,
        };
        (owner, wifi.requests_tx, wifi.progress_rx)
    }

    pub(crate) fn start(&mut self, now_nanos: u64) -> Result<OwnerProgress, AicRdifError> {
        if self.init.is_some() || self.started {
            return Err(AicError::Busy.into());
        }
        self.card.host_mut().enable_completion_irq()?;
        log::info!("[wifi] SDIO completion IRQ enabled; submitting IO-card initialization");
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
            return self.finish_progress(
                OwnerProgress::Wait(OwnerWait::Interrupt),
                rearm_after_step,
                now_nanos,
            );
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
            let action = self.device_mut()?.advance(AicInput {
                now: MonotonicTime::from_nanos(now_nanos),
                event: Some(AicInputEvent::Irq(snapshot)),
            });
            if let Some(progress) = self.consume_action(action, now_nanos)? {
                // A controller may latch CARD_INT together with command/data
                // completion. Preserve the card fact in the core, then still
                // advance the active host request with this same acknowledged
                // snapshot so neither half of the combined event is lost.
                if self.active.is_none() {
                    return self.finish_progress(progress, rearm_after_step, now_nanos);
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
        // Restore completion delivery first.  If CARD_INT and a command/data
        // completion become visible in the same rearm window, the SDHCI
        // completion path masks the level source while caching the normal
        // completion.  The card endpoint must then sample and republish the
        // still-latched CARD_INT instead of losing that edge in the window.
        let completion_pending = self.rearm_completion_irq()?;
        let card_pending = card_irq_rearm_allowed(
            self.card_irq_needed(),
            self.active.is_some(),
            self.card_irq_wait,
        ) && self
            .card_irq
            .as_mut()
            .is_some_and(CardIrqControl::rearm_and_check);
        if card_pending {
            self.irq_latch.publish_card_pending();
        }
        let queue_progress = self.outputs.take_queue_progress();
        Ok((
            progress,
            card_pending
                || completion_pending
                || self.irq_latch.has_pending()
                || queue_progress
                || self.outputs.has_runnable_pending(),
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

    pub(crate) fn startup_diagnostic(&mut self) -> (bool, u64, bool, Option<bool>) {
        let (irq_sequence, irq_pending) = self.irq_latch.diagnostic();
        let completion_pending = self
            .card
            .host_mut()
            .rearm_completion_irq_and_check()
            .ok()
            .map(|result| matches!(result, CompletionIrqRearm::Pending));
        (self.started, irq_sequence, irq_pending, completion_pending)
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
                        return self.finish_progress(
                            self.protocol_wait(now_nanos),
                            rearm_after_step,
                            now_nanos,
                        );
                    }
                    OperationProgress::Complete(info) => {
                        self.init = None;
                        self.initialize_device(info, now_nanos)?;
                        cause = ProgressCause::Submitted;
                        continue;
                    }
                }
            }

            if let Some(active) = &mut self.active {
                match active.advance(&mut self.card, cause)? {
                    OperationProgress::Pending => {
                        return self.finish_progress(
                            self.protocol_wait(now_nanos),
                            rearm_after_step,
                            now_nanos,
                        );
                    }
                    OperationProgress::Complete(completion) => {
                        self.active = None;
                        let action = self.complete_operation(completion, now_nanos)?;
                        if let Some(progress) = self.consume_action(action, now_nanos)? {
                            return self.finish_progress(progress, rearm_after_step, now_nanos);
                        }
                        cause = ProgressCause::Submitted;
                        continue;
                    }
                }
            }

            if let Some(control) = self.wifi_requests.try_pop() {
                self.outputs.begin_control(&control);
                let action = self.device_mut()?.advance(AicInput {
                    now: MonotonicTime::from_nanos(now_nanos),
                    event: Some(AicInputEvent::Control(control)),
                });
                if let Some(progress) = self.consume_action(action, now_nanos)? {
                    return self.finish_progress(progress, rearm_after_step, now_nanos);
                }
                continue;
            }
            if let Some(action) = self.submit_one_tx(now_nanos)?
                && let Some(progress) = self.consume_action(action, now_nanos)?
            {
                return self.finish_progress(progress, rearm_after_step, now_nanos);
            }
            let action = self
                .device_mut()?
                .advance(AicInput::tick(MonotonicTime::from_nanos(now_nanos)));
            if let Some(progress) = self.consume_action(action, now_nanos)? {
                return self.finish_progress(progress, rearm_after_step, now_nanos);
            }
        }
        self.finish_progress(
            OwnerProgress::Wait(OwnerWait::Interrupt),
            rearm_after_step,
            now_nanos,
        )
    }

    fn complete_operation(
        &mut self,
        completion: OperationCompletion,
        now_nanos: u64,
    ) -> Result<AicAction, AicRdifError> {
        Ok(self.device_mut()?.advance(AicInput {
            now: MonotonicTime::from_nanos(now_nanos),
            event: Some(AicInputEvent::Sdio(SdioCompletion {
                request_id: completion.request_id,
                result: Ok(completion.response),
            })),
        }))
    }

    fn consume_action(
        &mut self,
        mut action: AicAction,
        now_nanos: u64,
    ) -> Result<Option<OwnerProgress>, AicRdifError> {
        loop {
            self.card_irq_wait = CardIrqWait::Masked;
            match action {
                AicAction::SubmitSdio(request) => {
                    let mut active = ActiveOperation::submit(&mut self.card, request)?;
                    match active.advance(&mut self.card, ProgressCause::Submitted)? {
                        OperationProgress::Pending => {
                            self.active = Some(active);
                            return Ok(Some(self.protocol_wait(now_nanos)));
                        }
                        OperationProgress::Complete(completion) => {
                            action = self.complete_operation(completion, now_nanos)?;
                        }
                    }
                }
                AicAction::AbortSdio { request_id } => {
                    let mut active = self.active.take().ok_or(AicError::CompletionMismatch)?;
                    active.abort(&mut self.card)?;
                    action = self.device_mut()?.advance(AicInput {
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
                    self.card_irq_wait = CardIrqWait::Armed;
                    self.outputs
                        .publish_wait_progress(WifiControlProgress::WaitForInterrupt);
                    return Ok(Some(OwnerProgress::Wait(OwnerWait::Interrupt)));
                }
                AicAction::WaitForInterruptUntil(deadline) => {
                    self.card_irq_wait = CardIrqWait::Armed;
                    self.outputs.publish_wait_progress(
                        WifiControlProgress::WaitForInterruptUntil {
                            deadline_nanos: deadline.as_nanos(),
                        },
                    );
                    return Ok(Some(OwnerProgress::Wait(OwnerWait::InterruptUntil(
                        deadline.as_nanos(),
                    ))));
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
                    let ready = self.device()?.state() == AicState::Ready;
                    self.card_irq_wait = if ready {
                        CardIrqWait::Armed
                    } else {
                        CardIrqWait::Masked
                    };
                    return Ok(Some(if ready {
                        OwnerProgress::Ready
                    } else {
                        OwnerProgress::Wait(OwnerWait::Interrupt)
                    }));
                }
            }
        }
    }

    fn initialize_device(
        &mut self,
        info: SdioCardInfo,
        now_nanos: u64,
    ) -> Result<(), AicRdifError> {
        let function = FunctionNumber::new(1)
            .ok()
            .and_then(|number| self.card.function(number))
            .map(|function| function.cis);
        let variant = detect_sdio_card_variant(info, function)?;
        log::info!("[wifi] detected supported AIC SDIO variant {variant:?}");
        let mut device = AicDevice::new(variant)?;
        device.start(MonotonicTime::from_nanos(now_nanos))?;
        self.device = Some(device);
        self.started = true;
        Ok(())
    }

    fn submit_one_tx(&mut self, now_nanos: u64) -> Result<Option<AicAction>, AicRdifError> {
        if self.device()?.state() != AicState::Ready {
            return Ok(None);
        }
        let Some((token, frame)) = self.outputs.take_tx_frame() else {
            return Ok(None);
        };
        Ok(Some(self.device_mut()?.advance(AicInput {
            now: MonotonicTime::from_nanos(now_nanos),
            event: Some(AicInputEvent::Tx { token, frame }),
        })))
    }

    fn device(&self) -> Result<&AicDevice, AicRdifError> {
        self.device.as_ref().ok_or(AicRdifError::CoreUnavailable)
    }

    fn device_mut(&mut self) -> Result<&mut AicDevice, AicRdifError> {
        self.device.as_mut().ok_or(AicRdifError::CoreUnavailable)
    }

    fn protocol_wait(&self, now_nanos: u64) -> OwnerProgress {
        let retry_after = self
            .init
            .as_ref()
            .and_then(SdioInitRequest::register_retry_after);
        match retry_after
            .map(|retry_after| HostProgressWait::Register { retry_after })
            .unwrap_or_else(|| self.card.progress_wait())
        {
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
        now_nanos: u64,
    ) -> Result<OwnerProgress, AicRdifError> {
        if !rearm_after_step {
            return Ok(progress);
        }
        // Keep the completion-before-card ordering described in
        // `rearm_and_advance`; otherwise a CARD_INT asserted between the two
        // checks can be consumed by the SDHCI completion helper without being
        // published to the card-interrupt latch.
        let completion_pending = self.rearm_completion_irq()?;
        let card_pending = card_irq_rearm_allowed(
            self.card_irq_needed(),
            self.active.is_some(),
            self.card_irq_wait,
        ) && self
            .card_irq
            .as_mut()
            .is_some_and(CardIrqControl::rearm_and_check);
        if card_pending {
            self.irq_latch.publish_card_pending();
        }
        Ok(
            pending_irq_retry(self.started, card_pending, completion_pending, now_nanos)
                .map(|deadline| OwnerProgress::Wait(OwnerWait::RetryAt(deadline)))
                .unwrap_or(progress),
        )
    }

    fn rearm_completion_irq(&mut self) -> Result<bool, AicRdifError> {
        let pending = matches!(
            self.card.host_mut().rearm_completion_irq_and_check()?,
            CompletionIrqRearm::Pending
        );
        if pending {
            self.irq_latch.publish_completion_pending();
        }
        Ok(pending)
    }

    fn card_irq_needed(&self) -> bool {
        self.device
            .as_ref()
            .is_some_and(|device| device.card_irq_needed())
    }
}

const fn card_irq_rearm_allowed(
    card_protocol_ready: bool,
    sdio_operation_active: bool,
    wait: CardIrqWait,
) -> bool {
    card_protocol_ready && !sdio_operation_active && matches!(wait, CardIrqWait::Armed)
}

const fn pending_irq_retry(
    card_protocol_ready: bool,
    card_pending: bool,
    completion_pending: bool,
    now_nanos: u64,
) -> Option<u64> {
    if completion_pending || (card_protocol_ready && card_pending) {
        Some(now_nanos)
    } else {
        None
    }
}

fn detect_sdio_card_variant(
    info: SdioCardInfo,
    function: Option<CisInfo>,
) -> Result<ChipVariant, AicRdifError> {
    let common = observed_identity(info.common_cis);
    let function1 = function.map(observed_identity).unwrap_or(AicSdioIdentity {
        manufacturer_id: None,
        product_id: None,
    });
    let detected = function1
        .complete()
        .or_else(|| common.complete())
        .map(|(vid, did)| ChipVariant::from_vid_did(vid, did))
        .unwrap_or(ChipVariant::Unknown);
    if ChipProfile::for_variant(detected).is_none() {
        return Err(AicRdifError::UnsupportedCardIdentity {
            detected,
            io_functions: info.io_functions,
            function1,
            common,
        });
    }
    Ok(detected)
}

fn observed_identity(cis: CisInfo) -> AicSdioIdentity {
    AicSdioIdentity {
        manufacturer_id: cis.manufacturer_id,
        product_id: cis.product_id,
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::*;

    #[test]
    fn card_interrupt_stays_masked_until_sdio_enumeration_completes() {
        assert!(!card_irq_rearm_allowed(false, false, CardIrqWait::Armed));
        assert!(card_irq_rearm_allowed(true, false, CardIrqWait::Armed));
        assert!(!card_irq_rearm_allowed(true, true, CardIrqWait::Armed));
        assert!(!card_irq_rearm_allowed(true, false, CardIrqWait::Masked));
    }

    #[test]
    fn asserted_card_interrupt_is_republished_before_startup_waits() {
        assert_eq!(pending_irq_retry(false, true, false, 41), None);
        assert_eq!(pending_irq_retry(true, false, false, 41), None);
        assert_eq!(pending_irq_retry(true, true, false, 41), Some(41));
    }

    #[test]
    fn latched_completion_is_retried_even_before_card_startup_completes() {
        assert_eq!(pending_irq_retry(false, false, true, 41), Some(41));
    }

    #[test]
    fn common_cis_identifies_dc_when_function_one_omits_manfid() {
        let common = CisInfo {
            pointer: 0x1000,
            manufacturer_id: Some(0xc8a1),
            product_id: Some(0xc08d),
        };
        let function = CisInfo {
            pointer: 0x2000,
            manufacturer_id: None,
            product_id: None,
        };
        let info = SdioCardInfo {
            rca: 1,
            ocr: 0,
            io_functions: 2,
            cccr_revision: 3,
            sd_revision: 3,
            common_cis: common,
        };

        assert_eq!(
            detect_sdio_card_variant(info, Some(function)).unwrap(),
            ChipVariant::Aic8800DC
        );
    }

    #[test]
    fn rejected_identity_reports_function_and_common_manfid() {
        let common = CisInfo {
            pointer: 0x1000,
            manufacturer_id: Some(0xc8a1),
            product_id: Some(0x0082),
        };
        let function = CisInfo {
            pointer: 0x2000,
            manufacturer_id: Some(0xc8a1),
            product_id: Some(0x2082),
        };
        let info = SdioCardInfo {
            rca: 1,
            ocr: 0,
            io_functions: 2,
            cccr_revision: 3,
            sd_revision: 3,
            common_cis: common,
        };

        let error = detect_sdio_card_variant(info, Some(function))
            .unwrap_err()
            .to_string();

        assert!(error.contains("function1=c8a1:2082"), "{error}");
        assert!(error.contains("common=c8a1:0082"), "{error}");
        assert!(error.contains("io_functions=2"), "{error}");
    }
}
