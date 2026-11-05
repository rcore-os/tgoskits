//! Optional hardware command queue, owned by the card protocol scheduler.
use rdif_block::{BatchSubmitResult, BlkError, CompletionSink, OwnedRequestBatch, SubmissionSink};

use crate::block_queue::BlockConfig;
/// Host-engine activation facts, prepared and checked by the card protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandQueueActivation {
    pub rca: u16,
    pub card_depth: usize,
    pub flush_command: crate::Command,
    pub status_command: crate::Command,
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

    /// Returns whether the hardware engine currently owns the command path.
    /// Suspension temporarily transfers it to the protocol scheduler; shutdown
    /// remains terminal and never authorizes fallback or reactivation.
    fn is_active(&self) -> bool;

    /// Whether all staged, published, and direct tasks have been reclaimed.
    fn is_idle(&self) -> bool;

    /// Temporarily returns the drained command path to the protocol owner.
    /// Must synchronize IRQ readers and retire their completion generation.
    fn suspend(&mut self) -> Result<(), BlkError>;

    /// Restores the engine after successful legacy protocol maintenance.
    fn resume(&mut self) -> Result<(), BlkError>;

    /// Monotonic time for the protocol's fixed maintenance deadlines.
    fn now_ns(&self) -> u64;

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
