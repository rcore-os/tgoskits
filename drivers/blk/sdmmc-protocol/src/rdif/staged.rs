//! Discovery-to-ready RDIF adapter for SD/MMC controllers.

use alloc::boxed::Box;
use core::{cell::RefCell, mem};

use rdif_block::{
    BlkError, ControllerInitEndpoint, DeviceInfo, IdList, InitError, InitIrqProgress,
    InitialController, Interface, IrqHandler, IrqSourceList, LifecycleEndpoint, QueueHandle,
    QueueLimits,
};

use super::{
    BlockConfig, BlockDevice, host::BlockHost, irq::BlockIrqHandler, map_dev_err_to_blk_err,
    queue_limits,
};
use crate::sdio::{
    DeferredIrqAck, InitInput, InitPoll, InitializedSdioCard, OwnedSdioInit, OwnedSdioInitHost,
};

const CONTROLLER_SOURCE_ID: usize = 0;

/// Function that consumes successful card identification and constructs the
/// normal interrupt-backed RDIF device.
pub type ReadyBlockBuilder<H> = fn(InitializedSdioCard<H>, BlockConfig) -> BlockDevice<H>;

enum StagedState<H>
where
    H: BlockHost + OwnedSdioInitHost,
{
    Initial {
        init: Box<OwnedSdioInit<H>>,
        config: BlockConfig,
    },
    Ready(BlockDevice<H>),
    Failed(BlockDevice<H>),
    Transitioning,
}

/// RDIF interface that owns SD/MMC card initialization until it can publish a
/// normal block queue.
///
/// The runtime first takes and registers the initialization IRQ endpoint, then
/// enables controller delivery and drives [`InitialController`]. A successful
/// terminal transition updates capacity from [`crate::sdio::CardInfo`] and invokes the
/// ready builder. [`BlockDevice`] installs typed recovery storage while
/// retaining every controller-owned platform capability.
pub struct StagedBlockDevice<H>
where
    H: BlockHost + OwnedSdioInitHost,
{
    name: &'static str,
    state: RefCell<StagedState<H>>,
    init_irq_handler: Option<Box<dyn IrqHandler>>,
    ready_builder: ReadyBlockBuilder<H>,
}

impl<H> StagedBlockDevice<H>
where
    H: BlockHost + OwnedSdioInitHost,
    H::DataRequest<'static>: Send,
    H::BusRequest: Send,
{
    /// Build a discovery object without issuing any hardware operation.
    pub fn new(
        mut init: OwnedSdioInit<H>,
        config: BlockConfig,
        ready_builder: ReadyBlockBuilder<H>,
    ) -> Self {
        let name = config.name;
        let init_irq_handler = Some(Box::new(BlockIrqHandler::<H> {
            irq: init.irq_handle(),
            control: None,
        }) as Box<dyn IrqHandler>);
        Self {
            name,
            state: RefCell::new(StagedState::Initial {
                init: Box::new(init),
                config,
            }),
            init_irq_handler,
            ready_builder,
        }
    }

    fn map_init_error(error: crate::Error) -> InitError {
        match error {
            crate::Error::Timeout(_) => InitError::TimedOut,
            crate::Error::InvalidArgument => InitError::InvalidState,
            crate::Error::UnsupportedCommand => {
                InitError::Hardware("SD/MMC host does not support bounded initialization")
            }
            _ => InitError::Hardware("SD/MMC card initialization failed"),
        }
    }
}

impl<H> rdif_block::DriverGeneric for StagedBlockDevice<H>
where
    H: BlockHost + OwnedSdioInitHost,
    H::DataRequest<'static>: Send,
    H::BusRequest: Send,
{
    fn name(&self) -> &str {
        self.name
    }
}

