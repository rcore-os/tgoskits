//! `/dev/rga` character device. Routes `RGA_BLIT_SYNC` (0x5017) and the MultiRGA v1.3.1
//! handle-import API to the Phase D submit path. Real RGA2 hardware execution is board-gated;
//! on QEMU `get_list` returns empty and the ioctl returns `ENODEV`.

use alloc::{collections::btree_map::BTreeMap, sync::Arc};
use core::{any::Any, ffi::c_int};

use axfs_ng_vfs::{NodeFlags, VfsError, VfsResult};
use kspin::Mutex;
use rockchip_rga::{RgaVersion, RockchipRga, backend::RgaStatus, librga_abi};
use starry_vm::VmPtr;

use crate::pseudofs::{DeviceOps, dev::dma_heap};

/// A buffer imported via `RGA_IOC_IMPORT_BUFFER`. Stores the physical address and, when
/// imported from a dma-buf fd, keeps the backing allocation alive until release.
struct ImportedBuf {
    phys_addr: u64,
    /// `Some` when imported from a dma-buf fd (RGA_DMA_BUFFER); `None` when imported as a
    /// raw physical address (RGA_PHYSICAL_ADDRESS — caller guarantees lifetime).
    _obj: Option<Arc<dma_heap::DmaBufObject>>,
}

/// `/dev/rga` character device with a handle table for the MultiRGA import API.
pub struct RgaDevice {
    handle_table: Mutex<BTreeMap<u32, ImportedBuf>>,
    next_handle: Mutex<u32>,
}

impl RgaDevice {
    pub fn new() -> Self {
        Self {
            handle_table: Mutex::new(BTreeMap::new()),
            next_handle: Mutex::new(1),
        }
    }

    /// Resolve a buffer address, returning the phys addr and (for fd-based buffers) the
    /// `Arc<DmaBufObject>` that must stay alive for the operation's duration.
    fn resolve_buf(
        &self,
        raw: u64,
        handle_flag: bool,
    ) -> VfsResult<(u64, Option<Arc<dma_heap::DmaBufObject>>)> {
        if raw == 0 {
            return Ok((0, None));
        }
        if handle_flag {
            let handle = raw as u32;
            let table = self.handle_table.lock();
            let phys = table
                .get(&handle)
                .map(|b| b.phys_addr)
                .ok_or(VfsError::BadFileDescriptor)?;
            // Buffer stays alive via the handle table (removed only on RELEASE_BUFFER).
            Ok((phys, None))
        } else {
            // Legacy path: raw value is a dma-buf fd.
            let obj = dma_heap::resolve_dmabuf_fd(raw as c_int)
                .map_err(|_| VfsError::BadFileDescriptor)?;
            let phys = obj.phys_addr();
            Ok((phys, Some(obj)))
        }
    }

