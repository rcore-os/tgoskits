use alloc::{boxed::Box, string::String, vec::Vec};
use core::ptr::NonNull;

use irq_framework::IrqId;
use rdif_display::{
    DisplayError as RdifDisplayError, Gpu3dErrorKind as RdifGpu3dErrorKind, Interface,
};

use crate::{
    CapsetInfo, DisplayDevice, DisplayError, DisplayInfo, Gpu3dErrorKind, PixelFormat, TransferBox,
};

pub struct RdifDisplayDevice {
    name: String,
    device: Box<dyn Interface>,
    fb_base_vaddr: NonNull<u8>,
    irq: Option<IrqId>,
}

unsafe impl Send for RdifDisplayDevice {}

impl RdifDisplayDevice {
    pub fn new(device: Box<dyn Interface>) -> Result<Self, DisplayError> {
        Self::new_with_irq(device, None)
    }

    pub fn new_with_irq(
        mut device: Box<dyn Interface>,
        irq: Option<IrqId>,
    ) -> Result<Self, DisplayError> {
        let name = device.name().into();
        let fb_base_vaddr = {
            let mut framebuffer = device.framebuffer().map_err(map_display_error)?;
            NonNull::new(framebuffer.as_mut_slice().as_mut_ptr())
                .ok_or(DisplayError::InvalidFramebuffer)?
        };
        Ok(Self {
            name,
            device,
            fb_base_vaddr,
            irq,
        })
    }

    pub fn from_interface(device: impl Interface + 'static) -> Result<Self, DisplayError> {
        Self::new(Box::new(device))
    }
}

impl DisplayDevice for RdifDisplayDevice {
    fn name(&self) -> &str {
        &self.name
    }

    fn info(&self) -> DisplayInfo {
        let info = self.device.info();
        DisplayInfo {
            width: info.width,
            height: info.height,
            fb_base_vaddr: self.fb_base_vaddr.as_ptr() as usize,
            fb_size: info.fb_size,
            stride: info.stride,
            format: info.format.into(),
        }
    }

    fn flush(&mut self) -> Result<(), DisplayError> {
        if self.device.need_flush() {
            self.device.flush().map_err(map_display_error)?;
        }
        Ok(())
    }

    fn irq_id(&self) -> Option<IrqId> {
        self.irq
    }

    fn enable_irq(&mut self) {
        self.device.enable_irq();
    }

    fn disable_irq(&mut self) {
        self.device.disable_irq();
    }

    fn is_irq_enabled(&self) -> bool {
        self.device.is_irq_enabled()
    }

    fn handle_irq(&mut self) -> bool {
        self.device.handle_irq().handled
    }

    // --- 2D resource / scanout forwarding ---

    fn resource_create_2d(
        &mut self,
        resource_id: u32,
        width: u32,
        height: u32,
    ) -> crate::DisplayResult {
        self.device
            .resource_create_2d(resource_id, width, height)
            .map_err(map_display_error)
    }

    fn resource_attach_backing(
        &mut self,
        resource_id: u32,
        paddr: u64,
        length: u32,
    ) -> crate::DisplayResult {
        self.device
            .resource_attach_backing(resource_id, paddr, length)
            .map_err(map_display_error)
    }

    fn set_scanout(
        &mut self,
        scanout_id: u32,
        resource_id: u32,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
    ) -> crate::DisplayResult {
        self.device
            .set_scanout(scanout_id, resource_id, x, y, w, h)
            .map_err(map_display_error)
    }

    fn transfer_to_host_2d(
        &mut self,
        resource_id: u32,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
    ) -> crate::DisplayResult {
        self.device
            .transfer_to_host_2d(resource_id, x, y, w, h)
            .map_err(map_display_error)
    }

    fn resource_flush(
        &mut self,
        resource_id: u32,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
    ) -> crate::DisplayResult {
        self.device
            .resource_flush(resource_id, x, y, w, h)
            .map_err(map_display_error)
    }

