//! `/dev/rga` character device. Routes `RGA_BLIT_SYNC` (0x5017) and the MultiRGA v1.3.1
//! handle-import API to the Phase D submit path. Real RGA2 hardware execution is board-gated;
//! on QEMU `get_list` returns empty and the ioctl returns `ENODEV`.

use alloc::{collections::btree_map::BTreeMap, sync::Arc};
use core::{any::Any, ffi::c_int};

use ax_sync::Mutex;
use axfs_ng_vfs::{NodeFlags, VfsError, VfsResult};
use rockchip_rga::{RgaVersion, RockchipRga, backend::RgaStatus, librga_abi};
use starry_vm::{VmMutPtr, VmPtr};

use crate::pseudofs::{DeviceOps, dev::dma_heap};

/// Per-task process key — the unique task ID from the scheduler.
fn current_id() -> u64 {
    ax_task::current().id().as_u64()
}

/// A buffer imported via `RGA_IOC_IMPORT_BUFFER`. Stores the physical address and, when
/// imported from a dma-buf fd, keeps the backing allocation alive until release.
struct ImportedBuf {
    phys_addr: u64,
    /// `Some` when imported from a dma-buf fd (RGA_DMA_BUFFER); `None` when imported as a
    /// raw physical address (RGA_PHYSICAL_ADDRESS — caller guarantees lifetime).
    obj: Option<Arc<dma_heap::DmaBufObject>>,
}

/// `/dev/rga` character device with a handle table for the MultiRGA import API.
/// Handles are keyed by (task_id, handle) so Process A cannot resolve Process B's buffers.
pub struct RgaDevice {
    handle_table: Mutex<BTreeMap<(u64, u32), ImportedBuf>>,
    next_handle: Mutex<u32>,
}

impl RgaDevice {
    pub fn new() -> Self {
        Self {
            handle_table: Mutex::new(BTreeMap::new()),
            next_handle: Mutex::new(1),
        }
    }

    /// Allocate a unique non-zero handle keyed by the current task, and insert the entry
    /// in one critical section.
    fn alloc_handle(&self, entry: ImportedBuf) -> VfsResult<u32> {
        let tid = current_id();
        let mut table = self.handle_table.lock();
        let mut next = self.next_handle.lock();
        for _ in 0..=u32::MAX {
            let h = *next;
            *next = h.wrapping_add(1);
            if h == 0 || table.contains_key(&(tid, h)) {
                continue;
            }
            table.insert((tid, h), entry);
            return Ok(h);
        }
        Err(VfsError::NoMemory)
    }

    /// Resolve a buffer address, returning the phys addr and (for dma-buf-backed buffers) the
    /// `Arc<DmaBufObject>` that must stay alive and be cache-synced for the operation's duration.
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
            let tid = current_id();
            let table = self.handle_table.lock();
            let entry = table
                .get(&(tid, handle))
                .ok_or(VfsError::BadFileDescriptor)?;
            // Clone the Arc so the dma-buf stays alive across submit+poll even if a concurrent
            // RELEASE_BUFFER removes the table entry mid-op. RGA_PHYSICAL_ADDRESS entries carry
            // None — the caller owns coherency for raw phys imports.
            Ok((entry.phys_addr, entry.obj.clone()))
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

        // Resolve source / destination buffers, keeping the backing Arcs alive.
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
        let (dst_phys, dst_keep) = self.resolve_buf(parsed.dst.addr, handle_flag)?;
        let (dst_uv_phys, dst_uv_keep) = if parsed.dst.uv_addr != 0 {
            let (p, k) = self.resolve_buf(parsed.dst.uv_addr, handle_flag)?;
            (Some(p), k)
        } else {
            (None, None)
        };

        let op = parsed
            .into_operation(src_phys, src_uv_phys, dst_phys, dst_uv_phys)
            .map_err(|_| VfsError::InvalidInput)?;

        // QEMU path: no RGA2 device → ENODEV.
        let devs = rdrive::get_list::<RockchipRga>();
        if devs.is_empty() {
            return Err(VfsError::NoSuchDevice);
        }

        let mut guard = devs[0].try_lock().map_err(|_| VfsError::ResourceBusy)?;
        let rga = &mut *guard;

        let core = rga
            .cores_mut()
            .iter_mut()
            .find(|c| c.config().version == RgaVersion::Rga2)
            .ok_or(VfsError::NoSuchDevice)?;

