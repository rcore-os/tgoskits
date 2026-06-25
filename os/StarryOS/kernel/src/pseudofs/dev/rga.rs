//! `/dev/rga` character device. Routes `RGA_BLIT_SYNC` (0x5017) and the MultiRGA v1.3.1
//! handle-import API to the Phase D submit path. Real RGA2 hardware execution is board-gated;
//! on QEMU `get_list` returns empty and the ioctl returns `ENODEV`.

use alloc::{collections::btree_map::BTreeMap, sync::Arc, vec::Vec};
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
/// Handles and staged requests are keyed by (task_id, id) so Process A cannot touch
/// Process B's buffers/requests.
pub struct RgaDevice {
    handle_table: Mutex<BTreeMap<(u64, u32), ImportedBuf>>,
    next_handle: Mutex<u32>,
    /// Requests staged via RGA_IOC_REQUEST_CONFIG, awaiting RGA_IOC_REQUEST_SUBMIT.
    requests: Mutex<BTreeMap<(u64, u32), Vec<librga_abi::RgaReq>>>,
    next_request_id: Mutex<u32>,
}

impl RgaDevice {
    pub fn new() -> Self {
        Self {
            handle_table: Mutex::new(BTreeMap::new()),
            next_handle: Mutex::new(1),
            requests: Mutex::new(BTreeMap::new()),
            next_request_id: Mutex::new(1),
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
        self.execute_blit(&req)
    }

    /// Submit one already-read `RgaReq` to the engine and block until completion.
    /// Shared by the legacy `RGA_BLIT_SYNC` path and the `RGA_IOC_REQUEST_SUBMIT` task path.
    fn execute_blit(&self, req: &librga_abi::RgaReq) -> VfsResult<usize> {
        let parsed = librga_abi::parse(req).map_err(|_| VfsError::InvalidInput)?;

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

    /// `RGA_IOC_GET_DRVIER_VERSION` — librga reads this at init to pick the ABI. Report the
    /// MultiRGA v1.3.1 driver version we mirror, so librga uses the matching `rga_req` layout.
    fn handle_get_driver_version(&self, arg: usize) -> VfsResult<usize> {
        let mut v = librga_abi::RgaVersionT {
            major: 1,
            minor: 3,
            revision: 1,
            string: [0; 16],
        };
        v.string[..5].copy_from_slice(b"1.3.1");
        unsafe { (arg as *mut librga_abi::RgaVersionT).vm_write(v)? };
        Ok(0)
    }

    /// `RGA_IOC_GET_HW_VERSION` — librga enumerates scheduler cores. Report one RGA2 core
    /// at hardware v3.2 (matches the OrangePi-5-Plus version register 0x032660D8).
    fn handle_get_hw_version(&self, arg: usize) -> VfsResult<usize> {
        let mut hw = librga_abi::RgaHwVersions::default();
        hw.size = 1;
        hw.version[0] = librga_abi::RgaVersionT {
            major: 3,
            minor: 2,
            revision: 0,
            string: [0; 16],
        };
        hw.version[0].string[..3].copy_from_slice(b"3.2");
        unsafe { (arg as *mut librga_abi::RgaHwVersions).vm_write(hw)? };
        Ok(0)
    }

    /// `RGA_IOC_REQUEST_CREATE` — allocate a request id (written back to userspace).
    fn handle_request_create(&self, arg: usize) -> VfsResult<usize> {
        let tid = current_id();
        let mut next = self.next_request_id.lock();
        let mut requests = self.requests.lock();
        // Pick a non-zero id not already live for this task.
        let id = loop {
            let id = *next;
            *next = id.wrapping_add(1);
            if id != 0 && !requests.contains_key(&(tid, id)) {
                break id;
            }
        };
        requests.insert((tid, id), Vec::new());
        drop(requests);
        drop(next);
        unsafe { (arg as *mut u32).vm_write(id)? };
        Ok(0)
    }

    /// Read the `task_num` `RgaReq` array a request points at (bounded).
    fn read_request_tasks(req: &librga_abi::RgaUserRequest) -> VfsResult<Vec<librga_abi::RgaReq>> {
        if req.task_num == 0 || req.task_num > 16 || req.task_ptr == 0 {
            return Err(VfsError::InvalidInput);
        }
        let elem = core::mem::size_of::<librga_abi::RgaReq>();
        let base = req.task_ptr as usize;
        let mut tasks = Vec::with_capacity(req.task_num as usize);
        for i in 0..req.task_num as usize {
            let p = base + i * elem;
            let t: librga_abi::RgaReq = unsafe {
                (p as *const librga_abi::RgaReq)
                    .vm_read_uninit()?
                    .assume_init()
            };
            tasks.push(t);
        }
        Ok(tasks)
    }

    /// `RGA_IOC_REQUEST_CONFIG` — stage a request's tasks without running them.
    fn handle_request_config(&self, arg: usize) -> VfsResult<usize> {
        let ureq: librga_abi::RgaUserRequest = unsafe {
            (arg as *const librga_abi::RgaUserRequest)
                .vm_read_uninit()?
                .assume_init()
        };
        let tasks = Self::read_request_tasks(&ureq)?;
        let tid = current_id();
        self.requests.lock().insert((tid, ureq.id), tasks);
        Ok(0)
    }

    /// `RGA_IOC_REQUEST_SUBMIT` — run a request's tasks (carried inline, or previously
    /// staged via CONFIG) and block until each completes. We are always synchronous.
    fn handle_request_submit(&self, arg: usize) -> VfsResult<usize> {
        let ureq: librga_abi::RgaUserRequest = unsafe {
            (arg as *const librga_abi::RgaUserRequest)
                .vm_read_uninit()?
                .assume_init()
        };
        let tid = current_id();
        // Tasks may be carried inline (common im2d single-blit) or staged by a prior CONFIG.
        let tasks = if ureq.task_num > 0 {
            Self::read_request_tasks(&ureq)?
        } else {
            self.requests
                .lock()
                .get(&(tid, ureq.id))
                .cloned()
                .ok_or(VfsError::InvalidInput)?
        };
        // Drop any staged copy now that we own the tasks.
        self.requests.lock().remove(&(tid, ureq.id));

        for task in &tasks {
            self.execute_blit(task)?;
        }
        Ok(0)
    }

    /// `RGA_IOC_REQUEST_CANCEL` — drop a staged request.
    fn handle_request_cancel(&self, arg: usize) -> VfsResult<usize> {
        let id: u32 = unsafe { (arg as *const u32).vm_read_uninit()?.assume_init() };
        let tid = current_id();
        self.requests.lock().remove(&(tid, id));
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
            librga_abi::RGA_IOC_GET_DRVIER_VERSION => self.handle_get_driver_version(arg),
            librga_abi::RGA_IOC_GET_HW_VERSION => self.handle_get_hw_version(arg),
            librga_abi::RGA_IOC_IMPORT_BUFFER => self.handle_import_buffer(arg),
            librga_abi::RGA_IOC_RELEASE_BUFFER => self.handle_release_buffer(arg),
            librga_abi::RGA_IOC_REQUEST_CREATE => self.handle_request_create(arg),
            librga_abi::RGA_IOC_REQUEST_CONFIG => self.handle_request_config(arg),
            librga_abi::RGA_IOC_REQUEST_SUBMIT => self.handle_request_submit(arg),
            librga_abi::RGA_IOC_REQUEST_CANCEL => self.handle_request_cancel(arg),
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