    // --- 3D forwarding ---

    fn has_virgl(&self) -> bool {
        self.device.has_virgl()
    }

    fn has_resource_blob(&self) -> bool {
        self.device.has_resource_blob()
    }

    fn ctx_create(&mut self, ctx_id: u32, name: &str, context_init: u32) -> crate::DisplayResult {
        self.device
            .ctx_create(ctx_id, name, context_init)
            .map_err(map_display_error)
    }

    fn ctx_destroy(&mut self, ctx_id: u32) -> crate::DisplayResult {
        self.device.ctx_destroy(ctx_id).map_err(map_display_error)
    }

    fn ctx_attach_resource(&mut self, ctx_id: u32, resource_id: u32) -> crate::DisplayResult {
        self.device
            .ctx_attach_resource(ctx_id, resource_id)
            .map_err(map_display_error)
    }

    fn ctx_detach_resource(&mut self, ctx_id: u32, resource_id: u32) -> crate::DisplayResult {
        self.device
            .ctx_detach_resource(ctx_id, resource_id)
            .map_err(map_display_error)
    }

    fn resource_create_3d(
        &mut self,
        ctx_id: u32,
        resource_id: u32,
        target: u32,
        format: u32,
        bind: u32,
        width: u32,
        height: u32,
        depth: u32,
        array_size: u32,
        last_level: u32,
        nr_samples: u32,
        flags: u32,
    ) -> crate::DisplayResult {
        self.device
            .resource_create_3d(
                ctx_id,
                resource_id,
                target,
                format,
                bind,
                width,
                height,
                depth,
                array_size,
                last_level,
                nr_samples,
                flags,
            )
            .map_err(map_display_error)
    }

    fn resource_unref(&mut self, resource_id: u32) -> crate::DisplayResult {
        self.device
            .resource_unref(resource_id)
            .map_err(map_display_error)
    }

    fn resource_create_blob(
        &mut self,
        ctx_id: u32,
        resource_id: u32,
        blob_mem: u32,
        blob_flags: u32,
        size: u64,
        blob_id: u64,
        cmd: &[u8],
    ) -> crate::DisplayResult {
        self.device
            .resource_create_blob(ctx_id, resource_id, blob_mem, blob_flags, size, blob_id, cmd)
            .map_err(map_display_error)
    }

    fn transfer_to_host_3d(
        &mut self,
        ctx_id: u32,
        resource_id: u32,
        box_: TransferBox,
        offset: u64,
        level: u32,
        stride: u32,
        layer_stride: u32,
    ) -> crate::DisplayResult {
        self.device
            .transfer_to_host_3d(
                ctx_id,
                resource_id,
                box_.into(),
                offset,
                level,
                stride,
                layer_stride,
            )
            .map_err(map_display_error)
    }

    fn transfer_from_host_3d(
        &mut self,
        ctx_id: u32,
        resource_id: u32,
        box_: TransferBox,
        offset: u64,
        level: u32,
        stride: u32,
        layer_stride: u32,
    ) -> crate::DisplayResult {
        self.device
            .transfer_from_host_3d(
                ctx_id,
                resource_id,
                box_.into(),
                offset,
                level,
                stride,
                layer_stride,
            )
            .map_err(map_display_error)
    }

    fn submit_cmd(&mut self, ctx_id: u32, cmds: &[u8]) -> Result<u64, DisplayError> {
        self.device
            .submit_cmd(ctx_id, cmds)
            .map_err(map_display_error)
    }

    fn capset_info(&mut self, index: u32) -> Result<CapsetInfo, DisplayError> {
        self.device
            .get_capset_info(index)
            .map(|info| info.into())
            .map_err(map_display_error)
    }

    fn capset(&mut self, id: u32, ver: u32, size: u32) -> Result<Vec<u8>, DisplayError> {
        self.device
            .get_capset(id, ver, size)
            .map_err(map_display_error)
    }
}