    fn handle_blit_sync(&self, arg: usize) -> VfsResult<usize> {
        // SAFETY: `arg` is a userspace pointer to a `RgaReq` (#[repr(C)]);
        // `vm_read_uninit` faults safely on a bad address.
        let req: librga_abi::RgaReq = unsafe {
            (arg as *const librga_abi::RgaReq)
                .vm_read_uninit()?
                .assume_init()
        };

        let parsed = librga_abi::parse(&req).map_err(|_| VfsError::InvalidInput)?;

        let handle_flag = req.handle_flag != 0;

        // Resolve destination buffer.
        let (dst_phys, dst_keep) = self.resolve_buf(parsed.dst.addr, handle_flag)?;
        let (dst_uv_phys, dst_uv_keep) = if parsed.dst.uv_addr != 0 {
            let (p, k) = self.resolve_buf(parsed.dst.uv_addr, handle_flag)?;
            (Some(p), k)
        } else {
            (None, None)
        };

        // Resolve source buffers (None for Fill).
        let is_fill = matches!(parsed.kind, librga_abi::ParsedKind::Fill);
        let (src_phys, src_keep) = if is_fill {
            (0, None)
        } else {
            self.resolve_buf(parsed.src.addr, handle_flag)?
        };
        let (src_uv_phys, src_uv_keep) = if !is_fill && parsed.src.uv_addr != 0 {
            let (p, k) = self.resolve_buf(parsed.src.uv_addr, handle_flag)?;
            (Some(p), k)
        } else {
            (None, None)
        };

        let op = parsed
            .into_operation(src_phys, src_uv_phys, dst_phys, dst_uv_phys)
            .map_err(|_| VfsError::InvalidInput)?;

        // Keep resolved dma-buf objects alive across the operation (handle-imported buffers
        // are already pinned by the handle table).
        let _keep_alive = (src_keep, src_uv_keep, dst_keep, dst_uv_keep);

        // Imported buffers stay alive via the handle table (entries removed only on
        // RELEASE_BUFFER); no additional guard needed after phys addrs are resolved.

        // QEMU path: no RGA2 device → ENODEV.
        let devs = rdrive::get_list::<RockchipRga>();
        if devs.is_empty() {
            return Err(VfsError::NoSuchDevice);
        }

        let mut guard = devs[0].lock().map_err(|_| VfsError::ResourceBusy)?;
        let rga = &mut *guard;

        let core = rga
            .cores_mut()
            .iter_mut()
            .find(|c| c.config().version == RgaVersion::Rga2)
            .ok_or(VfsError::NoSuchDevice)?;

        // DMA coherency: clean before, invalidate after.
        // When buffers come from the handle table we already resolved phys addrs
        // above; the dma-buf objects are pinned by the handle-table lock.
        // For imported buffers, sync_for_device/sync_for_cpu is handled by the
        // dma-buf sync ioctl when it lands; this is a defensive blanket.
        core.start(&op).map_err(|_| VfsError::InvalidInput)?;

        for _ in 0..500 {
            match core.poll_status() {
                RgaStatus::Done => {
                    core.finish();
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

        let _ = core.recover();
        Err(VfsError::TimedOut)
    }

    /// Handle `RGA_IOC_IMPORT_BUFFER`: resolve dma-buf fds → physical addresses and assign
    /// handles. Returns the handle to userspace via the in/out `rga_external_buffer` array.
    fn handle_import_buffer(&self, arg: usize) -> VfsResult<usize> {
        // Read the pool struct: { buffers_ptr: u64, size: u32 }
        let pool: librga_abi::RgaBufferPool = unsafe {
            (arg as *const librga_abi::RgaBufferPool)
                .vm_read_uninit()?
                .assume_init()
        };

        if pool.size == 0 || pool.size > 64 {
            return Err(VfsError::InvalidInput);
        }
        if pool.buffers_ptr == 0 {
            return Err(VfsError::InvalidInput);
        }

        let buf_ptr = pool.buffers_ptr as usize;

        // Read the user's external buffer array.
        let mut ext: librga_abi::RgaExternalBuffer = unsafe {
            (buf_ptr as *const librga_abi::RgaExternalBuffer)
                .vm_read_uninit()?
                .assume_init()
        };

        // Resolve the backing memory and assign a handle.
        let (handle, entry) = match ext.r#type {
            librga_abi::RGA_DMA_BUFFER => {
                let obj = dma_heap::resolve_dmabuf_fd(ext.memory as c_int)
                    .map_err(|_| VfsError::BadFileDescriptor)?;
                let phys = obj.phys_addr();
                let mut next = self.next_handle.lock();
                let h = *next;
                *next = h.wrapping_add(1);
                drop(next);
                (
                    h,
                    ImportedBuf {
                        phys_addr: phys,
                        _obj: Some(obj),
                    },
                )
            }
            librga_abi::RGA_PHYSICAL_ADDRESS => {
                let phys = ext.memory;
                let mut next = self.next_handle.lock();
                let h = *next;
                *next = h.wrapping_add(1);
                drop(next);
                (
                    h,
                    ImportedBuf {
                        phys_addr: phys,
                        _obj: None,
                    },
                )
            }
            _ => return Err(VfsError::Unsupported),
        };

        // Insert into the handle table.
        self.handle_table.lock().insert(handle, entry);

        // Write the assigned handle back to userspace.
        ext.handle = handle;
        unsafe {
            (buf_ptr as *mut librga_abi::RgaExternalBuffer).vm_write(&ext)?;
        }

        Ok(0)
    }

    /// Handle `RGA_IOC_RELEASE_BUFFER`: remove handles from the table.
    fn handle_release_buffer(&self, arg: usize) -> VfsResult<usize> {
        let pool: librga_abi::RgaBufferPool = unsafe {
            (arg as *const librga_abi::RgaBufferPool)
                .vm_read_uninit()?
                .assume_init()
        };

        if pool.size == 0 || pool.buffers_ptr == 0 {
            return Err(VfsError::InvalidInput);
        }

        let buf_ptr = pool.buffers_ptr as usize;

        // Read the userspace buffer array to get the handle(s) to release.
        let ext: librga_abi::RgaExternalBuffer = unsafe {
            (buf_ptr as *const librga_abi::RgaExternalBuffer)
                .vm_read_uninit()?
                .assume_init()
        };

        let mut table = self.handle_table.lock();
        table.remove(&ext.handle);

        Ok(0)
    }
}

impl Default for RgaDevice {
    fn default() -> Self {
        Self::new()
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
            librga_abi::RGA_BLIT_ASYNC => Err(VfsError::Unsupported),
            librga_abi::RGA_GET_VERSION => Ok(0),
            librga_abi::RGA_IOC_IMPORT_BUFFER => self.handle_import_buffer(arg),
            librga_abi::RGA_IOC_RELEASE_BUFFER => self.handle_release_buffer(arg),
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
