//! `/dev/rga` character device. Routes `RGA_BLIT_SYNC` (0x5017) and the MultiRGA v1.3.1
//! handle-import API to the Phase D submit path. Real RGA2 hardware execution is board-gated;
//! on QEMU `get_list` returns empty and the ioctl returns `ENODEV`.
//!
//! Lifetime model (mirrors the Linux RGA driver's `file->private_data`): the node object
//! [`RgaDevice`] holds no per-open state; each `open("/dev/rga")` gets its own [`RgaFile`]
//! holding that open's handle/request tables. `dup`/`fork`/`SCM_RIGHTS` share the same
//! `Arc<RgaFile>`, so siblings share the session and it is freed exactly once, when the last
//! reference is dropped (the `release()` analogue) — no pid/tgid keying, no open-count
//! bookkeeping.

use alloc::{borrow::Cow, collections::btree_map::BTreeMap, sync::Arc, vec::Vec};
use core::{any::Any, ffi::c_int, task::Context};

use ax_errno::AxResult;
use ax_sync::Mutex;
use axfs_ng_vfs::{NodeFlags, VfsError, VfsResult};
use axpoll::{IoEvents, Pollable};
use rockchip_rga::{
    RgaVersion, RockchipRga,
    backend::RgaStatus,
    librga_abi,
    operation::{ImageDesc, RgaOperation},
};
use starry_vm::{VmMutPtr, VmPtr};

use crate::{
    file::{
        File as KernelFile, FileLike, IoDst, IoSrc, Kstat,
        dmabuf::{DmaBufFile, resolve_contiguous_dmabuf},
    },
    pseudofs::DeviceOps,
    task::AsThread,
};

/// Per-ioctl cap on buffers imported by `RGA_IOC_IMPORT_BUFFER`
/// (`RGA_BUFFER_POOL_SIZE_MAX` in the RGA3 UAPI).
const RGA_BUFFER_POOL_SIZE_MAX: u32 = 40;
/// Cap on tasks in one request for `RGA_IOC_REQUEST_CONFIG`/`SUBMIT`
/// (`RGA_TASK_NUM_MAX` in the RGA3 UAPI).
const RGA_TASK_NUM_MAX: u32 = 256;

/// A buffer imported via `RGA_IOC_IMPORT_BUFFER`. Stores the physical address, the byte
/// length (for bounds-checking planes before an MMU-off DMA), and, when imported from a
/// dma-buf fd, keeps the backing allocation alive until release.
struct ImportedBuf {
    phys_addr: u64,
    /// Byte length of the imported buffer. `u64::MAX` for a raw `RGA_PHYSICAL_ADDRESS` import
    /// (CAP_SYS_RAWIO): unbounded, the privileged caller owns the range.
    len: u64,
    /// `Some` when imported from a dma-buf fd (RGA_DMA_BUFFER); `None` when imported as a
    /// raw physical address (RGA_PHYSICAL_ADDRESS — caller guarantees lifetime).
    obj: Option<Arc<DmaBufFile>>,
}

/// `/dev/rga` device node. Shared across every open, so it holds **no** per-open state:
/// each open is served by its own [`RgaFile`]. The node only exists so the VFS has a
/// `Device` to route opens through (see [`open_rga_file`]) and so hardware/global state can
/// hang off it in future; today the hardware is reached through the global `rdrive` list.
pub(crate) struct RgaDevice;

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

impl DeviceOps for RgaDevice {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        Err(VfsError::InvalidInput)
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Err(VfsError::InvalidInput)
    }

    /// Never reached in practice: `open("/dev/rga")` is rerouted to a per-open [`RgaFile`]
    /// in `fd_ops` (see [`open_rga_file`]), whose `ioctl` handles the ABI. A bare node ioctl
    /// has no session, so it cannot serve the handle API.
    fn ioctl(&self, _cmd: u32, _arg: usize) -> VfsResult<usize> {
        Err(VfsError::NotATty)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE
    }
}