impl<H> Interface for StagedBlockDevice<H>
where
    H: BlockHost + OwnedSdioInitHost,
    H::DataRequest<'static>: Send,
    H::BusRequest: Send,
{
    fn controller_init(&mut self) -> ControllerInitEndpoint<'_> {
        match self.state.get_mut() {
            StagedState::Ready(_) => ControllerInitEndpoint::Ready,
            StagedState::Initial { .. } | StagedState::Failed(_) => {
                ControllerInitEndpoint::Pending(self)
            }
            StagedState::Transitioning => {
                unreachable!("staged controller re-entered during terminal transition")
            }
        }
    }

    fn lifecycle(&mut self) -> LifecycleEndpoint<'_> {
        match self.state.get_mut() {
            StagedState::Ready(device) | StagedState::Failed(device) => device.lifecycle(),
            // Runtime lifecycle validation occurs only after initialization.
            // The discovery view must not fabricate a DMA-quiescence identity.
            StagedState::Initial { .. } => LifecycleEndpoint::Inline,
            StagedState::Transitioning => {
                unreachable!("staged controller exposed lifecycle while transitioning")
            }
        }
    }

    fn device_info(&self) -> DeviceInfo {
        match &*self.state.borrow() {
            StagedState::Ready(device) | StagedState::Failed(device) => device.device_info(),
            StagedState::Initial { config, .. } => super::device_info(config),
            StagedState::Transitioning => {
                unreachable!("staged controller exposed geometry while transitioning")
            }
        }
    }

    fn queue_limits(&self) -> QueueLimits {
        match &*self.state.borrow() {
            StagedState::Ready(device) | StagedState::Failed(device) => device.queue_limits(),
            StagedState::Initial { config, .. } => queue_limits(config, config.dma_mask),
            StagedState::Transitioning => {
                unreachable!("staged controller exposed limits while transitioning")
            }
        }
    }

    fn create_queue(&mut self) -> Option<QueueHandle> {
        match self.state.get_mut() {
            StagedState::Ready(device) => device.create_queue(),
            StagedState::Initial { .. } | StagedState::Failed(_) => None,
            StagedState::Transitioning => {
                unreachable!("staged controller created a queue while transitioning")
            }
        }
    }

    fn enable_irq(&self) -> Result<(), BlkError> {
        match &mut *self.state.borrow_mut() {
            StagedState::Ready(device) | StagedState::Failed(device) => device.enable_irq(),
            StagedState::Initial { init, .. } => {
                init.enable_completion_irq()
                    .map_err(map_dev_err_to_blk_err)?;
                init.completion_irq_enabled()
                    .then_some(())
                    .ok_or(BlkError::Io)
            }
            StagedState::Transitioning => Err(BlkError::Offline),
        }
    }

    fn disable_irq(&self) -> Result<(), BlkError> {
        match &mut *self.state.borrow_mut() {
            StagedState::Ready(device) | StagedState::Failed(device) => device.disable_irq(),
            StagedState::Initial { init, .. } => {
                init.disable_completion_irq()
                    .map_err(map_dev_err_to_blk_err)?;
                (!init.completion_irq_enabled())
                    .then_some(())
                    .ok_or(BlkError::Io)
            }
            StagedState::Transitioning => Err(BlkError::Offline),
        }
    }

    fn is_irq_enabled(&self) -> bool {
        match &*self.state.borrow() {
            StagedState::Ready(device) | StagedState::Failed(device) => device.is_irq_enabled(),
            StagedState::Initial { init, .. } => init.completion_irq_enabled(),
            StagedState::Transitioning => false,
        }
    }

    fn irq_sources(&self) -> IrqSourceList {
        match &*self.state.borrow() {
            StagedState::Ready(device) | StagedState::Failed(device) => device.irq_sources(),
            StagedState::Initial { .. } | StagedState::Transitioning => alloc::vec::Vec::new(),
        }
    }

    fn take_irq_handler(&mut self, source_id: usize) -> Option<Box<dyn IrqHandler>> {
        match self.state.get_mut() {
            StagedState::Ready(device) => device.take_irq_handler(source_id),
            StagedState::Initial { .. } | StagedState::Failed(_) | StagedState::Transitioning => {
                None
            }
        }
    }
}

