use alloc::{boxed::Box, string::String, vec::Vec};

use irq_framework::IrqId;

use crate::{CapsetInfo, DisplayInfo, TransferBox};

pub type DisplayResult<T = ()> = Result<T, DisplayError>;

/// 3D GPU error kinds mirrored from the driver layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gpu3dErrorKind {
    IoError,
    Unsupported,
    NotReady,
    InvalidParam,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayError {
    NotSupported,
    NotAvailable,
    InvalidFramebuffer,
    BadState,
    Gpu3dError(Gpu3dErrorKind),
}

/// Domain boundary consumed by graphics modules and device files.
pub trait DisplayDevice: Send {
    // --- 2D ---

    fn name(&self) -> &str;

    fn info(&self) -> DisplayInfo;

    fn flush(&mut self) -> DisplayResult;

    fn irq_id(&self) -> Option<IrqId> {
        None
    }

    fn enable_irq(&mut self) {}

    fn disable_irq(&mut self) {}

    fn is_irq_enabled(&self) -> bool {
        false
    }

    fn handle_irq(&mut self) -> bool {
        false
    }

    // --- 2D resource / scanout primitives (default: unsupported) ---

    fn resource_create_2d(
        &mut self,
        _resource_id: u32,
        _width: u32,
        _height: u32,
    ) -> DisplayResult {
        Err(DisplayError::NotSupported)
    }

    fn resource_attach_backing(
        &mut self,
        _resource_id: u32,
        _paddr: u64,
        _length: u32,
    ) -> DisplayResult {
        Err(DisplayError::NotSupported)
    }

    fn set_scanout(
        &mut self,
        _scanout_id: u32,
        _resource_id: u32,
        _x: u32,
        _y: u32,
        _w: u32,
        _h: u32,
    ) -> DisplayResult {
        Err(DisplayError::NotSupported)
    }

    fn transfer_to_host_2d(
        &mut self,
        _resource_id: u32,
        _x: u32,
        _y: u32,
        _w: u32,
        _h: u32,
    ) -> DisplayResult {
        Err(DisplayError::NotSupported)
    }

    fn resource_flush(
        &mut self,
        _resource_id: u32,
        _x: u32,
        _y: u32,
        _w: u32,
        _h: u32,
    ) -> DisplayResult {
        Err(DisplayError::NotSupported)
    }

    // --- 3D (default: unsupported) ---

    fn has_virgl(&self) -> bool {
        false
    }

    fn has_resource_blob(&self) -> bool {
        false
    }

    fn ctx_create(&mut self, _ctx_id: u32, _name: &str, _context_init: u32) -> DisplayResult {
        Err(DisplayError::NotSupported)
    }

    fn ctx_destroy(&mut self, _ctx_id: u32) -> DisplayResult {
        Err(DisplayError::NotSupported)
    }

    fn ctx_attach_resource(&mut self, _ctx_id: u32, _resource_id: u32) -> DisplayResult {
        Err(DisplayError::NotSupported)
    }

    fn ctx_detach_resource(&mut self, _ctx_id: u32, _resource_id: u32) -> DisplayResult {
        Err(DisplayError::NotSupported)
    }

    #[allow(clippy::too_many_arguments)]
    fn resource_create_3d(
        &mut self,
        _ctx_id: u32,
        _resource_id: u32,
        _target: u32,
        _format: u32,
        _bind: u32,
        _width: u32,
        _height: u32,
        _depth: u32,
        _array_size: u32,
        _last_level: u32,
        _nr_samples: u32,
        _flags: u32,
    ) -> DisplayResult {
        Err(DisplayError::NotSupported)
    }

    #[allow(clippy::too_many_arguments)]
    fn resource_create_blob(
        &mut self,
        _ctx_id: u32,
        _resource_id: u32,
        _blob_mem: u32,
        _blob_flags: u32,
        _size: u64,
        _blob_id: u64,
        _cmd: &[u8],
    ) -> DisplayResult {
        Err(DisplayError::NotSupported)
    }

    fn resource_unref(&mut self, _resource_id: u32) -> DisplayResult {
        Err(DisplayError::NotSupported)
    }

    #[allow(clippy::too_many_arguments)]
    fn transfer_to_host_3d(
        &mut self,
        _ctx_id: u32,
        _resource_id: u32,
        _box_: TransferBox,
        _offset: u64,
        _level: u32,
        _stride: u32,
        _layer_stride: u32,
    ) -> DisplayResult {
        Err(DisplayError::NotSupported)
    }

    #[allow(clippy::too_many_arguments)]
    fn transfer_from_host_3d(
        &mut self,
        _ctx_id: u32,
        _resource_id: u32,
        _box_: TransferBox,
        _offset: u64,
        _level: u32,
        _stride: u32,
        _layer_stride: u32,
    ) -> DisplayResult {
        Err(DisplayError::NotSupported)
    }

