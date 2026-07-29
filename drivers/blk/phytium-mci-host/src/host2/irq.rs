use super::*;

/// Owned Phytium MCI IRQ top-half endpoint.
pub struct PhytiumMciIrqHandle {
    pub(crate) irq: Arc<host::IrqCore>,
}

impl SdioIrqHost for PhytiumMci {
    type Event = Event;
    type IrqHandle = PhytiumMciIrqHandle;

    fn irq_handle(&mut self) -> Self::IrqHandle {
        PhytiumMci::irq_endpoint(self)
    }

    fn completion_irq_enabled(&self) -> bool {
        PhytiumMci::completion_irq_enabled(self)
    }

    fn enable_completion_irq(&mut self) -> Result<(), Error> {
        PhytiumMci::enable_completion_irq(self);
        Ok(())
    }

    fn disable_completion_irq(&mut self) -> Result<(), Error> {
        PhytiumMci::disable_completion_irq(self);
        Ok(())
    }

    fn device_dma(&self) -> Result<&dma_api::DeviceDma, Error> {
        self.dma.as_ref().ok_or(Error::UnsupportedCommand)
    }

    fn progress_wait_kind(&self) -> sdmmc_protocol::sdio::HostProgressWait {
        if self.command_needs_register_retry() {
            sdmmc_protocol::sdio::HostProgressWait::Register {
                retry_after: PHYTIUM_REGISTER_RETRY_DELAY,
            }
        } else {
            sdmmc_protocol::sdio::HostProgressWait::Irq
        }
    }
}
