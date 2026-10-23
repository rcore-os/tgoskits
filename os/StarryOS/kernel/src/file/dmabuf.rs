//! A minimal contiguous dma-buf file backing the `/dev/dma_heap` allocator.
//!
//! Each [`DmaBufFile`] owns one physically-contiguous, DMA-coherent allocation
//! (via `dma-api`). It is handed to userspace as a file descriptor; `mmap` maps
//! the buffer's physical pages, and the physical base is what the
//! `/dev/mpp_service` node programs into the JPEG decoder. The allocation lives
//! in an inner `Arc` so that an active `mmap` keeps the pages alive even if the
//! fd is closed first; it is freed only when both the fd and every mmap drop.

use alloc::{borrow::Cow, boxed::Box, sync::Arc};
#[cfg(any(feature = "jpeg", feature = "rknpu", feature = "rga"))]
use core::ffi::c_int;
use core::{any::Any, ffi::c_void, mem::size_of, ptr};

use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr, PhysAddrRange};
use axpoll::{IoEvents, Pollable};
use dma_api::{CoherentArray, DmaError};
use kdrm_core::{
    drm_dma_buf_upstream::dma_buf_export,
    drm_prime_upstream::{DmaBuf, DmaBufAttachment, DmaBufExportInfo, DmaBufOps},
};
use drm_kapi::{
    config::{err_ptr, GFP_KERNEL, is_err},
    memory::{kfree, kmalloc},
    shmem::{self, File as LinuxKpiFile},
    types::{Scatterlist, SgTable},
};
use linux_raw_sys::general::O_RDWR;

use super::{FileLike, Kstat, dma_buf_seek};
use crate::{StarryError, StarryResult, pseudofs::DeviceMmap, sync::Mutex};

const DMA_BUF_MASK: u64 = u32::MAX as u64;

/// The owned contiguous allocation. Freed when the last reference (the fd's
/// `DmaBufFile` and any mmap retainer) drops.
struct DmaBufAlloc {
    dma: CoherentArray<u8>,
    size: usize,
}

/// A contiguous, DMA-coherent buffer exposed as a dma-buf file.
pub struct DmaBufFile {
    alloc: Arc<DmaBufAlloc>,
    /// Root LinuxKPI file reference for the common DRM DMA-BUF identity.
    /// The exported private owner retains the allocation, never this wrapper,
    /// so the VFS fd and the DMA-BUF file do not form an ownership cycle.
    drm_file: Mutex<usize>,
}

impl DmaBufFile {
    /// Allocate a page-aligned contiguous buffer of at least `len` bytes.
    pub fn alloc(len: usize) -> StarryResult<Self> {
        Self::alloc_with_device(
            len,
            &axklib::dma::device(dma_api::DmaDeviceInfo::new(
                dma_api::DmaDomainId::Direct,
                dma_api::DmaCoherency::NonCoherent,
                dma_api::DmaConstraints::new(DMA_BUF_MASK),
            )),
        )
    }

    fn alloc_with_device(len: usize, dma: &dma_api::DeviceDma) -> StarryResult<Self> {
        let align = PAGE_SIZE_4K;
        let size = len
            .checked_next_multiple_of(align)
            .ok_or(StarryError::InvalidInput)?
            .max(align);
        // The accelerators that consume these buffers (JPU/RGA/NPU) run with the
        // IOMMU bypassed and program raw 32-bit physical DMA addresses, so the
        // backing pages must live below 4 GiB. Plain `alloc_coherent_pages` draws
        // from anywhere in RAM and returns >4 GiB pages on large-memory boards,
        // which the 32-bit address registers cannot reach.
        let dma = dma
            .coherent_array_zero_with_align::<u8>(size, align)
            .map_err(|err| match err {
                DmaError::LayoutError(_) => StarryError::InvalidInput,
                _ => StarryError::NoMemory,
            })?;
        Ok(Self {
            alloc: Arc::new(DmaBufAlloc { dma, size }),
            drm_file: Mutex::new(0),
        })
    }