impl From<rdif_display::PixelFormat> for PixelFormat {
    fn from(value: rdif_display::PixelFormat) -> Self {
        match value {
            rdif_display::PixelFormat::Rgb565 => Self::Rgb565,
            rdif_display::PixelFormat::Rgb888 => Self::Rgb888,
            rdif_display::PixelFormat::Xrgb8888 => Self::Xrgb8888,
            rdif_display::PixelFormat::Argb8888 => Self::Argb8888,
            rdif_display::PixelFormat::Bgr888 => Self::Bgr888,
            rdif_display::PixelFormat::Xbgr8888 => Self::Xbgr8888,
        }
    }
}

fn map_display_error(error: RdifDisplayError) -> DisplayError {
    match error {
        RdifDisplayError::NotSupported => DisplayError::NotSupported,
        RdifDisplayError::NotAvailable => DisplayError::NotAvailable,
        RdifDisplayError::InvalidFramebuffer => DisplayError::InvalidFramebuffer,
        RdifDisplayError::Gpu3dError(kind) => {
            let mapped = match kind {
                RdifGpu3dErrorKind::IoError => Gpu3dErrorKind::IoError,
                RdifGpu3dErrorKind::Unsupported => Gpu3dErrorKind::Unsupported,
                RdifGpu3dErrorKind::NotReady => Gpu3dErrorKind::NotReady,
                RdifGpu3dErrorKind::InvalidParam => Gpu3dErrorKind::InvalidParam,
                RdifGpu3dErrorKind::Other => Gpu3dErrorKind::Other,
            };
            DisplayError::Gpu3dError(mapped)
        }
        RdifDisplayError::Other(err) => {
            log::warn!("[axdisplay] rdif error collapsed to BadState: {err}");
            DisplayError::BadState
        }
    }
}

impl From<TransferBox> for rdif_display::TransferBox {
    fn from(b: TransferBox) -> Self {
        Self {
            x: b.x,
            y: b.y,
            z: b.z,
            w: b.w,
            h: b.h,
            d: b.d,
        }
    }
}

impl From<rdif_display::CapsetInfo> for CapsetInfo {
    fn from(c: rdif_display::CapsetInfo) -> Self {
        Self {
            capset_id: c.capset_id,
            max_version: c.max_version,
            max_size: c.max_size,
        }
    }
}

#[cfg(test)]
mod tests {
    use irq_framework::{HwIrq, IrqDomainId, IrqId};
    use rdif_display::{DisplayInfo, DriverGeneric, FrameBuffer, PixelFormat};

    use super::*;

    struct TestDisplay {
        fb: [u8; 16],
    }

    impl DriverGeneric for TestDisplay {
        fn name(&self) -> &str {
            "test-display"
        }
    }

    impl Interface for TestDisplay {
        fn info(&self) -> DisplayInfo {
            DisplayInfo {
                width: 2,
                height: 2,
                stride: 8,
                format: PixelFormat::Xrgb8888,
                fb_size: self.fb.len(),
            }
        }

        fn framebuffer(&mut self) -> Result<FrameBuffer<'_>, rdif_display::DisplayError> {
            Ok(FrameBuffer::from_slice(&mut self.fb))
        }
    }

    #[test]
    fn rdif_display_device_exposes_resolved_irq_id() {
        let irq = IrqId::new(IrqDomainId(7), HwIrq(42));
        let device =
            RdifDisplayDevice::new_with_irq(Box::new(TestDisplay { fb: [0; 16] }), Some(irq))
                .unwrap();
        let erased = crate::ErasedDisplayDevice::new(device);

        assert_eq!(erased.irq_id(), Some(irq));
    }

    #[test]
    fn rdif_display_device_without_resolved_irq_has_no_irq_id() {
        let device = RdifDisplayDevice::new(Box::new(TestDisplay { fb: [0; 16] })).unwrap();
        let erased = crate::ErasedDisplayDevice::new(device);

        assert_eq!(erased.irq_id(), None);
    }
}