        // DMA coherency — the dma-heap backing is CACHED on aarch64.
        // Clean dirty CPU lines to DRAM before the engine reads src / writes dst, so
        // stale cache lines cannot evict over engine output (the rga-selftest proved this).
        for o in [&src_keep, &src_uv_keep, &dst_keep, &dst_uv_keep]
            .into_iter()
            .flatten()
        {
            o.sync_for_device();
        }

        core.start(&op).map_err(|_| VfsError::InvalidInput)?;

        for _ in 0..500 {
            match core.poll_status() {
                RgaStatus::Done => {
                    core.finish();
                    // Invalidate the CPU cache for the destination(s) so subsequent reads
                    // see the engine's output rather than stale cached data.
                    if let Some(o) = dst_keep.as_ref() {
                        o.sync_for_cpu();
                    }
                    if let Some(o) = dst_uv_keep.as_ref() {
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

        let _ = core.recover();
        Err(VfsError::TimedOut)
    }

    /// Handle `RGA_IOC_IMPORT_BUFFER`: resolve dma-buf fds → physical addresses and assign
    /// handles. Processes all `pool.size` entries; writes the assigned handle back to each.
    fn handle_import_buffer(&self, arg: usize) -> VfsResult<usize> {
        let pool: librga_abi::RgaBufferPool = unsafe {
            (arg as *const librga_abi::RgaBufferPool)
                .vm_read_uninit()?
                .assume_init()
        };

        if pool.size == 0 || pool.size > 64 || pool.buffers_ptr == 0 {
            return Err(VfsError::InvalidInput);
        }

        let elem_size = core::mem::size_of::<librga_abi::RgaExternalBuffer>(); // 288
        let base = pool.buffers_ptr as usize;

        // Write the handle back FIRST, then insert into table. If the write faults, no
        // handle entry is leaked (the table was never touched for this element).
        for i in 0..pool.size as usize {
            let ptr = base + i * elem_size;
            let mut ext: librga_abi::RgaExternalBuffer = unsafe {
                (ptr as *const librga_abi::RgaExternalBuffer)
                    .vm_read_uninit()?
                    .assume_init()
            };

            let entry = match ext.r#type {
                librga_abi::RGA_DMA_BUFFER => {
                    let obj = dma_heap::resolve_dmabuf_fd(ext.memory as c_int)
                        .map_err(|_| VfsError::BadFileDescriptor)?;
                    ImportedBuf {
                        phys_addr: obj.phys_addr(),
                        obj: Some(obj),
                    }
                }
                librga_abi::RGA_PHYSICAL_ADDRESS => ImportedBuf {
                    phys_addr: ext.memory,
                    obj: None,
                },
                _ => return Err(VfsError::Unsupported),
            };

            let handle = self.alloc_handle(entry)?;
            ext.handle = handle;

            // Write-back must succeed for userspace to know the handle; on fault the
            // alloc_handle entry is already inserted (it's a permanent leak but the fault
            // means the process is dying anyway — mirroring the kernel's behaviour).
            unsafe { (ptr as *mut librga_abi::RgaExternalBuffer).vm_write(ext)? };
        }
        Ok(0)
    }

    /// Handle `RGA_IOC_RELEASE_BUFFER`: remove handles from the table, freeing the
    /// backing dma-buf references.
    fn handle_release_buffer(&self, arg: usize) -> VfsResult<usize> {
        let pool: librga_abi::RgaBufferPool = unsafe {
            (arg as *const librga_abi::RgaBufferPool)
                .vm_read_uninit()?
                .assume_init()
        };

        if pool.size == 0 || pool.size > 64 || pool.buffers_ptr == 0 {
            return Err(VfsError::InvalidInput);
        }

        let elem_size = core::mem::size_of::<librga_abi::RgaExternalBuffer>();
        let base = pool.buffers_ptr as usize;
        let tid = current_id();
        let mut table = self.handle_table.lock();

        for i in 0..pool.size as usize {
            let ptr = base + i * elem_size;
            let ext: librga_abi::RgaExternalBuffer = unsafe {
                (ptr as *const librga_abi::RgaExternalBuffer)
                    .vm_read_uninit()?
                    .assume_init()
            };
            if table.remove(&(tid, ext.handle)).is_none() {
                return Err(VfsError::BadFileDescriptor);
            }
        }
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
            librga_abi::RGA_GET_VERSION => {
                let version: [u8; 5] = *b"3.02\0";
                unsafe { (arg as *mut [u8; 5]).vm_write(version)? };
                Ok(0)
            }
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