    /// Returns a new reference to the shared LinuxKPI DMA-BUF file.
    ///
    /// The cached reference is one per Starry DmaBufFile object, so repeated
    /// PRIME imports of the same heap fd see the same dma_buf identity.
    pub(crate) fn get_drm_file(&self) -> *mut LinuxKpiFile {
        let mut cached = self.drm_file.lock();
        if *cached == 0 {
            let Ok(owner) = Box::try_new(StarryDmaBufExport {
                alloc: self.alloc.clone(),
            }) else {
                return ptr::null_mut();
            };
            let owner = Box::into_raw(owner);
            let info = DmaBufExportInfo {
                exp_name: c"starry-dma-heap".as_ptr(),
                owner: ptr::null_mut(),
                ops: &STARRY_DMABUF_OPS,
                size: self.alloc.size,
                flags: 0,
                resv: ptr::null_mut(),
                r#priv: owner.cast::<c_void>(),
            };
            // SAFETY: `info` points to a stack-allocated `DmaBufExportInfo` whose fields and ops are initialized and live.
            let dma_buf = unsafe { dma_buf_export(&info) };
            if dma_buf.is_null() || is_err(dma_buf) {
                // Export did not take ownership unless it returned a live
                // dma_buf; its release callback therefore has not run.
                // SAFETY: `dma_buf_export` failed and did not take ownership; restoring `Box` frees the allocated owner.
                unsafe { drop(Box::from_raw(owner)) };
                return ptr::null_mut();
            }
            // SAFETY: `dma_buf` is a valid, live `DmaBuf` pointer returned by `dma_buf_export`.
            let file = unsafe { (*dma_buf).file.cast::<LinuxKpiFile>() };
            if file.is_null() || is_err(file) {
                // SAFETY: `dma_buf` is valid; drops our reference since `file` extraction failed.
                unsafe { kdrm_core::drm_dma_buf_upstream::dma_buf_put(dma_buf) };
                return ptr::null_mut();
            }
            *cached = file as usize;
        }
        // SAFETY: `cached` stores a valid, non-null pointer to a live `LinuxKpiFile`; increments file reference count.
        unsafe { shmem::get_file_file(*cached as *mut LinuxKpiFile) }
    }

    /// Physical address range of the buffer.
    pub fn phys_range(&self) -> PhysAddrRange {
        PhysAddrRange::from_start_size(
            PhysAddr::from(self.alloc.dma.dma_addr().as_u64() as usize),
            self.alloc.size,
        )
    }

    /// Physical base address.
    #[cfg(any(
        feature = "jpeg",
        feature = "rknpu",
        feature = "rga",
        all(test, not(axtest))
    ))]
    pub fn phys_base(&self) -> usize {
        self.alloc.dma.dma_addr().as_u64() as usize
    }

    /// Size of the allocation in bytes (page-rounded up from the request).
    ///
    /// Accelerator import paths use this to bound-check every imported buffer before
    /// an MMU-off DMA (a plane must not address past its buffer).
    #[cfg(any(feature = "jpeg", feature = "rknpu", feature = "rga"))]
    pub fn size(&self) -> usize {
        self.alloc.size
    }
}

struct StarryDmaBufExport {
    alloc: Arc<DmaBufAlloc>,
}

static STARRY_DMABUF_OPS: DmaBufOps = DmaBufOps {
    attach: None,
    detach: None,
    pin: None,
    unpin: None,
    map_dma_buf: Some(starry_dmabuf_map),
    unmap_dma_buf: Some(starry_dmabuf_unmap),
    release: Some(starry_dmabuf_release),
    // CoherentArray uses the DMA API coherent-allocation contract. No
    // streaming synchronization is required for this exporter.
    begin_cpu_access: None,
    end_cpu_access: None,
    mmap: None,
    vmap: None,
    vunmap: None,
};

