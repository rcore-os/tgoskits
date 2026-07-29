use alloc::{boxed::Box, sync::Arc, vec};
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

use rdif_block::{
    BlkError, BlockController, ControllerEvent, ControllerState, ControllerUpdate, DeviceInfo,
    DriverGeneric, HardIrqHandler, HardwareQueue, IrqEndpoint,
};

use crate::{
    rdif::{
        config::{BlockConfig, device_info},
        irq::BlockIrqHandler,
        queue::BlockQueue,
    },
    sdio::{card::SdioSdmmc, host::SdioIrqHost, init::CardInitPreference},
};

const INIT_INITIALIZING: u8 = 0;
const INIT_READY: u8 = 1;
const INIT_FAILED: u8 = 2;

pub(super) struct BlockInitStatus {
    state: AtomicU8,
    capacity_blocks: AtomicU64,
    controller_wake: AtomicBool,
}

impl BlockInitStatus {
    fn initialized(capacity_blocks: u64) -> Self {
        Self {
            state: AtomicU8::new(INIT_READY),
            capacity_blocks: AtomicU64::new(capacity_blocks),
            controller_wake: AtomicBool::new(false),
        }
    }

    fn initializing() -> Self {
        Self {
            state: AtomicU8::new(INIT_INITIALIZING),
            capacity_blocks: AtomicU64::new(0),
            controller_wake: AtomicBool::new(true),
        }
    }

    pub(super) fn mark_ready(&self, capacity_blocks: u64) {
        self.capacity_blocks
            .store(capacity_blocks, Ordering::Release);
        self.state.store(INIT_READY, Ordering::Release);
        self.controller_wake.store(false, Ordering::Release);
    }

    pub(super) fn mark_failed(&self) {
        self.state.store(INIT_FAILED, Ordering::Release);
        self.controller_wake.store(false, Ordering::Release);
    }

    pub(super) fn needs_controller_wake(&self) -> bool {
        self.controller_wake.load(Ordering::Acquire)
    }

    fn capacity_blocks(&self) -> u64 {
        self.capacity_blocks.load(Ordering::Acquire)
    }

    fn controller_state(&self) -> Result<ControllerState, BlkError> {
        match self.state.load(Ordering::Acquire) {
            INIT_INITIALIZING => Ok(ControllerState::WaitingForIrq),
            INIT_READY => Ok(ControllerState::Ready),
            INIT_FAILED => Err(BlkError::Io),
            _ => Err(BlkError::InvalidRequest),
        }
    }
}

/// Interrupt-driven single-queue SD/MMC controller.
pub struct BlockDevice<H>
where
    H: SdioIrqHost + Send + 'static,
    H::TransactionRequest<'static>: Send,
    H::BusRequest: Send,
{
    card: Option<SdioSdmmc<H>>,
    config: BlockConfig,
    irq_handler: Option<Box<dyn HardIrqHandler>>,
    init_preference: Option<CardInitPreference>,
    init_status: Arc<BlockInitStatus>,
    started: bool,
    stopped: bool,
}