impl<H> InitialController for StagedBlockDevice<H>
where
    H: BlockHost + OwnedSdioInitHost,
    H::DataRequest<'static>: Send,
    H::BusRequest: Send,
{
    fn irq_sources(&self) -> IdList {
        IdList::from_bits(1u64 << CONTROLLER_SOURCE_ID)
    }

    fn take_irq_handler(&mut self, source_id: usize) -> Option<Box<dyn IrqHandler>> {
        (source_id == CONTROLLER_SOURCE_ID)
            .then(|| self.init_irq_handler.take())
            .flatten()
    }

    fn service_deferred_irq(&mut self, source_id: usize) -> InitIrqProgress {
        if source_id != CONTROLLER_SOURCE_ID {
            return InitIrqProgress::Unhandled;
        }
        let StagedState::Initial { init, .. } = self.state.get_mut() else {
            return InitIrqProgress::Unhandled;
        };
        match init.acknowledge_deferred_irq() {
            DeferredIrqAck::Unhandled => InitIrqProgress::Unhandled,
            DeferredIrqAck::Acknowledged => InitIrqProgress::Acknowledged,
            DeferredIrqAck::Contended => InitIrqProgress::Deferred,
        }
    }

    fn poll_init(&mut self, input: rdif_block::InitInput) -> rdif_block::InitPoll<()> {
        // Taking the endpoint proves that OS glue owns a registered IRQ
        // action. Enabling delivery proves that commands cannot enter the
        // controller before their only completion path exists.
        if self.init_irq_handler.is_some() {
            return rdif_block::InitPoll::Failed(InitError::InvalidState);
        }
        if matches!(
            self.state.get_mut(),
            StagedState::Initial { init, .. } if !init.completion_irq_enabled()
        ) {
            return rdif_block::InitPoll::Failed(InitError::InvalidState);
        }

        let progress = match self.state.get_mut() {
            StagedState::Ready(_) => return rdif_block::InitPoll::Ready(()),
            StagedState::Failed(_) | StagedState::Transitioning => {
                return rdif_block::InitPoll::Failed(InitError::InvalidState);
            }
            StagedState::Initial { init, .. } => {
                init.poll_init(InitInput::from_rdif(input, CONTROLLER_SOURCE_ID))
            }
        };

        match progress {
            InitPoll::Pending(schedule) => match schedule.into_rdif(CONTROLLER_SOURCE_ID) {
                Ok(schedule) => rdif_block::InitPoll::Pending(schedule),
                Err(error) => rdif_block::InitPoll::Failed(error),
            },
            InitPoll::Failed(error) => rdif_block::InitPoll::Failed(Self::map_init_error(error)),
            InitPoll::Ready(_) => self.finish_initialization(),
        }
    }
}

impl<H> StagedBlockDevice<H>
where
    H: BlockHost + OwnedSdioInitHost,
    H::DataRequest<'static>: Send,
    H::BusRequest: Send,
{
    fn finish_initialization(&mut self) -> rdif_block::InitPoll<()> {
        let state = mem::replace(self.state.get_mut(), StagedState::Transitioning);
        let StagedState::Initial { init, mut config } = state else {
            return rdif_block::InitPoll::Failed(InitError::InvalidState);
        };
        let initialized = match (*init).try_into_ready() {
            Ok(ready) => ready,
            Err(init) => {
                *self.state.get_mut() = StagedState::Initial { init, config };
                return rdif_block::InitPoll::Failed(InitError::InvalidState);
            }
        };

        let Some(capacity_blocks) = initialized.card_info().capacity_blocks else {
            let failed = (self.ready_builder)(initialized, config);
            *self.state.get_mut() = StagedState::Failed(failed);
            return rdif_block::InitPoll::Failed(InitError::Hardware(
                "SD/MMC card did not publish a usable capacity",
            ));
        };
        config.capacity_blocks = capacity_blocks;
        let ready = (self.ready_builder)(initialized, config);
        *self.state.get_mut() = StagedState::Ready(ready);
        rdif_block::InitPoll::Ready(())
    }
}
