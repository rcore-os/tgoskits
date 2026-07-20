use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};

use rdif_block::{
    BlkError, BlockIrqSource, InitError, InitInput, InitPoll, InitSchedule, Interface,
    InterruptLifecycle, LifecycleEndpoint, QueueHandle, RecoveryCause,
};

use crate::{
    rdif::{
        config::{BlockConfig, device_info, map_dev_err_to_blk_err, queue_limits},
        host::BlockHost,
        irq::into_block_irq_source,
        queue::BlockQueue,
        shared_core::SharedCore,
    },
    sdio::{
        InitializedSdioCard,
        card::SdioSdmmc,
        host::{SDMMC_BLOCK_QUEUE_ID, SdioHost, SdioIrqHost},
    },
};

pub struct BlockDevice<H>
where
    H: BlockHost,
{
    pub(super) control: Arc<BlockControl<H>>,
    recovery: ControllerRecovery<H::RecoveryState>,
}

enum ControllerRecovery<R> {
    Idle,
    GuestOwned,
    Quiescing {
        epoch: rdif_block::ControllerEpoch,
        host: R,
    },
    Quiesced {
        epoch: rdif_block::ControllerEpoch,
        host: R,
    },
    Reinitializing {
        epoch: rdif_block::ControllerEpoch,
        host: R,
    },
}

pub struct BlockControl<H>
where
    H: BlockHost,
{
    pub(super) raw: SharedCore<SdioSdmmc<H>>,
    pub(super) config: BlockConfig,
    pub(super) irq_enabled: AtomicBool,
    pub(super) queue_taken: AtomicBool,
}

impl<H> BlockDevice<H>
where
    H: BlockHost,
{
    /// Construct the normal interrupt-backed device from successful card
    /// initialization.
    ///
    /// A raw [`SdioSdmmc`] cannot enter this path. The capability is produced
    /// only by the bounded initialization state machine and is consumed once.
    /// Controller platform capabilities remain owned by the resulting device
    /// so recovery and guest-return reconstruction can reuse them. This
    /// constructor deliberately does not acquire the runtime IRQ source: the
    /// runtime first closes the initialization action, then acquires the next
    /// exclusive source lease through [`Interface::take_irq_source`].
    ///
    /// ```compile_fail
    /// use sdmmc_protocol::{
    ///     rdif::{BlockConfig, BlockDevice, BlockHost},
    ///     sdio::SdioSdmmc,
    /// };
    ///
    /// fn bypass_initialization<H: BlockHost>(card: SdioSdmmc<H>, config: BlockConfig) {
    ///     let _ = BlockDevice::from_initialized(card, config);
    /// }
    /// ```
    pub fn from_initialized(initialized: InitializedSdioCard<H>, config: BlockConfig) -> Self {
        let (mut card, _info) = initialized.into_parts();
        card.host_mut().prepare_block_runtime();
        Self::from_card(card, config)
    }

    fn from_card(card: SdioSdmmc<H>, config: BlockConfig) -> Self {
        let raw = SharedCore::new(card);
        let control = Arc::new(BlockControl {
            raw,
            config,
            irq_enabled: AtomicBool::new(false),
            queue_taken: AtomicBool::new(false),
        });
        Self {
            control,
            recovery: ControllerRecovery::Idle,
        }
    }

    #[cfg(test)]
    pub(crate) fn from_card_for_test(card: SdioSdmmc<H>, config: BlockConfig) -> Self {
        Self::from_card(card, config)
    }

    pub fn config(&self) -> &BlockConfig {
        &self.control.config
    }

    fn queue_limits_with_mask(&self, dma_mask: u64) -> rdif_block::QueueLimits {
        queue_limits(&self.control.config, dma_mask)
    }
}