impl<H> BlockDevice<H>
where
    H: SdioIrqHost + Send + 'static,
    H::TransactionRequest<'static>: Send,
    H::BusRequest: Send,
{
    pub fn new(mut card: SdioSdmmc<H>, config: BlockConfig) -> Self {
        let init_status = Arc::new(BlockInitStatus::initialized(config.capacity_blocks()));
        let irq_handler = Box::new(BlockIrqHandler::<H> {
            irq: SdioIrqHost::irq_handle(card.host_mut()),
            init_status: Arc::clone(&init_status),
        });
        Self {
            card: Some(card),
            config,
            irq_handler: Some(irq_handler),
            init_preference: None,
            init_status,
            started: false,
            stopped: false,
        }
    }

    /// Creates a controller whose eMMC/SD protocol initialization is owned by
    /// the hctx task and advances command/data states only after IRQ ack.
    pub fn new_initializing(
        mut card: SdioSdmmc<H>,
        config: BlockConfig,
        preference: CardInitPreference,
    ) -> Self {
        let init_status = Arc::new(BlockInitStatus::initializing());
        let irq_handler = Box::new(BlockIrqHandler::<H> {
            irq: SdioIrqHost::irq_handle(card.host_mut()),
            init_status: Arc::clone(&init_status),
        });
        Self {
            card: Some(card),
            config,
            irq_handler: Some(irq_handler),
            init_preference: Some(preference),
            init_status,
            started: false,
            stopped: false,
        }
    }

    pub const fn config(&self) -> &BlockConfig {
        &self.config
    }

    fn start(&mut self) -> Result<ControllerUpdate, BlkError> {
        if self.started || self.stopped {
            return Err(BlkError::NotSupported);
        }
        let card = self.card.take().ok_or(BlkError::InvalidRequest)?;
        let handler = self.irq_handler.take().ok_or(BlkError::InvalidRequest)?;
        let queue: Box<dyn HardwareQueue> = if let Some(preference) = self.init_preference {
            Box::new(BlockQueue::new_initializing(
                card,
                self.config,
                0,
                preference,
                Arc::clone(&self.init_status),
            )?)
        } else {
            Box::new(BlockQueue::new(card, self.config, 0))
        };
        self.started = true;
        let state = self.init_status.controller_state()?;
        let mut update = ControllerUpdate::with_resources(
            state,
            vec![queue],
            vec![IrqEndpoint::new(0, 1, handler)],
        );
        if state == ControllerState::Ready {
            update = update.with_device_info(self.device_info());
        }
        Ok(update)
    }

    fn current_update(&self) -> Result<ControllerUpdate, BlkError> {
        let state = self.init_status.controller_state()?;
        let mut update = ControllerUpdate::state(state);
        if state == ControllerState::Ready {
            update = update.with_device_info(self.device_info());
        }
        Ok(update)
    }
}

impl<H> DriverGeneric for BlockDevice<H>
where
    H: SdioIrqHost + Send + 'static,
    H::TransactionRequest<'static>: Send,
    H::BusRequest: Send,
{
    fn name(&self) -> &str {
        self.config.name()
    }
}

impl<H> BlockController for BlockDevice<H>
where
    H: SdioIrqHost + Send + 'static,
    H::TransactionRequest<'static>: Send,
    H::BusRequest: Send,
{
    fn device_info(&self) -> DeviceInfo {
        let mut config = self.config;
        config.set_capacity_blocks(self.init_status.capacity_blocks());
        device_info(&config)
    }

    fn max_io_queues(&self) -> usize {
        1
    }

    fn advance(&mut self, event: ControllerEvent) -> Result<ControllerUpdate, BlkError> {
        match event {
            ControllerEvent::Start { target_queues } if target_queues != 0 => self.start(),
            ControllerEvent::OnlineSmp { .. }
            | ControllerEvent::RegisterRetry
            | ControllerEvent::Rearm { .. }
            | ControllerEvent::QuiesceIrqs
                if self.started && !self.stopped =>
            {
                self.current_update()
            }
            ControllerEvent::Irq(event)
                if self.started && !self.stopped && event.source_id() == 0 && event.bits() != 0 =>
            {
                self.current_update()
            }
            ControllerEvent::QuiesceIrqs if self.stopped => {
                Ok(ControllerUpdate::state(ControllerState::Shutdown))
            }
            ControllerEvent::Watchdog { .. } => {
                self.stopped = true;
                Ok(ControllerUpdate::state(ControllerState::Shutdown))
            }
            ControllerEvent::Shutdown => {
                self.stopped = true;
                Ok(ControllerUpdate::state(ControllerState::Shutdown))
            }
            _ => Err(BlkError::InvalidRequest),
        }
    }
}
