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

    fn port_offset(&self, access: &BusAccess) -> Result<(PortWindow, usize), DeviceError> {
        if access.kind != BusKind::Port || access.addr > u64::from(u16::MAX) {
            return Err(DeviceError::OutOfRange { addr: access.addr });
        }
        let port = access.addr as u16;
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
        Err(DeviceError::OutOfRange { addr: access.addr })
    }

    fn write_dma(
        &self,
        offset: usize,
        access: &BusAccess,
        context: &mut dyn DeviceAccess,
    ) -> Result<BusResponse, DeviceError> {
        let Some(descriptor) = self
            .inner
            .write_dma_port(offset, access.width, access.data as usize)
            .map_err(DeviceError::from)?
        else {
            return Ok(BusResponse::Write);
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
        Ok(BusResponse::Write)
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

    fn access(
        &self,
        access: &BusAccess,
        context: &mut dyn DeviceAccess,
    ) -> Result<BusResponse, DeviceError> {
        let (window, offset) = self.port_offset(access)?;
        match (window, access.is_read, offset) {
            (PortWindow::SelectorData, true, 0) => Ok(BusResponse::Read {
                value: u64::from(self.inner.read_selector()),
            }),
            (PortWindow::SelectorData, true, 1) => Ok(BusResponse::Read {
                value: self.inner.read_data(access.width) as u64,
            }),
            (PortWindow::SelectorData, false, 0) => {
                self.inner.select(access.data as u16);
                Ok(BusResponse::Write)
            }
            (PortWindow::SelectorData, false, 1) => Ok(BusResponse::Write),
            (PortWindow::Dma, true, _) => Ok(BusResponse::Read { value: 0 }),
            (PortWindow::Dma, false, offset) => self.write_dma(offset, access, context),
            _ => Err(DeviceError::OutOfRange { addr: access.addr }),
        }
    }
}
