extern crate alloc;

use alloc::format;

use rdif_display::{
    CapsetInfo, DisplayError, DisplayInfo, Event, FrameBuffer, PixelFormat, TransferBox,
};
use rdrive::{DriverGeneric, PlatformDevice, probe::OnProbeError};
#[cfg(feature = "pci")]
use virtio_drivers::transport::DeviceType;
use virtio_drivers::{
    Error as VirtIoError,
    device::gpu::{GpuBox, Rect, VirtIOGpu},
    transport::{InterruptStatus, Transport},
};

use crate::{BindingInfo, display::PlatformDeviceDisplay, virtio::VirtIoHalImpl};
#[cfg(feature = "pci")]
use crate::{PciIrqRequirement, binding_info_from_pci};

#[cfg(feature = "pci")]
crate::model_register!(
    name: "VirtIO GPU",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci {
        on_probe: probe_pci,
    }],
);

#[cfg(feature = "pci")]
fn probe_pci(mut probe: rdrive::probe::pci::ProbePci<'_>) -> Result<(), OnProbeError> {
    // INTx stays unmasked at probe time: card0's completion IRQ handler
    // (`framebuffer_handle_irq` + `refresh_fence_waiters_from_irq`) pumps the
    // used ring and wakes fence pollers directly. Without it, fence signaling
    // falls back to the 1ms refresher poll (measured: offscreen texture
    // 703 -> 1141 FPS with the IRQ path, +62%).
    let transport = crate::pci::take_virtio_transport(probe.endpoint_mut(), DeviceType::GPU)?;
    let info = binding_info_from_pci(probe.info(), PciIrqRequirement::Optional)?;
    register_transport_with_info(probe.into_platform_device(), transport, info)
}

pub fn register_transport<T: Transport + 'static>(
    plat_dev: PlatformDevice,
    transport: T,
) -> Result<(), OnProbeError> {
    register_transport_with_info(plat_dev, transport, BindingInfo::empty())
}

pub fn register_transport_with_info<T: Transport + 'static>(
    plat_dev: PlatformDevice,
    transport: T,
    info: BindingInfo,
) -> Result<(), OnProbeError> {
    let irq_num = info.irq_num();
    let dev = VirtIoDisplay::new(transport, irq_num)
        .map_err(|err| OnProbeError::other(format!("failed to initialize virtio-gpu: {err:?}")))?;
    let irq = plat_dev.register_display_with_info(dev, info);
    log::info!("registered virtio GPU device irq={irq:?}");
    Ok(())
}

struct VirtIoDisplay<T: Transport + 'static> {
    raw: VirtIOGpu<VirtIoHalImpl, T>,
    info: DisplayInfo,
    fb_base: *mut u8,
    irq_num: Option<usize>,
    irq_enabled: bool,
    next_fence_id: u64,
}

unsafe impl<T: Transport + 'static> Send for VirtIoDisplay<T> {}

impl<T: Transport + 'static> VirtIoDisplay<T> {
    fn new(transport: T, irq_num: Option<usize>) -> Result<Self, VirtIoError> {
        let mut raw = VirtIOGpu::new(transport)?;
        let framebuffer = raw.setup_framebuffer()?;
        let fb_base = framebuffer.as_mut_ptr();
        let fb_size = framebuffer.len();
        let (width, height) = raw.resolution()?;
        let info = DisplayInfo {
            width,
            height,
            stride: width as usize * 4,
            format: PixelFormat::Xrgb8888,
            fb_size,
        };
        let _ = raw.ack_interrupt();
        Ok(Self {
            raw,
            info,
            fb_base,
            irq_num,
            irq_enabled: false,
            next_fence_id: 1,
        })
    }
}

impl<T: Transport + 'static> DriverGeneric for VirtIoDisplay<T> {
    fn name(&self) -> &str {
        "virtio-gpu"
    }
}