/// # Safety
/// If non-null, `attachment` must point to a live `DmaBufAttachment` initialized by the DMA-BUF subsystem.
unsafe extern "C" fn starry_dmabuf_map(
    attachment: *mut DmaBufAttachment,
    _direction: i32,
) -> *mut SgTable {
    if attachment.is_null() {
        return err_ptr(-drm_kapi::config::EINVAL);
    }
    // SAFETY: dma_buf_map_attachment invokes this callback with a live
    // DmaBufAttachment owned by the DMA-BUF core.
    let dma_buf = unsafe { (*attachment).dmabuf };
    // SAFETY: `dma_buf` is non-null and valid, accessing `r#priv` field.
    if dma_buf.is_null() || unsafe { (*dma_buf).r#priv.is_null() } {
        return err_ptr(-drm_kapi::config::EINVAL);
    }
    // SAFETY: `r#priv` was verified non-null and points to the live `StarryDmaBufExport` installed during export.
    let owner = unsafe { &*(*dma_buf).r#priv.cast::<StarryDmaBufExport>() };
    let Ok(length) = u32::try_from(owner.alloc.size) else {
        return err_ptr(-drm_kapi::config::EOPNOTSUPP);
    };
    let Some(total) = size_of::<SgTable>().checked_add(size_of::<Scatterlist>()) else {
        return err_ptr(-drm_kapi::config::ENOMEM);
    };
    let allocation = kmalloc(total, GFP_KERNEL);
    if allocation.is_null() {
        return err_ptr(-drm_kapi::config::ENOMEM);
    }
    let table = allocation.cast::<SgTable>();
    // SAFETY: `allocation` was checked non-null with size >= size_of::<SgTable>() + size_of::<Scatterlist>();
    // pointer arithmetic stays strictly within the allocated bounds.
    let scatterlist = unsafe {
        allocation
            .cast::<u8>()
            .add(size_of::<SgTable>())
            .cast::<Scatterlist>()
    };
    let dma_address = owner.alloc.dma.dma_addr().as_u64();
    // SAFETY: `table` and `scatterlist` point to properly aligned, non-overlapping allocated memory within `allocation`.
    unsafe {
        table.write(SgTable {
            sgl: scatterlist,
            nents: 1,
            orig_nents: 1,
        });
        scatterlist.write(Scatterlist {
            // This exporter is one physically-contiguous DMA segment.
            // Importers must consume DMA-address semantics, not page_link.
            page_link: 0,
            offset: 0,
            length,
            dma_address,
            dma_length: length,
            _dma_flags_or_padding: 0,
        });
    }
    table
}

/// # Safety
/// If non-null, `table` must point to an `SgTable` previously returned by `starry_dmabuf_map`.
unsafe extern "C" fn starry_dmabuf_unmap(
    _attachment: *mut kdrm_core::drm_prime_upstream::DmaBufAttachment,
    table: *mut SgTable,
    _direction: i32,
) {
    if !table.is_null() && !is_err(table) {
        // SAFETY: `table` was allocated via `kmalloc` in `starry_dmabuf_map` and has not been freed.
        unsafe { kfree(table.cast::<c_void>()) };
    }
}

/// # Safety
/// If non-null, `dma_buf` must point to a live `DmaBuf` whose exporter is `STARRY_DMABUF_OPS`.
unsafe extern "C" fn starry_dmabuf_release(dma_buf: *mut DmaBuf) {
    // SAFETY: `dma_buf` is verified non-null before dereferencing `r#priv`.
    if dma_buf.is_null() || unsafe { (*dma_buf).r#priv.is_null() } {
        return;
    }
    // SAFETY: `r#priv` is non-null and was created via `Box::into_raw` during `dma_buf_export`.
    let owner = unsafe { Box::from_raw((*dma_buf).r#priv.cast::<StarryDmaBufExport>()) };
    // SAFETY: `dma_buf` is valid; clears private pointer to prevent double-free.
    unsafe { (*dma_buf).r#priv = ptr::null_mut() };
    drop(owner);
}

impl Drop for DmaBufFile {
    fn drop(&mut self) {
        // Drop already has exclusive access, so do not acquire the sleepable
        // mutex here. A buffer that was never PRIME-exported may be destroyed
        // before the task runtime (and its mutex provider) is initialized.
        let raw = core::mem::replace(self.drm_file.get_mut(), 0);
        if raw != 0 {
            // SAFETY: `raw` was stored from a valid `LinuxKpiFile` reference and is dropped here.
            unsafe { shmem::fput_file(raw as *mut LinuxKpiFile) };
        }
    }
}

/// A physically-contiguous, device-reachable DMA buffer that the accelerator
/// dev-nodes (JPU / RGA / NPU) can share by fd for zero-copy. The RK3588 engines
/// run IOMMU-bypassed, so the physical base is exactly what they program into
/// their address registers.
///
/// Consumed by the NPU import path (card1 `PRIME_FD_TO_HANDLE`), which needs the
/// CPU base and a lifetime retainer; the jpeg-only `/dev/mpp_service` path uses
/// the inherent [`DmaBufFile::phys_base`] instead, so this is gated on `rknpu`.
#[cfg(feature = "rknpu")]
pub trait ContiguousDmaBuf {
    /// Device-reachable physical/bus base address.
    fn dma_phys_base(&self) -> usize;
    /// Allocation length in bytes.
    fn dma_size(&self) -> usize;
    /// Kernel CPU virtual base, if the buffer is CPU-mapped (the coherent heap is).
    fn dma_cpu_base(&self) -> Option<usize>;
    /// A type-erased owner whose lifetime keeps the pages alive; an importer
    /// stores it so the buffer cannot be freed while another engine references it.
    fn dma_retainer(&self) -> Arc<dyn Any + Send + Sync>;
}

#[cfg(feature = "rknpu")]
impl ContiguousDmaBuf for DmaBufFile {
    fn dma_phys_base(&self) -> usize {
        self.phys_base()
    }

    fn dma_size(&self) -> usize {
        self.size()
    }

    fn dma_cpu_base(&self) -> Option<usize> {
        Some(self.alloc.dma.as_ptr().as_ptr() as usize)
    }

    fn dma_retainer(&self) -> Arc<dyn Any + Send + Sync> {
        self.alloc.clone()
    }
}

/// Resolve a userspace dma-buf fd to its backing contiguous allocation. Returns
/// `None` if the fd is not one of our shareable contiguous buffers (e.g. a
/// socket, pipe, or regular file) — callers reject with `EINVAL`.
///
/// This is the single seam every accelerator node uses to turn an fd into a
/// physical address, so JPU / RGA / NPU all resolve shared buffers identically.
#[cfg(any(feature = "jpeg", feature = "rknpu", feature = "rga"))]
pub fn resolve_contiguous_dmabuf(fd: c_int) -> Option<Arc<DmaBufFile>> {
    let file = super::get_file_like(fd).ok()?;
    file.downcast_arc::<DmaBufFile>().ok()
}

impl Pollable for DmaBufFile {
    fn poll(&self) -> IoEvents {
        IoEvents::IN | IoEvents::OUT
    }

    /// # Safety
    /// Implementation is a no-op; caller must provide a valid sink reference.
    unsafe fn register_shared(
        &self,
        _sink: &mut dyn axpoll::SharedRegistrationSink,
        _events: IoEvents,
    ) {
    }
}

impl FileLike for DmaBufFile {
    fn seek(&self, pos: ax_io::SeekFrom) -> StarryResult<u64> {
        dma_buf_seek(self.alloc.size as u64, pos)
    }

    fn validate_write_access(&self) -> StarryResult {
        Err(StarryError::InvalidInput)
    }

    fn stat(&self) -> StarryResult<Kstat> {
        Ok(Kstat {
            size: self.alloc.size as u64,
            ..Default::default()
        })
    }

    fn path(&self) -> Cow<'_, str> {
        Cow::Borrowed("/dev/dma_heap_buffer")
    }

    /// The buffer is read-write: `librockchip_mpp` gates `mmap` `PROT_WRITE` on
    /// `fcntl(fd, F_GETFL) & O_RDWR`, and it writes the stream and table buffers
    /// through the mapping, so the dma-buf fd must report read-write access.
    fn open_flags(&self) -> u32 {
        O_RDWR
    }

    fn device_mmap(&self, _offset: u64, _length: u64) -> StarryResult<DeviceMmap> {
        // Retain the allocation for the lifetime of the mapping so the pages are
        // not freed if userspace closes the fd while it is still mapped.
        let retainer: Arc<dyn Any + Send + Sync> = self.alloc.clone();
        Ok(DeviceMmap::Physical(self.phys_range(), Some(retainer)))
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    extern crate std;

    use core::{
        alloc::Layout,
        num::NonZeroUsize,
        ptr::NonNull,
        sync::atomic::{AtomicU64, AtomicUsize, Ordering},
    };

    use dma_api::{
        DeviceDma, DmaAllocHandle, DmaConstraints, DmaDirection, DmaError, DmaMapHandle, DmaOp,
    };

    use self::std::alloc::{alloc_zeroed, dealloc};
    use super::*;

    struct TestDma;

    static TEST_DMA: TestDma = TestDma;
    static ALLOC_MASK: AtomicU64 = AtomicU64::new(0);
    static RELEASES: AtomicUsize = AtomicUsize::new(0);

    impl DmaOp for TestDma {
        fn page_size(&self) -> usize {
            PAGE_SIZE_4K
        }

        /// # Safety
        /// Test stub implementation.
        unsafe fn alloc_contiguous(
            &self,
            _constraints: DmaConstraints,
            _layout: Layout,
        ) -> Option<DmaAllocHandle> {
            None
        }

        /// # Safety
        /// Test stub implementation.
        unsafe fn dealloc_contiguous(&self, _handle: DmaAllocHandle) {}

        /// # Safety
        /// Caller must ensure `layout` has non-zero size.
        unsafe fn alloc_coherent(
            &self,
            constraints: DmaConstraints,
            layout: Layout,
        ) -> Option<DmaAllocHandle> {
            ALLOC_MASK.store(constraints.addr_mask, Ordering::SeqCst);
            // SAFETY: `layout` has non-zero size per test configuration.
            let ptr = NonNull::new(unsafe { alloc_zeroed(layout) })?;
            // SAFETY: `ptr` is non-null and points to `layout`-sized zeroed memory.
            Some(unsafe { DmaAllocHandle::new(ptr, ptr, 0x2000_u64.into(), layout) })
        }

        /// # Safety
        /// `handle` must be an allocation returned by `alloc_coherent`.
        unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) -> Result<(), DmaError> {
            RELEASES.fetch_add(1, Ordering::SeqCst);
            // SAFETY: `handle` was allocated with `alloc_zeroed(layout)` and is freed with matching layout.
            unsafe { dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
            Ok(())
        }

        /// # Safety
        /// Test stub implementation.
        unsafe fn map_streaming(
            &self,
            _constraints: DmaConstraints,
            _addr: NonNull<u8>,
            _size: NonZeroUsize,
            _direction: DmaDirection,
        ) -> Result<DmaMapHandle, DmaError> {
            Err(DmaError::NoMemory)
        }

        /// # Safety
        /// Test stub implementation.
        unsafe fn unmap_streaming(&self, _handle: DmaMapHandle) {}
    }

    #[test]
    fn dma_buf_preserves_dma32_size_address_and_arc_lifetime() {
        RELEASES.store(0, Ordering::SeqCst);
        ALLOC_MASK.store(0, Ordering::SeqCst);
        let device = DeviceDma::new(
            dma_api::DmaDeviceInfo::new(
                dma_api::DmaDomainId::Direct,
                dma_api::DmaCoherency::NonCoherent,
                dma_api::DmaConstraints::new(DMA_BUF_MASK),
            ),
            &TEST_DMA,
        );
        let file = DmaBufFile::alloc_with_device(1, &device).unwrap();

        assert_eq!(file.alloc.size, PAGE_SIZE_4K);
        assert_eq!(file.phys_base(), 0x2000);
        assert_eq!(ALLOC_MASK.load(Ordering::SeqCst), DMA_BUF_MASK);
        assert_eq!(file.seek(ax_io::SeekFrom::Start(0)).unwrap(), 0);
        assert_eq!(
            file.seek(ax_io::SeekFrom::End(0)).unwrap(),
            PAGE_SIZE_4K as u64
        );
        assert!(file.seek(ax_io::SeekFrom::Current(0)).is_err());

        let mmap_owner = file.alloc.clone();
        drop(file);
        assert_eq!(RELEASES.load(Ordering::SeqCst), 0);
        drop(mmap_owner);
        assert_eq!(RELEASES.load(Ordering::SeqCst), 1);
    }
}