    fn submit_cmd(&mut self, _ctx_id: u32, _cmds: &[u8]) -> Result<u64, DisplayError> {
        Err(DisplayError::NotSupported)
    }

    fn capset_info(&mut self, _index: u32) -> Result<CapsetInfo, DisplayError> {
        Err(DisplayError::NotSupported)
    }

    fn capset(&mut self, _id: u32, _ver: u32, _size: u32) -> Result<Vec<u8>, DisplayError> {
        Err(DisplayError::NotSupported)
    }
}

pub struct ErasedDisplayDevice {
    name: String,
    inner: Box<dyn DisplayDevice>,
}

impl ErasedDisplayDevice {
    pub fn new(device: impl DisplayDevice + 'static) -> Self {
        let name = device.name().into();
        Self {
            name,
            inner: Box::new(device),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl DisplayDevice for ErasedDisplayDevice {
    fn name(&self) -> &str {
        &self.name
    }

    fn info(&self) -> DisplayInfo {
        self.inner.info()
    }

    fn flush(&mut self) -> DisplayResult {
        self.inner.flush()
    }

    fn irq_id(&self) -> Option<IrqId> {
        self.inner.irq_id()
    }

    fn enable_irq(&mut self) {
        self.inner.enable_irq();
    }

    fn disable_irq(&mut self) {
        self.inner.disable_irq();
    }

    fn is_irq_enabled(&self) -> bool {
        self.inner.is_irq_enabled()
    }

    fn handle_irq(&mut self) -> bool {
        self.inner.handle_irq()
    }

    fn set_scanout(
        &mut self,
        scanout_id: u32,
        resource_id: u32,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
    ) -> DisplayResult {
        self.inner.set_scanout(scanout_id, resource_id, x, y, w, h)
    }

    fn resource_create_2d(
        &mut self,
        resource_id: u32,
        width: u32,
        height: u32,
    ) -> DisplayResult {
        self.inner.resource_create_2d(resource_id, width, height)
    }

    fn resource_attach_backing(
        &mut self,
        resource_id: u32,
        paddr: u64,
        length: u32,
    ) -> DisplayResult {
        self.inner.resource_attach_backing(resource_id, paddr, length)
    }

    fn transfer_to_host_2d(
        &mut self,
        resource_id: u32,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
    ) -> DisplayResult {
        self.inner.transfer_to_host_2d(resource_id, x, y, w, h)
    }

    fn resource_flush(
        &mut self,
        resource_id: u32,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
    ) -> DisplayResult {
        self.inner.resource_flush(resource_id, x, y, w, h)
    }

    fn has_virgl(&self) -> bool {
        self.inner.has_virgl()
    }

    fn has_resource_blob(&self) -> bool {
        self.inner.has_resource_blob()
    }

    fn ctx_create(&mut self, ctx_id: u32, name: &str, context_init: u32) -> DisplayResult {
        self.inner.ctx_create(ctx_id, name, context_init)
    }

    fn ctx_destroy(&mut self, ctx_id: u32) -> DisplayResult {
        self.inner.ctx_destroy(ctx_id)
    }

    fn ctx_attach_resource(&mut self, ctx_id: u32, resource_id: u32) -> DisplayResult {
        self.inner.ctx_attach_resource(ctx_id, resource_id)
    }

    fn ctx_detach_resource(&mut self, ctx_id: u32, resource_id: u32) -> DisplayResult {
        self.inner.ctx_detach_resource(ctx_id, resource_id)
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
    ) -> DisplayResult {
        self.inner.resource_create_3d(
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
    }

    fn resource_unref(&mut self, resource_id: u32) -> DisplayResult {
        self.inner.resource_unref(resource_id)
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
    ) -> DisplayResult {
        self.inner.resource_create_blob(ctx_id, resource_id, blob_mem, blob_flags, size, blob_id, cmd)
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
    ) -> DisplayResult {
        self.inner.transfer_to_host_3d(
            ctx_id,
            resource_id,
            box_,
            offset,
            level,
            stride,
            layer_stride,
        )
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
    ) -> DisplayResult {
        self.inner.transfer_from_host_3d(
            ctx_id,
            resource_id,
            box_,
            offset,
            level,
            stride,
            layer_stride,
        )
    }

    fn submit_cmd(&mut self, ctx_id: u32, cmds: &[u8]) -> Result<u64, DisplayError> {
        self.inner.submit_cmd(ctx_id, cmds)
    }

    fn capset_info(&mut self, index: u32) -> Result<CapsetInfo, DisplayError> {
        self.inner.capset_info(index)
    }

    fn capset(&mut self, id: u32, ver: u32, size: u32) -> Result<Vec<u8>, DisplayError> {
        self.inner.capset(id, ver, size)
    }
}
