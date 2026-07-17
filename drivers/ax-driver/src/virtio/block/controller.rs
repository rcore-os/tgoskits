//! RDIF controller facade for staged initialization and IRQ-only operation.

use alloc::{boxed::Box, sync::Arc, vec};

use rdif_block::{BlkError, IdList, QueueHandle};
use rdrive::DriverGeneric;

use super::{
    VIRTIO_BLK_IRQ_SOURCE_ID,
    device::VirtIoBlkDevice,
    irq::{VirtioBlkInitIrqHandler, VirtioBlkIrqHandler},
    lifecycle::VirtioBlockLifecycle,
    queue::{BlockQueue, virtio_queue_ids, virtio_queue_info},
};
use crate::virtio::VirtIoTransport;

const IRQ_ENDPOINT_NOT_BOUND: BlkError = BlkError::Other("virtio block IRQ endpoint is not bound");

pub(super) struct BlockDevice<T: VirtIoTransport> {
    dev: Arc<VirtIoBlkDevice<T>>,
    queue_created: bool,
    irq_handler_taken: bool,
    init_irq_handler_taken: bool,
    lifecycle: VirtioBlockLifecycle,
}

impl<T: VirtIoTransport> BlockDevice<T> {
    pub(super) fn discovered(dev: Arc<VirtIoBlkDevice<T>>) -> Self {
        Self {
            dev,
            queue_created: false,
            irq_handler_taken: false,
            init_irq_handler_taken: false,
            lifecycle: VirtioBlockLifecycle::running(),
        }
    }
}

impl<T: VirtIoTransport> DriverGeneric for BlockDevice<T> {
    fn name(&self) -> &str {
        "virtio-blk"
    }
}

impl<T: VirtIoTransport> rdif_block::Interface for BlockDevice<T> {
    fn controller_init(&mut self) -> rdif_block::ControllerInitEndpoint<'_> {
        if self.dev.is_ready() {
            rdif_block::ControllerInitEndpoint::Ready
        } else {
            rdif_block::ControllerInitEndpoint::Pending(self)
        }
    }

    fn lifecycle(&mut self) -> rdif_block::LifecycleEndpoint<'_> {
        rdif_block::LifecycleEndpoint::Interrupt(self)
    }

    fn device_info(&self) -> rdif_block::DeviceInfo {
        let blocks = self.dev.capacity_if_ready().unwrap_or(0);
        let mut info = virtio_queue_info(blocks).device;
        info.read_only = self.dev.read_only_if_ready().unwrap_or(false);
        info
    }

    fn queue_limits(&self) -> rdif_block::QueueLimits {
        virtio_queue_info(0).limits
    }

    fn create_queue(&mut self) -> Option<QueueHandle> {
        if self.queue_created || !self.dev.is_ready() || !self.lifecycle.can_run() {
            return None;
        }
        self.queue_created = true;
        Some(QueueHandle::new(Box::new(BlockQueue::new(Arc::clone(
            &self.dev,
        )))))
    }

    fn enable_irq(&self) -> Result<(), BlkError> {
        if !self.lifecycle.can_run() {
            return Err(BlkError::Offline);
        }
        let endpoint_taken = if self.dev.is_ready() {
            self.irq_handler_taken
        } else {
            self.init_irq_handler_taken
        };
        if !endpoint_taken {
            return Err(IRQ_ENDPOINT_NOT_BOUND);
        }
        self.dev.enable_irq();
        Ok(())
    }

    fn disable_irq(&self) -> Result<(), BlkError> {
        self.dev.disable_irq();
        Ok(())
    }

    fn is_irq_enabled(&self) -> bool {
        self.lifecycle.can_run() && self.dev.is_irq_enabled()
    }

    fn irq_sources(&self) -> rdif_block::IrqSourceList {
        vec![rdif_block::IrqSourceInfo::new(
            VIRTIO_BLK_IRQ_SOURCE_ID,
            virtio_queue_ids(),
        )]
    }

    fn take_irq_handler(&mut self, source_id: usize) -> Option<rdif_block::BIrqHandler> {
        if source_id != VIRTIO_BLK_IRQ_SOURCE_ID || self.irq_handler_taken {
            return None;
        }
        self.irq_handler_taken = true;
        Some(Box::new(VirtioBlkIrqHandler {
            inner: Arc::clone(&self.dev),
        }) as _)
    }
}

impl<T: VirtIoTransport> rdif_block::InitialController for BlockDevice<T> {
    fn irq_sources(&self) -> IdList {
        let mut sources = IdList::none();
        sources.insert(VIRTIO_BLK_IRQ_SOURCE_ID);
        sources
    }

    fn take_irq_handler(&mut self, source_id: usize) -> Option<Box<dyn rdif_block::IrqHandler>> {
        if source_id != VIRTIO_BLK_IRQ_SOURCE_ID || self.init_irq_handler_taken {
            return None;
        }
        self.init_irq_handler_taken = true;
        Some(Box::new(VirtioBlkInitIrqHandler {
            inner: Arc::clone(&self.dev),
        }))
    }

    fn service_deferred_irq(&mut self, source_id: usize) -> rdif_block::InitIrqProgress {
        if source_id != VIRTIO_BLK_IRQ_SOURCE_ID {
            return rdif_block::InitIrqProgress::Unhandled;
        }
        self.dev.service_deferred_initialization_irq()
    }

    fn poll_init(&mut self, input: rdif_block::InitInput) -> rdif_block::InitPoll<()> {
        if !self.init_irq_handler_taken || !self.dev.is_irq_enabled() {
            return rdif_block::InitPoll::Failed(rdif_block::InitError::MissingInterrupt);
        }
        self.dev.poll_init(input)
    }
}

impl<T: VirtIoTransport> rdif_block::InterruptLifecycle for BlockDevice<T> {
    fn controller_cookie(&self) -> usize {
        Arc::as_ptr(&self.dev).expose_provenance()
    }

    fn service_deferred_irq(&mut self, source_id: usize) -> rdif_block::InitIrqProgress {
        if source_id != VIRTIO_BLK_IRQ_SOURCE_ID {
            return rdif_block::InitIrqProgress::Unhandled;
        }
        self.dev.service_deferred_initialization_irq()
    }

    fn begin_dma_quiesce(
        &mut self,
        epoch: rdif_block::ControllerEpoch,
        cause: rdif_block::RecoveryCause,
    ) -> Result<(), rdif_block::InitError> {
        self.lifecycle.begin_dma_quiesce(&*self.dev, epoch, cause)
    }

    fn poll_dma_quiesce(
        &mut self,
        input: rdif_block::InitInput,
    ) -> rdif_block::InitPoll<rdif_block::DmaQuiesced> {
        self.lifecycle.poll_dma_quiesce(&*self.dev, input)
    }

    fn enter_guest_owned(
        &mut self,
        quiesced: rdif_block::DmaQuiesced,
    ) -> Result<(), rdif_block::InitError> {
        self.lifecycle.enter_guest_owned(&*self.dev, quiesced)
    }

    fn begin_reinitialize(
        &mut self,
        quiesced: rdif_block::DmaQuiesced,
    ) -> Result<(), rdif_block::InitError> {
        if !self.irq_handler_taken {
            return Err(rdif_block::InitError::MissingInterrupt);
        }
        self.lifecycle.begin_reinitialize(&*self.dev, quiesced)
    }

    fn poll_reinitialize(
        &mut self,
        input: rdif_block::InitInput,
    ) -> rdif_block::InitPoll<rdif_block::ControllerReady> {
        self.lifecycle.poll_reinitialize(&*self.dev, input)
    }
}
