use alloc::{boxed::Box, string::String, vec::Vec};
use core::ptr::NonNull;

use irq_framework::IrqId;
use rdif_display::{
    DisplayError as RdifDisplayError, Gpu3dErrorKind as RdifGpu3dErrorKind, Interface,
};

use crate::{
    BlobMemory, CapsetInfo, DisplayDevice, DisplayError, DisplayInfo, Gpu3dErrorKind, PixelFormat,
    ResourceCreate3d, ResourceCreateBlob, Transfer3d, TransferBox,
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

    fn restore_framebuffer_scanout(&mut self) -> Result<(), DisplayError> {
        self.device
            .restore_framebuffer_scanout()
            .map_err(map_display_error)
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

    fn has_context_init(&self) -> bool {
        self.device.has_context_init()
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

    fn resource_create_3d(&mut self, params: ResourceCreate3d) -> crate::DisplayResult {
        self.device
            .resource_create_3d(params.into())
            .map_err(map_display_error)
    }

    fn resource_unref(&mut self, resource_id: u32) -> crate::DisplayResult {
        self.device
            .resource_unref(resource_id)
            .map_err(map_display_error)
    }

    fn resource_create_blob(&mut self, params: ResourceCreateBlob<'_>) -> crate::DisplayResult {
        self.device
            .resource_create_blob(params.into())
            .map_err(map_display_error)
    }

    fn transfer_to_host_3d(&mut self, params: Transfer3d) -> crate::DisplayResult {
        self.device
            .transfer_to_host_3d(params.into())
            .map_err(map_display_error)
    }

    fn transfer_from_host_3d(&mut self, params: Transfer3d) -> crate::DisplayResult {
        self.device
            .transfer_from_host_3d(params.into())
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

impl From<BlobMemory> for rdif_display::BlobMemory {
    fn from(memory: BlobMemory) -> Self {
        Self {
            paddr: memory.paddr,
            length: memory.length,
        }
    }
}

impl From<ResourceCreate3d> for rdif_display::ResourceCreate3d {
    fn from(value: ResourceCreate3d) -> Self {
        Self {
            ctx_id: value.ctx_id,
            resource_id: value.resource_id,
            target: value.target,
            format: value.format,
            bind: value.bind,
            width: value.width,
            height: value.height,
            depth: value.depth,
            array_size: value.array_size,
            last_level: value.last_level,
            nr_samples: value.nr_samples,
            flags: value.flags,
        }
    }
}

impl<'a> From<ResourceCreateBlob<'a>> for rdif_display::ResourceCreateBlob<'a> {
    fn from(value: ResourceCreateBlob<'a>) -> Self {
        Self {
            ctx_id: value.ctx_id,
            resource_id: value.resource_id,
            blob_mem: value.blob_mem,
            blob_flags: value.blob_flags,
            size: value.size,
            blob_id: value.blob_id,
            backing: value.backing.map(Into::into),
            cmd: value.cmd,
        }
    }
}

impl From<Transfer3d> for rdif_display::Transfer3d {
    fn from(value: Transfer3d) -> Self {
        Self {
            ctx_id: value.ctx_id,
            resource_id: value.resource_id,
            box_: value.box_.into(),
            offset: value.offset,
            level: value.level,
            stride: value.stride,
            layer_stride: value.layer_stride,
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