impl<H> BlockControl<H>
where
    H: BlockHost,
{
    pub(super) fn controller_cookie(&self) -> usize {
        core::ptr::from_ref(self).expose_provenance()
    }

    pub(super) fn claim_queue(&self) -> bool {
        self.queue_taken
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub(super) fn release_queue(&self) {
        self.queue_taken.store(false, Ordering::Release);
    }
}

impl<H> rdif_block::DriverGeneric for BlockDevice<H>
where
    H: BlockHost,
{
    fn name(&self) -> &str {
        self.control.config.name
    }
}

impl<H> Interface for BlockDevice<H>
where
    H: BlockHost,
{
    fn controller_init(&mut self) -> rdif_block::ControllerInitEndpoint<'_> {
        rdif_block::ControllerInitEndpoint::Ready
    }

    fn lifecycle(&mut self) -> LifecycleEndpoint<'_> {
        LifecycleEndpoint::Interrupt(self)
    }

    fn device_info(&self) -> rdif_block::DeviceInfo {
        device_info(&self.control.config)
    }

    fn queue_limits(&self) -> rdif_block::QueueLimits {
        self.queue_limits_with_mask(self.control.config.dma_mask)
    }

    fn create_queue(&mut self) -> Option<QueueHandle> {
        if !self.control.config.supports_runtime_queue() || !self.control.claim_queue() {
            return None;
        }
        Some(QueueHandle::new(Box::new(BlockQueue::<H>::new(
            Arc::clone(&self.control),
            SDMMC_BLOCK_QUEUE_ID,
        ))))
    }

    fn enable_irq(&self) -> Result<(), BlkError> {
        if !self.control.config.supports_runtime_queue() {
            self.control.irq_enabled.store(false, Ordering::Release);
            return Err(BlkError::NotSupported);
        }
        let mut raw = self
            .control
            .raw
            .try_borrow_mut()
            .map_err(|_| BlkError::Busy)?;
        let result = SdioHost::enable_completion_irq(raw.host_mut())
            .map_err(map_dev_err_to_blk_err)
            .and_then(|()| {
                raw.host()
                    .completion_irq_enabled()
                    .then_some(())
                    .ok_or(BlkError::Io)
            });
        drop(raw);
        self.control
            .irq_enabled
            .store(result.is_ok(), Ordering::Release);
        result
    }

    fn disable_irq(&self) -> Result<(), BlkError> {
        if !self.control.config.supports_runtime_queue() {
            self.control.irq_enabled.store(false, Ordering::Release);
            return Err(BlkError::NotSupported);
        }
        let mut raw = self
            .control
            .raw
            .try_borrow_mut()
            .map_err(|_| BlkError::Busy)?;
        let (result, enabled) = {
            let result =
                SdioHost::disable_completion_irq(raw.host_mut()).map_err(map_dev_err_to_blk_err);
            (result, raw.host().completion_irq_enabled())
        };
        drop(raw);
        self.control.irq_enabled.store(enabled, Ordering::Release);
        result?;
        if enabled {
            return Err(BlkError::Io);
        }
        Ok(())
    }

    fn is_irq_enabled(&self) -> bool {
        self.control.irq_enabled.load(Ordering::Acquire)
    }

    fn irq_sources(&self) -> rdif_block::IrqSourceList {
        if !self.control.config.supports_runtime_queue() {
            return Vec::new();
        }
        vec![rdif_block::IrqSourceInfo::legacy(
            rdif_block::IdList::from_bits(1),
        )]
    }

    fn take_irq_source(&mut self, source_id: usize) -> Option<BlockIrqSource> {
        if !self.control.config.supports_runtime_queue() || source_id != 0 {
            return None;
        }
        let mut card = self.control.raw.try_borrow_mut().ok()?;
        SdioIrqHost::take_irq_source(card.host_mut()).map(into_block_irq_source)
    }
}