/// True if `inner` (a device node's `DeviceOps` as `&dyn Any`) is the `/dev/rga` node.
/// Mirrors `usbfs::is_usbfs_device`.
pub(crate) fn is_rga_device(inner: &dyn Any) -> bool {
    inner.is::<RgaDevice>()
}

/// Build the per-open [`RgaFile`] for an `open("/dev/rga")`. Mirrors `usbfs::open_usbfs_file`:
/// each call allocates a fresh session; the returned `Arc<dyn FileLike>` is what `dup`/`fork`
/// share and what is dropped (freeing the session) at last close.
pub(crate) fn open_rga_file(file: ax_fs_ng::File, open_flags: u32) -> AxResult<Arc<dyn FileLike>> {
    Ok(Arc::new(RgaFile::new(KernelFile::new(file, open_flags))))
}

/// One open file description of `/dev/rga`. Owns this open's handle and request tables;
/// shared by `dup`/`fork` via its `Arc` and dropped exactly once at last close, which frees
/// the tables (and the dma-buf `Arc`s they hold) — the `release()` analogue. No pid/tgid.
/// Only ever surfaced as `Arc<dyn FileLike>` (see [`open_rga_file`]).
struct RgaFile {
    /// Backing file: keeps the node alive and serves the trivial `FileLike` methods.
    base: KernelFile,
    /// Handles assigned by `RGA_IOC_IMPORT_BUFFER`, keyed by handle id (this open's namespace).
    handle_table: Mutex<BTreeMap<u32, ImportedBuf>>,
    next_handle: Mutex<u32>,
    /// Requests created by `RGA_IOC_REQUEST_CREATE`, keyed by request id. An entry's presence
    /// marks the id as live; the `Vec` holds tasks staged via `RGA_IOC_REQUEST_CONFIG`.
    requests: Mutex<BTreeMap<u32, Vec<librga_abi::RgaReq>>>,
    next_request_id: Mutex<u32>,
}

impl RgaFile {
    fn new(base: KernelFile) -> Self {
        Self {
            base,
            handle_table: Mutex::new(BTreeMap::new()),
            next_handle: Mutex::new(1),
            requests: Mutex::new(BTreeMap::new()),
            next_request_id: Mutex::new(1),
        }
    }

    /// Allocate a unique non-zero handle for this open and insert the entry in one critical
    /// section.
    fn alloc_handle(&self, entry: ImportedBuf) -> VfsResult<u32> {
        let mut table = self.handle_table.lock();
        let mut next = self.next_handle.lock();
        for _ in 0..=u32::MAX {
            let h = *next;
            *next = h.wrapping_add(1);
            if h == 0 || table.contains_key(&h) {
                continue;
            }
            table.insert(h, entry);
            return Ok(h);
        }
        Err(VfsError::NoMemory)
    }

