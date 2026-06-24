//! `/dev/rga` character device. Routes `RGA_BLIT_SYNC` (0x5017) to the Phase D submit path.
//! Real RGA2 hardware execution is board-gated; on QEMU `get_list` returns empty and the ioctl
//! returns `ENODEV` — the graceful-no-device path exercised in Phase E4.

use alloc::sync::Arc;
use core::{any::Any, ffi::c_int};

use axfs_ng_vfs::{NodeFlags, VfsError, VfsResult};
use rockchip_rga::{RgaVersion, RockchipRga, backend::RgaStatus, librga_abi};
use starry_vm::VmPtr;

use crate::pseudofs::{DeviceOps, dev::dma_heap};

/// `/dev/rga` character device. Stateless; dispatches ioctls to the Phase D RGA submit path.
pub struct RgaDevice;

impl RgaDevice {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RgaDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl RgaDevice {
    fn handle_blit_sync(&self, arg: usize) -> VfsResult<usize> {
        // SAFETY: `arg` is a userspace pointer to a `RgaReq` (#[repr(C)], 8-byte aligned);
        // `vm_read_uninit` faults safely on a bad address.
        let req: librga_abi::RgaReq = unsafe {
            (arg as *const librga_abi::RgaReq)
                .vm_read_uninit()?
                .assume_init()
        };

        let parsed = librga_abi::parse(&req).map_err(|_| VfsError::InvalidInput)?;

        // CONFIRM ON BOARD: RgaBufferRef.addr treated as dma-buf fd (fd-vs-phys ABI).
        // PR-E1 buffer model (design §5): yrgb_addr / uv_addr carry dma-buf fds from userspace.

        // Resolve destination buffer (always required).
        let dst_obj = dma_heap::resolve_dmabuf_fd(parsed.dst.addr as c_int)
            .map_err(|_| VfsError::BadFileDescriptor)?;
        let dst_phys = dst_obj.phys_addr();

        let dst_uv_obj: Option<Arc<dma_heap::DmaBufObject>> = if parsed.dst.uv_addr != 0 {
            let obj = dma_heap::resolve_dmabuf_fd(parsed.dst.uv_addr as c_int)
                .map_err(|_| VfsError::BadFileDescriptor)?;
            Some(obj)
        } else {
            None
        };
        let dst_uv = dst_uv_obj.as_ref().map(|o| o.phys_addr());

        // Resolve the source dma-buf fds (None for Fill — into_operation ignores src). Bind the
        // Arc<DmaBufObject> at function scope so the backing pages stay alive across submit+poll:
        // the RGA hardware reads them until the blit completes. CONFIRM ON BOARD: addr treated as a
        // dma-buf fd (the fd-vs-phys ABI model).
        let is_fill = matches!(parsed.kind, librga_abi::ParsedKind::Fill);
        let src_obj = if is_fill {
            None
        } else {
            Some(
                dma_heap::resolve_dmabuf_fd(parsed.src.addr as c_int)
                    .map_err(|_| VfsError::BadFileDescriptor)?,
            )
        };
        let src_uv_obj = if !is_fill && parsed.src.uv_addr != 0 {
            Some(
                dma_heap::resolve_dmabuf_fd(parsed.src.uv_addr as c_int)
                    .map_err(|_| VfsError::BadFileDescriptor)?,
            )
        } else {
            None
        };
        let src_phys = src_obj.as_ref().map(|o| o.phys_addr()).unwrap_or(0);
        let src_uv = src_uv_obj.as_ref().map(|o| o.phys_addr());

        let op = parsed
            .into_operation(src_phys, src_uv, dst_phys, dst_uv)
            .map_err(|_| VfsError::InvalidInput)?;

        // QEMU path: no RGA2 device → ENODEV.
        let devs = rdrive::get_list::<RockchipRga>();
        if devs.is_empty() {
            return Err(VfsError::NoSuchDevice);
        }

        // Acquire the device. `lock()` spins until available (mirrors rga_selftest.rs pattern).
        let mut guard = devs[0].lock().map_err(|_| VfsError::ResourceBusy)?;
        let rga = &mut *guard;

        let core = rga
            .cores_mut()
            .iter_mut()
            .find(|c| c.config().version == RgaVersion::Rga2)
            .ok_or(VfsError::NoSuchDevice)?;

        // DMA coherency: the dma-heap backing is CACHED (see DmaBufObject), so before the engine
        // reads the source / writes the destination we must clean dirty CPU lines to DRAM
        // (sync_for_device), and after the engine writes the destination we must invalidate so the
        // CPU/NPU reads the engine's output rather than stale cache (sync_for_cpu). A fully
        // Linux-faithful driver would leave this to userspace `DMA_BUF_IOCTL_SYNC`, but we do it
        // defensively here: callers (e.g. librga) do not reliably bracket the op, and on a cached
        // backing the engine's output is otherwise clobbered by the destination's dirty alloc-time
        // lines — the exact failure the rga-selftest hit before this clean-before/invalidate-after
        // discipline was added.
        if let Some(o) = src_obj.as_ref() {
            o.sync_for_device();
        }
        if let Some(o) = src_uv_obj.as_ref() {
            o.sync_for_device();
        }
        dst_obj.sync_for_device();
        if let Some(o) = dst_uv_obj.as_ref() {
            o.sync_for_device();
        }

        // Submit + busy-wait poll (no IRQ in PR-E1).
        core.start(&op).map_err(|_| VfsError::InvalidInput)?;

        for _ in 0..500 {
            match core.poll_status() {
                RgaStatus::Done => {
                    core.finish();
                    // Hardware is done writing; invalidate the destination(s) so the CPU/NPU sees
                    // the engine's writes (cached backing). src/dst objs kept alive across the op.
                    dst_obj.sync_for_cpu();
                    if let Some(o) = dst_uv_obj.as_ref() {
                        o.sync_for_cpu();
                    }
                    return Ok(0);
                }
                RgaStatus::Error => {
                    core.finish();
                    return Err(VfsError::Io);
                }
                RgaStatus::Busy => {
                    ax_runtime::hal::time::busy_wait(core::time::Duration::from_micros(100));
                }
            }
        }

        // Timeout: attempt recovery, then return ETIMEDOUT.
        let _ = core.recover();
        Err(VfsError::TimedOut)
    }
}

impl DeviceOps for RgaDevice {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        Err(VfsError::InvalidInput)
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Err(VfsError::InvalidInput)
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        if arg == 0 {
            return Err(VfsError::InvalidInput);
        }
        match cmd {
            librga_abi::RGA_BLIT_SYNC => self.handle_blit_sync(arg),
            // RGA_GET_VERSION: minimal stub; real version string is board-deferred.
            librga_abi::RGA_GET_VERSION => Ok(0),
            // RGA_BLIT_ASYNC: sync-only in PR-E1.
            librga_abi::RGA_BLIT_ASYNC => Err(VfsError::Unsupported),
            _ => Err(VfsError::NotATty),
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE
    }
}