impl<H> InterruptLifecycle for BlockDevice<H>
where
    H: BlockHost,
{
    fn controller_cookie(&self) -> usize {
        self.control.controller_cookie()
    }

    fn begin_dma_quiesce(
        &mut self,
        epoch: rdif_block::ControllerEpoch,
        cause: RecoveryCause,
    ) -> Result<(), InitError> {
        if !matches!(
            self.recovery,
            ControllerRecovery::Idle | ControllerRecovery::GuestOwned
        ) || self.control.irq_enabled.load(Ordering::Acquire)
        {
            return Err(InitError::InvalidState);
        }
        let mut card = self
            .control
            .raw
            .try_borrow_mut()
            // Admission, queue work, and IRQ delivery must already be closed
            // before this one-shot lifecycle transition.
            .map_err(|_| InitError::InvalidState)?;
        let host = H::begin_recovery(card.host_mut(), cause)
            .map_err(|_| InitError::Hardware("SD/MMC controller could not enter recovery"))?;
        drop(card);
        self.recovery = ControllerRecovery::Quiescing { epoch, host };
        Ok(())
    }

    fn poll_dma_quiesce(&mut self, input: InitInput) -> InitPoll<rdif_block::DmaQuiesced> {
        let ControllerRecovery::Quiescing { host, .. } = &mut self.recovery else {
            return InitPoll::Failed(InitError::InvalidState);
        };
        let mut card = match self.control.raw.try_borrow_mut() {
            Ok(card) => card,
            // This is software-gate contention, not hardware-state discovery.
            // The runtime queues one more bounded lifecycle pass.
            Err(_) => return InitPoll::Pending(InitSchedule::immediate()),
        };
        let progress = H::poll_dma_quiesce(card.host_mut(), host, input);
        drop(card);
        match progress {
            InitPoll::Ready(()) => {
                let recovery = core::mem::replace(&mut self.recovery, ControllerRecovery::Idle);
                let ControllerRecovery::Quiescing { epoch, host } = recovery else {
                    unreachable!("matched quiescing recovery before state replacement")
                };
                self.recovery = ControllerRecovery::Quiesced { epoch, host };
                let cookie = self.controller_cookie();
                InitPoll::Ready(unsafe {
                    // SAFETY: the host-specific lifecycle returned Ready only
                    // after reset/stop status proved that every DMA and PIO
                    // engine was idle. Runtime admission, device IRQ delivery,
                    // and the registered IRQ action were already closed before
                    // `begin_dma_quiesce` was called.
                    rdif_block::DmaQuiesced::new(epoch, cookie)
                })
            }
            InitPoll::Pending(schedule) => InitPoll::Pending(schedule),
            InitPoll::Failed(error) => InitPoll::Failed(error),
        }
    }

    fn enter_guest_owned(&mut self, quiesced: rdif_block::DmaQuiesced) -> Result<(), InitError> {
        if quiesced.controller_cookie() != self.controller_cookie() {
            return Err(InitError::InvalidState);
        }
        let recovery = core::mem::replace(&mut self.recovery, ControllerRecovery::Idle);
        let (epoch, host) = match recovery {
            ControllerRecovery::Quiesced { epoch, host } => (epoch, host),
            recovery => {
                self.recovery = recovery;
                return Err(InitError::InvalidState);
            }
        };
        if quiesced.epoch() != epoch {
            self.recovery = ControllerRecovery::Quiesced { epoch, host };
            return Err(InitError::InvalidState);
        }
        drop(host);
        self.recovery = ControllerRecovery::GuestOwned;
        Ok(())
    }

    fn begin_reinitialize(&mut self, quiesced: rdif_block::DmaQuiesced) -> Result<(), InitError> {
        if quiesced.controller_cookie() != self.controller_cookie() {
            return Err(InitError::InvalidState);
        }
        let recovery = core::mem::replace(&mut self.recovery, ControllerRecovery::Idle);
        let ControllerRecovery::Quiesced { epoch, mut host } = recovery else {
            return Err(InitError::InvalidState);
        };
        if quiesced.epoch() != epoch {
            self.recovery = ControllerRecovery::Quiesced { epoch, host };
            return Err(InitError::InvalidState);
        }
        let mut card = match self.control.raw.try_borrow_mut() {
            Ok(card) => card,
            Err(_) => {
                self.recovery = ControllerRecovery::Quiesced { epoch, host };
                return Err(InitError::InvalidState);
            }
        };
        let reinitialize = H::begin_reinitialize(card.host_mut(), &mut host);
        drop(card);
        if reinitialize.is_err() {
            self.recovery = ControllerRecovery::Quiesced { epoch, host };
            return Err(InitError::Hardware(
                "SD/MMC controller could not begin reinitialization",
            ));
        }
        self.recovery = ControllerRecovery::Reinitializing { epoch, host };
        Ok(())
    }

    fn poll_reinitialize(&mut self, input: InitInput) -> InitPoll<rdif_block::ControllerReady> {
        let ControllerRecovery::Reinitializing { epoch, host } = &mut self.recovery else {
            return InitPoll::Failed(InitError::InvalidState);
        };
        let mut card = match self.control.raw.try_borrow_mut() {
            Ok(card) => card,
            // As above, retry only the bounded reconstruction state machine;
            // no normal-I/O completion state is inspected by a timer.
            Err(_) => return InitPoll::Pending(InitSchedule::immediate()),
        };
        let progress = H::poll_reinitialize(card.host_mut(), host, input);
        drop(card);
        match progress {
            InitPoll::Ready(()) => {
                let epoch = *epoch;
                self.recovery = ControllerRecovery::Idle;
                InitPoll::Ready(unsafe {
                    // SAFETY: the host-specific reconstruction returned Ready
                    // only after all controller-owned queue, clock, bus, DMA,
                    // and interrupt configuration was restored for this epoch.
                    rdif_block::ControllerReady::new(epoch, self.controller_cookie())
                })
            }
            InitPoll::Pending(schedule) => InitPoll::Pending(schedule),
            InitPoll::Failed(error) => InitPoll::Failed(error),
        }
    }
}
