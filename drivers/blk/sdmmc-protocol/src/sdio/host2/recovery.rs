//! Typed controller-wide recovery support for the host2 adapter.

use alloc::boxed::Box;
use core::marker::PhantomData;

use super::{SdioHost2Adapter, SdioHost2Irq};

/// Generation token for a preallocated host recovery session.
pub struct SdioHost2Recovery<H: SdioHost2Irq + 'static> {
    generation: u64,
    _host: PhantomData<fn() -> H>,
}

/// Controller-wide recovery capability used by the IRQ-only block runtime.
///
/// The associated state is owned by the protocol adapter while normal queue
/// access is closed. Each poll performs bounded work and communicates its next
/// activation with [`rdif_block::InitPoll`]. Implementations must not infer
/// DMA quiescence from elapsed time alone: `Ready(())` is a hardware promise
/// that every DMA/PIO engine has stopped.
pub trait SdioHost2Lifecycle: SdioHost2Irq {
    type RecoveryState: Send + 'static;

    fn begin_recovery(
        &mut self,
        cause: rdif_block::RecoveryCause,
    ) -> Result<Self::RecoveryState, crate::Error>;

    fn poll_dma_quiesce(
        &mut self,
        state: &mut Self::RecoveryState,
        input: rdif_block::InitInput,
    ) -> rdif_block::InitPoll<()>;

    fn begin_reinitialize(&mut self, state: &mut Self::RecoveryState) -> Result<(), crate::Error>;

    fn poll_reinitialize(
        &mut self,
        state: &mut Self::RecoveryState,
        input: rdif_block::InitInput,
    ) -> rdif_block::InitPoll<()>;
}

pub(super) trait ErasedRecovery<H>: Send {
    fn begin(
        &mut self,
        host: &mut H,
        cause: rdif_block::RecoveryCause,
    ) -> Result<u64, crate::Error>;

    fn poll_dma_quiesce(
        &mut self,
        host: &mut H,
        generation: u64,
        input: rdif_block::InitInput,
    ) -> rdif_block::InitPoll<()>;

    fn begin_reinitialize(&mut self, host: &mut H, generation: u64) -> Result<(), crate::Error>;

    fn poll_reinitialize(
        &mut self,
        host: &mut H,
        generation: u64,
        input: rdif_block::InitInput,
    ) -> rdif_block::InitPoll<()>;
}

pub(super) struct TypedRecovery<R> {
    pub(super) state: Option<R>,
    pub(super) generation: u64,
}

impl<H, R> ErasedRecovery<H> for TypedRecovery<R>
where
    H: SdioHost2Lifecycle<RecoveryState = R>,
    R: Send + 'static,
{
    fn begin(
        &mut self,
        host: &mut H,
        cause: rdif_block::RecoveryCause,
    ) -> Result<u64, crate::Error> {
        if self.state.is_some() {
            return Err(crate::Error::Busy);
        }
        let state = host.begin_recovery(cause)?;
        self.generation = self.generation.wrapping_add(1).max(1);
        self.state = Some(state);
        Ok(self.generation)
    }

    fn poll_dma_quiesce(
        &mut self,
        host: &mut H,
        generation: u64,
        input: rdif_block::InitInput,
    ) -> rdif_block::InitPoll<()> {
        if generation != self.generation {
            return rdif_block::InitPoll::Failed(rdif_block::InitError::InvalidState);
        }
        let Some(state) = self.state.as_mut() else {
            return rdif_block::InitPoll::Failed(rdif_block::InitError::InvalidState);
        };
        host.poll_dma_quiesce(state, input)
    }

    fn begin_reinitialize(&mut self, host: &mut H, generation: u64) -> Result<(), crate::Error> {
        if generation != self.generation {
            return Err(crate::Error::InvalidArgument);
        }
        let state = self.state.as_mut().ok_or(crate::Error::InvalidArgument)?;
        host.begin_reinitialize(state)
    }

    fn poll_reinitialize(
        &mut self,
        host: &mut H,
        generation: u64,
        input: rdif_block::InitInput,
    ) -> rdif_block::InitPoll<()> {
        if generation != self.generation {
            return rdif_block::InitPoll::Failed(rdif_block::InitError::InvalidState);
        }
        let Some(state) = self.state.as_mut() else {
            return rdif_block::InitPoll::Failed(rdif_block::InitError::InvalidState);
        };
        let progress = host.poll_reinitialize(state, input);
        if matches!(progress, rdif_block::InitPoll::Ready(())) {
            self.state = None;
        }
        progress
    }
}

impl<H: SdioHost2Irq + 'static> SdioHost2Adapter<H> {
    /// Install the controller's typed, non-blocking RDIF recovery backend.
    pub fn enable_block_lifecycle(&mut self)
    where
        H: SdioHost2Lifecycle,
    {
        if self.recovery.is_none() {
            self.recovery = Some(Box::new(TypedRecovery::<H::RecoveryState> {
                state: None,
                generation: 0,
            }));
        }
    }

    pub(crate) fn begin_block_recovery(
        &mut self,
        cause: rdif_block::RecoveryCause,
    ) -> Result<SdioHost2Recovery<H>, crate::Error> {
        let recovery = self
            .recovery
            .as_mut()
            .ok_or(crate::Error::UnsupportedCommand)?;
        let generation = self.core.with_mut(|host| recovery.begin(host, cause))?;
        Ok(SdioHost2Recovery {
            generation,
            _host: PhantomData,
        })
    }

    pub(crate) fn poll_block_dma_quiesce(
        &mut self,
        recovery: &mut SdioHost2Recovery<H>,
        input: rdif_block::InitInput,
    ) -> rdif_block::InitPoll<()> {
        let Some(slot) = self.recovery.as_mut() else {
            return rdif_block::InitPoll::Failed(rdif_block::InitError::InvalidState);
        };
        self.core
            .with_mut(|host| slot.poll_dma_quiesce(host, recovery.generation, input))
    }

    pub(crate) fn begin_block_reinitialize(
        &mut self,
        recovery: &mut SdioHost2Recovery<H>,
    ) -> Result<(), crate::Error> {
        let slot = self
            .recovery
            .as_mut()
            .ok_or(crate::Error::InvalidArgument)?;
        self.core
            .with_mut(|host| slot.begin_reinitialize(host, recovery.generation))
    }

    pub(crate) fn poll_block_reinitialize(
        &mut self,
        recovery: &mut SdioHost2Recovery<H>,
        input: rdif_block::InitInput,
    ) -> rdif_block::InitPoll<()> {
        let Some(slot) = self.recovery.as_mut() else {
            return rdif_block::InitPoll::Failed(rdif_block::InitError::InvalidState);
        };
        self.core
            .with_mut(|host| slot.poll_reinitialize(host, recovery.generation, input))
    }
}