impl<T: Transport + 'static> rdif_display::Interface for VirtIoDisplay<T> {
    fn info(&self) -> DisplayInfo {
        self.info
    }

    fn framebuffer(&mut self) -> Result<FrameBuffer<'_>, DisplayError> {
        Ok(unsafe { FrameBuffer::from_raw_parts_mut(self.fb_base, self.info.fb_size) })
    }

    fn irq_num(&self) -> Option<usize> {
        self.irq_num
    }

    fn need_flush(&self) -> bool {
        true
    }

    fn flush(&mut self) -> Result<(), DisplayError> {
        self.raw.flush().map_err(map_display_err)
    }

    fn enable_irq(&mut self) {
        self.irq_enabled = true;
    }

    fn disable_irq(&mut self) {
        self.irq_enabled = false;
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled
    }

    fn handle_irq(&mut self) -> Event {
        let status = self.raw.ack_interrupt();
        // Drain the control queue's used ring: async EXECBUFFERs are fire-and-forget, so a
        // completion IRQ is the prompt signal that their descriptors can be recycled and the
        // queue can make room for the next batch (Linux `virtio_gpu_dequeue_ctrl_func`).
        let _ = self.raw.pump_completions();
        display_irq_event(self.irq_enabled, status)
    }

    // --- 2D resource / scanout primitives ---

    fn resource_create_2d(
        &mut self,
        resource_id: u32,
        width: u32,
        height: u32,
    ) -> Result<(), DisplayError> {
        self.raw
            .resource_create_2d(resource_id, width, height)
            .map_err(map_gpu3d_err)
    }

    fn resource_attach_backing(
        &mut self,
        resource_id: u32,
        paddr: u64,
        length: u32,
    ) -> Result<(), DisplayError> {
        self.raw
            .resource_attach_backing(resource_id, paddr, length)
            .map_err(map_gpu3d_err)
    }

    fn set_scanout(
        &mut self,
        scanout_id: u32,
        resource_id: u32,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
    ) -> Result<(), DisplayError> {
        self.raw
            .set_scanout(
                Rect {
                    x,
                    y,
                    width: w,
                    height: h,
                },
                scanout_id,
                resource_id,
            )
            .map_err(map_gpu3d_err)
    }

    fn transfer_to_host_2d(
        &mut self,
        resource_id: u32,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
    ) -> Result<(), DisplayError> {
        self.raw
            .transfer_to_host_2d(
                Rect {
                    x,
                    y,
                    width: w,
                    height: h,
                },
                0,
                resource_id,
            )
            .map_err(map_gpu3d_err)
    }

    fn resource_flush(
        &mut self,
        resource_id: u32,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
    ) -> Result<(), DisplayError> {
        self.raw
            .resource_flush(
                Rect {
                    x,
                    y,
                    width: w,
                    height: h,
                },
                resource_id,
            )
            .map_err(map_gpu3d_err)
    }

    // --- 3D methods ---

    fn has_virgl(&self) -> bool {
        self.raw.has_virgl()
    }

    fn has_resource_blob(&self) -> bool {
        self.raw.has_resource_blob()
    }

    fn ctx_create(
        &mut self,
        ctx_id: u32,
        name: &str,
        context_init: u32,
    ) -> Result<(), DisplayError> {
        self.raw
            .ctx_create(ctx_id, name, context_init)
            .map_err(map_gpu3d_err)
    }

    fn ctx_destroy(&mut self, ctx_id: u32) -> Result<(), DisplayError> {
        self.raw.ctx_destroy(ctx_id).map_err(map_gpu3d_err)
    }

    fn ctx_attach_resource(&mut self, ctx_id: u32, resource_id: u32) -> Result<(), DisplayError> {
        self.raw
            .ctx_attach_resource(ctx_id, resource_id)
            .map_err(map_gpu3d_err)
    }

    fn ctx_detach_resource(&mut self, ctx_id: u32, resource_id: u32) -> Result<(), DisplayError> {
        self.raw
            .ctx_detach_resource(ctx_id, resource_id)
            .map_err(map_gpu3d_err)
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
    ) -> Result<(), DisplayError> {
        self.raw
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
            .map_err(map_gpu3d_err)
    }

    fn resource_unref(&mut self, resource_id: u32) -> Result<(), DisplayError> {
        self.raw.resource_unref(resource_id).map_err(map_gpu3d_err)
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
    ) -> Result<(), DisplayError> {
        // Linux order (`virtio_gpu_resource_create_blob_ioctl`, virtgpu_ioctl.c):
        // submit the virgl cmd stream first, then send RESOURCE_CREATE_BLOB —
        // both go on the same virtqueue and execute in order on the host.
        if !cmd.is_empty() {
            self.submit_cmd(ctx_id, cmd)?;
        }
        // Guest backing (mem_entries) is handled by the caller at the DRM
        // layer; the HOST3D blobs used by the present path carry none.
        // SAFETY: HOST3D path passes an empty `mem_entries` slice, so the
        // device is not given any guest memory range to read/write; the
        // contract (valid, allocated, non-aliased backing covering `size`)
        // is trivially satisfied. Guest-backed blobs are created by the DRM
        // layer, which owns their backing.
        unsafe {
            self.raw.resource_create_blob(
                ctx_id,
                resource_id,
                blob_mem,
                blob_flags,
                size,
                blob_id,
                &[],
            )
        }
        .map_err(map_gpu3d_err)
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
    ) -> Result<(), DisplayError> {
        self.raw
            .transfer_to_host_3d(
                ctx_id,
                resource_id,
                GpuBox {
                    x: box_.x,
                    y: box_.y,
                    z: box_.z,
                    w: box_.w,
                    h: box_.h,
                    d: box_.d,
                },
                offset,
                level,
                stride,
                layer_stride,
            )
            .map_err(map_gpu3d_err)
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
    ) -> Result<(), DisplayError> {
        self.raw
            .transfer_from_host_3d(
                ctx_id,
                resource_id,
                GpuBox {
                    x: box_.x,
                    y: box_.y,
                    z: box_.z,
                    w: box_.w,
                    h: box_.h,
                    d: box_.d,
                },
                offset,
                level,
                stride,
                layer_stride,
            )
            .map_err(map_gpu3d_err)
    }

    fn submit_cmd(&mut self, ctx_id: u32, cmds: &[u8]) -> Result<u64, DisplayError> {
        let fence_id = self.next_fence_id;
        self.next_fence_id = self.next_fence_id.wrapping_add(1).max(1);
        self.raw
            .submit_3d(ctx_id, fence_id, cmds)
            .map_err(map_gpu3d_err)?;
        Ok(fence_id)
    }

    fn wait_fence(&mut self, fence_id: u64) -> Result<(), DisplayError> {
        self.raw.wait_fence(fence_id).map_err(map_gpu3d_err)
    }

    fn pump(&mut self) -> Result<(), DisplayError> {
        self.raw.pump_completions().map_err(map_gpu3d_err)?;
        Ok(())
    }

    fn fence_completed(&mut self, fence_id: u64) -> Result<bool, DisplayError> {
        // Drain the used ring before answering: the device's completion
        // interrupt is not delivered in every environment (this virtio-vga
        // guest never receives one), so the completion level only advances
        // when some caller pumps. Every fence query pumping keeps a
        // poll-blocked waiter's refresher able to observe completion.
        self.raw.pump_completions().map_err(map_gpu3d_err)?;
        Ok(self.raw.fence_completed(fence_id))
    }

    fn fence_completed_no_pump(&mut self, fence_id: u64) -> Result<bool, DisplayError> {
        // Completion-level-only query for IRQ handlers: the caller (card0's
        // display IRQ path) has already drained the used ring via
        // `handle_irq`'s pump, so re-pumping per registered fence would
        // double the per-IRQ cost on the frequent on-screen completion path.
        Ok(self.raw.fence_completed(fence_id))
    }

    fn get_capset_info(&mut self, index: u32) -> Result<CapsetInfo, DisplayError> {
        let resp = self.raw.get_capset_info(index).map_err(map_gpu3d_err)?;
        Ok(CapsetInfo {
            capset_id: resp.capset_id,
            max_version: resp.capset_max_version,
            max_size: resp.capset_max_size,
        })
    }

    fn get_capset(
        &mut self,
        id: u32,
        ver: u32,
        size: u32,
    ) -> Result<alloc::vec::Vec<u8>, DisplayError> {
        self.raw.get_capset(id, ver, size).map_err(map_gpu3d_err)
    }

    fn ctrl_notify(&mut self) {
        self.raw.ctrl_notify();
    }
}

