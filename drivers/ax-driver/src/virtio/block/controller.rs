//! RDIF controller facade for staged initialization and IRQ-only operation.

use alloc::{boxed::Box, sync::Arc, vec};

use ax_kspin::PreemptIrqGuard;
use rdif_block::{BlkError, IdList, QueueHandle};
use rdrive::DriverGeneric;

use super::{
    VIRTIO_BLK_IRQ_SOURCE_ID,
    device::VirtIoBlkDevice,
    irq::{VirtioInterruptPort, VirtioIrqOwnership},
    lifecycle::VirtioBlockLifecycle,
    queue::{BlockQueue, virtio_queue_ids, virtio_queue_info},
};
use crate::virtio::VirtIoTransport;

const IRQ_ENDPOINT_NOT_BOUND: BlkError = BlkError::Other("virtio block IRQ endpoint is not bound");

pub(super) struct BlockDevice<T: VirtIoTransport> {
    dev: Arc<VirtIoBlkDevice<T>>,
    queue_created: bool,
    irq: VirtioIrqOwnership,
    lifecycle: VirtioBlockLifecycle,
}

impl<T: VirtIoTransport> BlockDevice<T> {
    pub(super) fn discovered(
        dev: Arc<VirtIoBlkDevice<T>>,
        interrupt_port: VirtioInterruptPort,
    ) -> Self {
        Self {
            dev,
            queue_created: false,
            irq: VirtioIrqOwnership::new(interrupt_port),
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
        Some(QueueHandle::new(Box::new(BlockQueue::new(
            Arc::clone(&self.dev),
            self.irq.register_mapping_lease(),
        ))))
    }

    fn enable_irq(&self) -> Result<(), BlkError> {
        if !self.lifecycle.can_run() {
            return Err(BlkError::Offline);
        }
        let endpoint_live = if self.dev.is_ready() {
            self.irq.normal_io_is_live()
        } else {
            self.irq.initialization_is_live()
        };
        if !endpoint_live {
            return Err(IRQ_ENDPOINT_NOT_BOUND);
        }
        let _context = PreemptIrqGuard::new();
        self.irq.enable();
        self.dev.enable_irq();
        Ok(())
    }

    fn disable_irq(&self) -> Result<(), BlkError> {
        let _context = PreemptIrqGuard::new();
        self.dev.disable_irq();
        self.irq.disable();
        Ok(())
    }

    fn is_irq_enabled(&self) -> bool {
        self.lifecycle.can_run() && self.dev.is_irq_enabled() && self.irq.is_enabled()
    }

    fn irq_sources(&self) -> rdif_block::IrqSourceList {
        vec![rdif_block::IrqSourceInfo::new(
            VIRTIO_BLK_IRQ_SOURCE_ID,
            virtio_queue_ids(),
        )]
    }

    fn take_irq_source(&mut self, source_id: usize) -> Option<rdif_block::BlockIrqSource> {
        if source_id != VIRTIO_BLK_IRQ_SOURCE_ID {
            return None;
        }
        self.irq.take_normal_io_source()
    }
}

impl<T: VirtIoTransport> rdif_block::InitialController for BlockDevice<T> {
    fn irq_sources(&self) -> IdList {
        let mut sources = IdList::none();
        sources.insert(VIRTIO_BLK_IRQ_SOURCE_ID);
        sources
    }

    fn take_irq_source(&mut self, source_id: usize) -> Option<rdif_block::BlockIrqSource> {
        if source_id != VIRTIO_BLK_IRQ_SOURCE_ID {
            return None;
        }
        self.irq.take_initialization_source()
    }

    fn poll_init(&mut self, input: rdif_block::InitInput) -> rdif_block::InitPoll<()> {
        if !self.irq.initialization_is_live() || !self.dev.is_irq_enabled() {
            return rdif_block::InitPoll::Failed(rdif_block::InitError::MissingInterrupt);
        }
        self.dev.poll_init(input)
    }
}

impl<T: VirtIoTransport> rdif_block::InterruptLifecycle for BlockDevice<T> {
    fn controller_cookie(&self) -> usize {
        Arc::as_ptr(&self.dev).expose_provenance()
    }

    fn begin_dma_quiesce(
        &mut self,
        epoch: rdif_block::ControllerEpoch,
        cause: rdif_block::RecoveryCause,
    ) -> Result<(), rdif_block::InitError> {
        self.irq.disable();
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
        if !self.irq.normal_io_is_live() {
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
