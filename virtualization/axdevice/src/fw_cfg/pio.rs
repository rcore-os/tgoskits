//! x86 PIO transport for the architecture-neutral fw_cfg selector state.

use alloc::{boxed::Box, sync::Arc};
use core::cell::RefCell;

use axdevice_base::*;

use super::FwCfg;
use crate::{DeviceManagerError, DeviceManagerResult};

/// QEMU-compatible x86 fw_cfg selector/data and DMA port windows.
pub struct FwCfgPioDevice {
    inner: Arc<FwCfg>,
    selector_base: u16,
    selector_size: u16,
    dma_base: u16,
    dma_size: u16,
    dma_grant: DmaGrant,
    resources: Box<[Resource]>,
}

impl FwCfgPioDevice {
    pub(crate) fn new(
        inner: Arc<FwCfg>,
        selector_base: u16,
        selector_size: u16,
        dma_base: u16,
        dma_size: u16,
        dma_grant: DmaGrant,
    ) -> DeviceManagerResult<Self> {
        if selector_size != 2 || dma_size != 8 {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "build x86 PIO fw_cfg device",
                detail: "selector/data and DMA windows must be 2 and 8 bytes".into(),
            });
        }
        Ok(Self {
            inner,
            selector_base,
            selector_size,
            dma_base,
            dma_size,
            dma_grant,
            resources: alloc::vec![
                Resource::PortRange {
                    base: selector_base,
                    size: selector_size,
                },
                Resource::PortRange {
                    base: dma_base,
                    size: dma_size,
                },
            ]
            .into_boxed_slice(),
        })
    }

    fn port_offset(&self, access: &DeviceAccess) -> Result<(PortWindow, usize), DeviceError> {
        if access.bus() != BusKind::Port || access.address() > u64::from(u16::MAX) {
            return Err(DeviceError::OutOfRange {
                addr: access.address(),
            });
        }
        let port = access.address() as u16;
        if let Some(offset) = port.checked_sub(self.selector_base)
            && offset < self.selector_size
        {
            return Ok((PortWindow::SelectorData, usize::from(offset)));
        }
        if let Some(offset) = port.checked_sub(self.dma_base)
            && offset < self.dma_size
        {
            return Ok((PortWindow::Dma, usize::from(offset)));
        }
        Err(DeviceError::OutOfRange {
            addr: access.address(),
        })
    }

    fn write_dma(
        &self,
        offset: usize,
        access: &DeviceAccess,
        value: u64,
        context: &mut dyn DeviceContext,
    ) -> DeviceResult {
        let Some(descriptor) = self
            .inner
            .write_dma_port(offset, access.width(), value as usize)
            .map_err(DeviceError::from)?
        else {
            return Ok(());
        };
        let context = RefCell::new(context);
        self.inner
            .process_dma(
                descriptor,
                |gpa, data| {
                    context
                        .borrow_mut()
                        .read_guest_memory(&self.dma_grant, gpa, data)
                        .map_err(DeviceManagerError::from)
                },
                |gpa, data| {
                    context
                        .borrow_mut()
                        .write_guest_memory(&self.dma_grant, gpa, data)
                        .map_err(DeviceManagerError::from)
                },
            )
            .map_err(DeviceError::from)?;
        Ok(())
    }
}

enum PortWindow {
    SelectorData,
    Dma,
}

impl Device for FwCfgPioDevice {
    fn name(&self) -> &str {
        "fw-cfg-pio"
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn read(&self, access: &DeviceAccess, _context: &mut dyn DeviceContext) -> DeviceResult<u64> {
        let (window, offset) = self.port_offset(access)?;
        match (window, offset) {
            (PortWindow::SelectorData, 0) => Ok(u64::from(self.inner.read_selector())),
            (PortWindow::SelectorData, 1) => Ok(self.inner.read_data(access.width()) as u64),
            (PortWindow::Dma, _) => Ok(0),
            _ => Err(DeviceError::OutOfRange {
                addr: access.address(),
            }),
        }
    }

    fn write(
        &self,
        access: &DeviceAccess,
        value: u64,
        context: &mut dyn DeviceContext,
    ) -> DeviceResult {
        let (window, offset) = self.port_offset(access)?;
        match (window, offset) {
            (PortWindow::SelectorData, 0) => {
                self.inner.select(value as u16);
                Ok(())
            }
            (PortWindow::SelectorData, 1) => Ok(()),
            (PortWindow::Dma, offset) => self.write_dma(offset, access, value, context),
            _ => Err(DeviceError::OutOfRange {
                addr: access.address(),
            }),
        }
    }
}