fn display_irq_event(irq_enabled: bool, status: InterruptStatus) -> Event {
    if !irq_enabled {
        return Event::none();
    }
    Event {
        handled: !status.is_empty(),
        changed: status.contains(InterruptStatus::DEVICE_CONFIGURATION_INTERRUPT),
    }
}

fn map_display_err(err: VirtIoError) -> DisplayError {
    match err {
        VirtIoError::Unsupported => DisplayError::NotSupported,
        VirtIoError::NotReady => DisplayError::NotAvailable,
        _ => DisplayError::Other(alloc::boxed::Box::new(err)),
    }
}

fn map_gpu3d_err(err: VirtIoError) -> DisplayError {
    use rdif_display::Gpu3dErrorKind;
    let kind = match err {
        VirtIoError::IoError => Gpu3dErrorKind::IoError,
        VirtIoError::Unsupported => Gpu3dErrorKind::Unsupported,
        VirtIoError::NotReady => Gpu3dErrorKind::NotReady,
        VirtIoError::InvalidParam => Gpu3dErrorKind::InvalidParam,
        _ => Gpu3dErrorKind::Other,
    };
    DisplayError::Gpu3dError(kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_irq_is_ignored_until_driver_enables_it() {
        let status =
            InterruptStatus::QUEUE_INTERRUPT | InterruptStatus::DEVICE_CONFIGURATION_INTERRUPT;

        assert_eq!(display_irq_event(false, status), Event::none());
    }

    #[test]
    fn display_irq_reports_configuration_changes() {
        assert_eq!(
            display_irq_event(true, InterruptStatus::DEVICE_CONFIGURATION_INTERRUPT),
            Event {
                handled: true,
                changed: true,
            }
        );
    }

    #[test]
    fn display_irq_reports_non_configuration_interrupt_as_handled_only() {
        assert_eq!(
            display_irq_event(true, InterruptStatus::QUEUE_INTERRUPT),
            Event {
                handled: true,
                changed: false,
            }
        );
    }

    #[test]
    fn display_irq_empty_status_is_not_claimed() {
        assert_eq!(
            display_irq_event(true, InterruptStatus::empty()),
            Event::none()
        );
    }
}
