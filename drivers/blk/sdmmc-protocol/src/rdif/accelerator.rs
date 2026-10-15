//! Optional hardware queue acceleration for initialized eMMC cards.

use rdif_block::{BatchSubmitResult, BlkError, CompletionSink, OwnedRequestBatch, SubmissionSink};

use super::config::BlockConfig;

/// Card-side facts required to activate a hardware command queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandQueueActivation {
    /// Relative card address used by automatic status commands.
    pub rca: u16,
    /// Number of data tags supported by the eMMC device.
    pub card_depth: usize,
    /// Whether the eMMC volatile write cache needs an explicit flush command.
    pub cache_enabled: bool,
}

impl CommandQueueActivation {
    /// Cache flush command, or a status barrier when the cache is disabled.
    pub fn flush_command(self) -> crate::Command {
        if self.cache_enabled {
            crate::cmd::cmd6_mmc_switch(3, crate::cmd::ext_csd::FLUSH_CACHE as u8, 1)
        } else {
            self.status_command()
        }
    }

    /// Status command used to verify the result of a completed cache flush.
    pub fn status_command(self) -> crate::Command {
        crate::cmd::cmd13(self.rca)
    }

    /// Validates card-side errors after a CQHCI direct command.
    pub fn check_response(raw: u32, final_status: bool) -> Result<(), BlkError> {
        let response =
            crate::response::R1Response::from_native_raw(raw).map_err(|_| BlkError::Io)?;
        if response.switch_error()
            || (final_status
                && (!response.ready_for_data()
                    || response.current_state() != crate::response::CardState::Transfer))
        {
            return Err(BlkError::Io);
        }
        Ok(())
    }
}

/// Optional hardware queue owned by the SD/MMC block maintenance task.
///
/// Card discovery and `CMDQ_MODE_EN` remain in the protocol layer. The
/// accelerator owns only the host-controller queue after activation.
pub trait CommandQueueAccelerator: Send + 'static {
    /// Maximum data-task depth that the runtime must provision before card
    /// discovery. Activation may shrink this to the card's reported depth.
    fn provisioned_depth(&self) -> usize;

    /// Activates the hardware engine and replaces legacy queue limits.
    fn activate(
        &mut self,
        activation: CommandQueueActivation,
        config: &mut BlockConfig,
    ) -> Result<(), BlkError>;

    /// Returns whether successful activation transferred the card's data path.
    ///
    /// Ownership remains true after shutdown or a runtime failure: stopping
    /// the host does not disable CMDQ_MODE_EN on the card. A closed accelerator
    /// must keep rejecting requests instead of routing them to the legacy host.
    /// Activation failure leaves ownership false for card-side rollback.
    fn is_active(&self) -> bool;

    /// Stages an ordered request prefix into free hardware tags.
    fn submit_batch_owned(
        &mut self,
        requests: &mut OwnedRequestBatch,
        sink: &mut dyn SubmissionSink,
    ) -> BatchSubmitResult;

    /// Publishes every descriptor staged by the preceding submission call.
    fn commit_submissions(&mut self) -> Result<(), BlkError>;

    /// Completes tags acknowledged by the hard IRQ endpoint.
    fn drain_completions(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError>;

    /// Quiesces the engine and returns or quarantines all DMA ownership.
    fn shutdown(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError>;
}