    /// Resolve a buffer address, returning the phys addr, its byte length (for bounds checks),
    /// and (for dma-buf-backed buffers) the `Arc<DmaBufFile>` that must stay alive for the
    /// operation's duration. The shared `/dev/dma_heap` allocator is DMA-coherent, so no cache
    /// maintenance is required.
    fn resolve_buf(
        &self,
        raw: u64,
        handle_flag: bool,
    ) -> VfsResult<(u64, u64, Option<Arc<DmaBufFile>>)> {
        if raw == 0 {
            return Ok((0, 0, None));
        }
        if handle_flag {
            let handle = raw as u32;
            let table = self.handle_table.lock();
            let entry = table.get(&handle).ok_or(VfsError::BadFileDescriptor)?;
            // Clone the Arc so the dma-buf stays alive across submit+poll even if a concurrent
            // RELEASE_BUFFER removes the table entry mid-op. RGA_PHYSICAL_ADDRESS entries carry
            // None — the caller owns coherency for raw phys imports.
            Ok((entry.phys_addr, entry.len, entry.obj.clone()))
        } else {
            // Legacy path: raw value is a dma-buf fd.
            let obj = resolve_contiguous_dmabuf(raw as c_int).ok_or(VfsError::BadFileDescriptor)?;
            let phys = obj.phys_base() as u64;
            let len = obj.size() as u64;
            Ok((phys, len, Some(obj)))
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
        let parsed = librga_abi::parse(req).map_err(|e| {
            warn!("RGA_BLIT: rejecting unsupported request: {e:?}");
            VfsError::InvalidInput
        })?;

        let handle_flag = req.handle_flag != 0;

        // Resolve source / destination buffers, keeping the backing Arcs alive and recording
        // each buffer's byte length for the bounds check below.
        let is_fill = matches!(parsed.kind, librga_abi::ParsedKind::Fill);
        let (src_phys, src_len, src_keep) = if is_fill {
            (0, 0, None)
        } else {
            self.resolve_buf(parsed.src.addr, handle_flag)?
        };
        let (src_uv_phys, src_uv_len, src_uv_keep) = if !is_fill && parsed.src.uv_addr != 0 {
            let (p, l, k) = self.resolve_buf(parsed.src.uv_addr, handle_flag)?;
            (Some(p), Some(l), k)
        } else {
            (None, None, None)
        };
        let (dst_phys, dst_len, dst_keep) = self.resolve_buf(parsed.dst.addr, handle_flag)?;
        let (dst_uv_phys, dst_uv_len, dst_uv_keep) = if parsed.dst.uv_addr != 0 {
            let (p, l, k) = self.resolve_buf(parsed.dst.uv_addr, handle_flag)?;
            (Some(p), Some(l), k)
        } else {
            (None, None, None)
        };

        let op = match parsed.into_operation(src_phys, src_uv_phys, dst_phys, dst_uv_phys) {
            Ok(o) => o,
            Err(e) => {
                warn!("RGA_BLIT into_operation FAIL {:?}", e);
                return Err(VfsError::InvalidInput);
            }
        };

        // RGA2 runs MMU-off, so every plane the engine addresses must stay inside the buffer it
        // was imported from — otherwise a small buffer with a large geometry would let the DMA
        // read/write adjacent physical memory. Reject before touching hardware.
        Self::check_bounds(
            &op,
            (src_phys, src_len),
            src_uv_len,
            (dst_phys, dst_len),
            dst_uv_len,
        )?;

        // QEMU path: no RGA2 device → ENODEV.
        let devs = rdrive::get_list::<RockchipRga>();
        if devs.is_empty() {
            return Err(VfsError::NoSuchDevice);
        }

        // The RK3588 DTB exposes three RGA cores as separate devices (rga3_core0 @
        // fdb60000, rga3_core1 @ fdb70000, rga2_core0 @ fdb80000). Only the RGA2
        // backend is implemented (RGA3 is an Unsupported skeleton). Lock the device
        // that actually owns the RGA2 core -- NOT blindly devs[0], which is an RGA3
        // core on this board and has no RGA2 core (-> NoSuchDevice, blit fails).
        let mut guard = devs
            .iter()
            .filter_map(|d| d.try_lock().ok())
            .find(|g| {
                g.cores()
                    .iter()
                    .any(|c| c.config().version == RgaVersion::Rga2)
            })
            .ok_or(VfsError::NoSuchDevice)?;
        let rga = &mut *guard;

        let core = rga
            .cores_mut()
            .iter_mut()
            .find(|c| c.config().version == RgaVersion::Rga2)
            .ok_or(VfsError::NoSuchDevice)?;

        // The shared `/dev/dma_heap` allocator (crate::file::dmabuf) hands out
        // DMA-COHERENT memory, so no explicit cache maintenance is needed around the
        // engine's DMA. We still hold the backing Arcs alive across submit + poll so a
        // concurrent RELEASE_BUFFER cannot free the pages out from under the engine.
        let _keep = (src_keep, src_uv_keep, dst_keep, dst_uv_keep);

        if let Err(e) = core.start(&op) {
            warn!("RGA_BLIT core.start failed: {:?}", e);
            return Err(VfsError::InvalidInput);
        }

        for _ in 0..500 {
            match core.poll_status() {
                RgaStatus::Done => {
                    core.finish();
                    return Ok(0);
                }
                RgaStatus::Error => {
                    let d = core.diag();
                    core.finish();
                    warn!(
                        "RGA_BLIT poll=Error int=0x{:08x} status=0x{:08x} cmd_ctrl=0x{:08x}",
                        d.int, d.status, d.cmd_ctrl
                    );
                    return Err(VfsError::Io);
                }
                RgaStatus::Busy => {
                    ax_runtime::hal::time::busy_wait(core::time::Duration::from_micros(100));
                }
            }
        }

        let d = core.diag();
        let _ = core.recover();
        warn!(
            "RGA_BLIT poll=Timeout int=0x{:08x} status=0x{:08x} cmd_ctrl=0x{:08x}",
            d.int, d.status, d.cmd_ctrl
        );
        Err(VfsError::TimedOut)
    }

    /// Bound-check every plane an operation will touch against the imported buffer it was
    /// resolved from. `src`/`dst` are `(base, len)` of the luma/RGB buffers; `*_uv_len` is the
    /// length of a *separately* imported chroma buffer (`None` when the chroma plane is derived
    /// inside the luma buffer, or absent).
    fn check_bounds(
        op: &RgaOperation,
        src: (u64, u64),
        src_uv_len: Option<u64>,
        dst: (u64, u64),
        dst_uv_len: Option<u64>,
    ) -> VfsResult<()> {
        match op {
            RgaOperation::Fill { dst: d, .. } => Self::check_desc(d, dst, dst_uv_len),
            RgaOperation::Copy { src: s, dst: d } => {
                Self::check_desc(s, src, src_uv_len)?;
                Self::check_desc(d, dst, dst_uv_len)
            }
            RgaOperation::Blit(b) => {
                Self::check_desc(&b.src, src, src_uv_len)?;
                Self::check_desc(&b.dst, dst, dst_uv_len)
            }
        }
    }

    /// Verify each plane of `desc` stays within its imported buffer. `buf` is the luma/RGB
    /// buffer `(base, len)`; `uv_sep_len` is the length of a separately imported chroma buffer.
    fn check_desc(desc: &ImageDesc, buf: (u64, u64), uv_sep_len: Option<u64>) -> VfsResult<()> {
        let ext = desc.plane_extents().map_err(|_| VfsError::InvalidInput)?;
        let (base, len) = buf;
        // The luma/RGB plane starts at the imported buffer base.
        if !Self::within(desc.phys_addr, ext.y, base, len) {
            warn!("RGA_BLIT: luma/RGB plane addresses past its imported buffer");
            return Err(VfsError::InvalidInput);
        }
        if let Some(uv_ext) = ext.uv {
            let uv_base = desc.uv_phys_addr.ok_or(VfsError::InvalidInput)?;
            // A derived chroma plane sits right after luma in the SAME buffer
            // (uv_base == base + y_extent); a separately imported one has its own length.
            let ok = if base.checked_add(ext.y) == Some(uv_base) {
                Self::within(uv_base, uv_ext, base, len)
            } else if let Some(uv_len) = uv_sep_len {
                Self::within(uv_base, uv_ext, uv_base, uv_len)
            } else {
                false
            };
            if !ok {
                warn!("RGA_BLIT: chroma plane addresses past its imported buffer");
                return Err(VfsError::InvalidInput);
            }
        }
        Ok(())
    }

    /// `[start, start + ext)` fully inside `[base, base + len)`, overflow-safe.
    fn within(start: u64, ext: u64, base: u64, len: u64) -> bool {
        start >= base
            && start
                .checked_sub(base)
                .and_then(|off| off.checked_add(ext))
                .is_some_and(|end| end <= len)
    }

    /// Handle `RGA_IOC_IMPORT_BUFFER`: resolve dma-buf fds → physical addresses and assign
    /// handles. Processes all `pool.size` entries; writes the assigned handle back to each.
    fn handle_import_buffer(&self, arg: usize) -> VfsResult<usize> {
        let pool: librga_abi::RgaBufferPool = unsafe {
            (arg as *const librga_abi::RgaBufferPool)
                .vm_read_uninit()?
                .assume_init()
        };

        if pool.size == 0 || pool.size > RGA_BUFFER_POOL_SIZE_MAX || pool.buffers_ptr == 0 {
            return Err(VfsError::InvalidInput);
        }

        let elem_size = core::mem::size_of::<librga_abi::RgaExternalBuffer>(); // 288
        let base = pool.buffers_ptr as usize;

        // For each element: insert the handle, then write it back to userspace and roll the
        // insertion back if that write faults, so a bad user pointer can't strand an unreachable
        // handle in the table for the fd's lifetime.
        for i in 0..pool.size as usize {
            let ptr = base + i * elem_size;
            let mut ext: librga_abi::RgaExternalBuffer = unsafe {
                (ptr as *const librga_abi::RgaExternalBuffer)
                    .vm_read_uninit()?
                    .assume_init()
            };

            let entry = match ext.r#type {
                librga_abi::RGA_DMA_BUFFER => {
                    let obj = resolve_contiguous_dmabuf(ext.memory as c_int)
                        .ok_or(VfsError::BadFileDescriptor)?;
                    ImportedBuf {
                        phys_addr: obj.phys_base() as u64,
                        len: obj.size() as u64,
                        obj: Some(obj),
                    }
                }
                librga_abi::RGA_PHYSICAL_ADDRESS => {
                    // A raw physical address bypasses all buffer bookkeeping: the RGA DMA
                    // engine (MMU-off) will read/write whatever physical page userspace names
                    // — kernel memory, another process's pages, anything. Gate it behind
                    // CAP_SYS_RAWIO (the capability Linux requires for /dev/mem-class raw I/O)
                    // so only privileged callers can use it; unprivileged code must import a
                    // dma-buf fd, whose physical range the kernel owns and can bound. The clean
                    // long-term fix is the dma-buf unification (see the follow-up design) which
                    // lets every buffer arrive as an fd and removes this path entirely.
                    if !ax_task::current().as_thread().cred().has_cap_sys_rawio() {
                        warn!(
                            "RGA_IOC_IMPORT_BUFFER: RGA_PHYSICAL_ADDRESS requires CAP_SYS_RAWIO; \
                             denied"
                        );
                        return Err(VfsError::OperationNotPermitted);
                    }
                    ImportedBuf {
                        phys_addr: ext.memory,
                        len: u64::MAX,
                        obj: None,
                    }
                }
                _ => return Err(VfsError::Unsupported),
            };

            let handle = self.alloc_handle(entry)?;
            ext.handle = handle;

            // Write-back must succeed for userspace to learn (and later release) the handle. A
            // fault here returns EFAULT and the process keeps running, so a stranded entry would
            // leak for the fd's lifetime — roll the just-inserted handle back before propagating.
            let res = (ptr as *mut librga_abi::RgaExternalBuffer).vm_write(ext);
            if res.is_err() {
                self.handle_table.lock().remove(&handle);
            }
            res?;
        }
        Ok(0)
    }

    /// Handle `RGA_IOC_RELEASE_BUFFER`: remove handles from the table, freeing the backing
    /// dma-buf references. An unknown handle fails with `ENOENT` (matching the Linux driver's
    /// `rga_mm_release_buffer`).
    fn handle_release_buffer(&self, arg: usize) -> VfsResult<usize> {
        let pool: librga_abi::RgaBufferPool = unsafe {
            (arg as *const librga_abi::RgaBufferPool)
                .vm_read_uninit()?
                .assume_init()
        };

        if pool.size == 0 || pool.size > RGA_BUFFER_POOL_SIZE_MAX || pool.buffers_ptr == 0 {
            return Err(VfsError::InvalidInput);
        }

        let elem_size = core::mem::size_of::<librga_abi::RgaExternalBuffer>();
        let base = pool.buffers_ptr as usize;
        let mut table = self.handle_table.lock();

        for i in 0..pool.size as usize {
            let ptr = base + i * elem_size;
            let ext: librga_abi::RgaExternalBuffer = unsafe {
                (ptr as *const librga_abi::RgaExternalBuffer)
                    .vm_read_uninit()?
                    .assume_init()
            };
            if table.remove(&ext.handle).is_none() {
                return Err(VfsError::NotFound);
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
        (arg as *mut librga_abi::RgaVersionT).vm_write(v)?;
        Ok(0)
    }

    /// `RGA_IOC_GET_HW_VERSION` — librga enumerates scheduler cores and classifies each by
    /// (major, minor, revision). The RK3588 RGA2 core's key is (3, 2, 0x63318) — librga's
    /// `rga_get_info()` maps exactly this to `RGA_2_ENHANCE` and grants YUYV_422 input + the
    /// CSC features (`im2d_impl.cpp`). Reporting revision 0 hit librga's `default:` branch
    /// (TRY_TO_COMPATIBLE) and aborted with "rga2 get info failed", rejecting every op.
    fn handle_get_hw_version(&self, arg: usize) -> VfsResult<usize> {
        let mut v0 = librga_abi::RgaVersionT {
            major: 3,
            minor: 2,
            revision: 0x63318,
            string: [0; 16],
        };
        v0.string[..8].copy_from_slice(b"3.2.0e63");
        let mut hw = librga_abi::RgaHwVersions {
            size: 1,
            ..Default::default()
        };
        hw.version[0] = v0;
        (arg as *mut librga_abi::RgaHwVersions).vm_write(hw)?;
        Ok(0)
    }

    /// `RGA_IOC_REQUEST_CREATE` — allocate a request id (written back to userspace). The
    /// created id must exist for CONFIG/SUBMIT/CANCEL to accept it.
    fn handle_request_create(&self, arg: usize) -> VfsResult<usize> {
        let mut next = self.next_request_id.lock();
        let mut requests = self.requests.lock();
        // Pick a non-zero id not already live for this open.
        let id = loop {
            let id = *next;
            *next = id.wrapping_add(1);
            if id != 0 && !requests.contains_key(&id) {
                break id;
            }
        };
        requests.insert(id, Vec::new());
        drop(requests);
        drop(next);
        // Roll the inserted id back if the write-back faults: a bad user pointer returns EFAULT
        // (the process keeps running) and must not strand a request the user never learns the id of.
        let res = (arg as *mut u32).vm_write(id);
        if res.is_err() {
            self.requests.lock().remove(&id);
        }
        res?;
        Ok(0)
    }

    /// Read the `task_num` `RgaReq` array a request points at (bounded by `RGA_TASK_NUM_MAX`).
    fn read_request_tasks(req: &librga_abi::RgaUserRequest) -> VfsResult<Vec<librga_abi::RgaReq>> {
        if req.task_num == 0 || req.task_num > RGA_TASK_NUM_MAX || req.task_ptr == 0 {
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

    /// `RGA_IOC_REQUEST_CONFIG` — stage a created request's tasks without running them.
    /// The request id must already exist (Linux returns `-EINVAL` otherwise).
    fn handle_request_config(&self, arg: usize) -> VfsResult<usize> {
        let ureq: librga_abi::RgaUserRequest = unsafe {
            (arg as *const librga_abi::RgaUserRequest)
                .vm_read_uninit()?
                .assume_init()
        };
        let tasks = Self::read_request_tasks(&ureq)?;
        let mut requests = self.requests.lock();
        // The id must have been created via REQUEST_CREATE.
        let slot = requests.get_mut(&ureq.id).ok_or(VfsError::InvalidInput)?;
        *slot = tasks;
        Ok(0)
    }

    /// `RGA_IOC_REQUEST_SUBMIT` — run a created request's tasks (carried inline, or previously
    /// staged via CONFIG) and block until each completes. We are always synchronous; async and
    /// fence modes are not implemented and are rejected explicitly.
    fn handle_request_submit(&self, arg: usize) -> VfsResult<usize> {
        let ureq: librga_abi::RgaUserRequest = unsafe {
            (arg as *const librga_abi::RgaUserRequest)
                .vm_read_uninit()?
                .assume_init()
        };
        // Only synchronous submission is implemented. `sync_mode == RGA_BLIT_ASYNC` is the
        // unambiguous async request (it is what carries the acquire/release fences); reject it
        // explicitly rather than silently running it synchronously. The fence fd fields are not
        // inspected directly: librga leaves them at 0 or -1 ("none") on a sync request, so
        // gating on them would wrongly reject valid sync blits.
        if ureq.sync_mode == librga_abi::RGA_BLIT_ASYNC {
            return Err(VfsError::Unsupported);
        }
        // Read inline tasks (if any) before claiming the request, so a faulting `task_ptr`
        // returns EFAULT without consuming the request id.
        let inline = if ureq.task_num > 0 {
            Some(Self::read_request_tasks(&ureq)?)
        } else {
            None
        };
        // Claim the request in one critical section: it must have been created, and a request
        // runs once. Removing under the lock serialises concurrent submits of the same id — a
        // losing racer sees `None` and gets EINVAL instead of running the tasks a second time.
        let staged = self
            .requests
            .lock()
            .remove(&ureq.id)
            .ok_or(VfsError::InvalidInput)?;
        // Inline tasks (common im2d single-blit) take precedence over a prior CONFIG's tasks.
        let tasks = inline.unwrap_or(staged);
        for task in &tasks {
            self.execute_blit(task)?;
        }
        Ok(0)
    }

    /// `RGA_IOC_REQUEST_CANCEL` — drop a created request. Cancelling an id that does not exist
    /// fails with `-EINVAL` (matching Linux), rather than silently succeeding.
    fn handle_request_cancel(&self, arg: usize) -> VfsResult<usize> {
        let id: u32 = unsafe { (arg as *const u32).vm_read_uninit()?.assume_init() };
        if self.requests.lock().remove(&id).is_none() {
            return Err(VfsError::InvalidInput);
        }
        Ok(0)
    }
}

impl FileLike for RgaFile {
    fn read(&self, dst: &mut IoDst) -> AxResult<usize> {
        self.base.read(dst)
    }

    fn write(&self, src: &mut IoSrc) -> AxResult<usize> {
        self.base.write(src)
    }

    fn stat(&self) -> AxResult<Kstat> {
        self.base.stat()
    }

    fn path(&self) -> Cow<'_, str> {
        self.base.path()
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> AxResult<usize> {
        if arg == 0 {
            return Err(VfsError::InvalidInput);
        }
        match cmd {
            librga_abi::RGA_BLIT_SYNC => self.handle_blit_sync(arg),
            librga_abi::RGA_BLIT_ASYNC => Err(VfsError::Unsupported),
            librga_abi::RGA_GET_VERSION => {
                // librga passes a 16-byte buffer (Linux writes back a char[16]).
                let mut version = [0u8; 16];
                version[..4].copy_from_slice(b"3.02");
                (arg as *mut [u8; 16]).vm_write(version)?;
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

    fn open_flags(&self) -> u32 {
        self.base.open_flags()
    }

    fn nonblocking(&self) -> bool {
        self.base.nonblocking()
    }

    fn set_nonblocking(&self, nonblocking: bool) -> AxResult {
        self.base.set_nonblocking(nonblocking)
    }
}

impl Pollable for RgaFile {
    /// The engine is driven synchronously inside `ioctl`, so the fd is always ready and never
    /// blocks.
    fn poll(&self) -> IoEvents {
        IoEvents::IN | IoEvents::OUT
    }

    fn register(&self, _context: &mut Context<'_>, _events: IoEvents) {}
}
