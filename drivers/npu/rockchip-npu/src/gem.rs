use alloc::{collections::btree_map::BTreeMap, sync::Arc};
use core::{
    any::Any,
    sync::atomic::{AtomicUsize, Ordering},
};

use dma_api::{ContiguousArray, DeviceDma, DmaDirection};

use crate::{
    RknpuError,
    ioctrl::{RknpuMemCreate, RknpuMemSync},
};

const RKNPU_MEM_CACHEABLE: u32 = 1 << 1;
const RKNPU_MEM_WRITE_COMBINE: u32 = 1 << 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GemCachePolicy {
    NonCacheable,
    Cacheable,
    WriteCombine,
}

impl GemCachePolicy {
    fn from_flags(flags: u32) -> Self {
        if flags & RKNPU_MEM_CACHEABLE != 0 {
            Self::Cacheable
        } else if flags & RKNPU_MEM_WRITE_COMBINE != 0 {
            Self::WriteCombine
        } else {
            Self::NonCacheable
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GemBufferInfo {
    pub obj_addr: usize,
    pub dma_addr: u64,
    pub size: usize,
    pub flags: u32,
    pub cache_policy: GemCachePolicy,
}

// These are resource policy limits, not RK3588 hardware limits. Charge full
// pages, and leave room for other users of the shared contiguous DMA allocator.
const MAX_ALLOCATION_BYTES: usize = 64 * 1024 * 1024;
const MAX_OWNER_BYTES: usize = 256 * 1024 * 1024;
const MAX_OWNER_OBJECTS: usize = 1024;
const MAX_DEVICE_BYTES: usize = 512 * 1024 * 1024;
const MAX_DEVICE_OBJECTS: usize = 4096;

/// Allocation account for one open file description. Share this account across
/// dup/fork; create a new account for each independent open. Outstanding GEM
/// backing retains the account even after its file or handle has been closed.
/// Imported buffers retain their exporter's allocation account unchanged.
pub struct GemOwner {
    usage: Arc<GemUsage>,
}

impl Default for GemOwner {
    fn default() -> Self {
        Self {
            usage: Arc::new(GemUsage::new(MAX_OWNER_BYTES, MAX_OWNER_OBJECTS)),
        }
    }
}

struct GemUsage {
    bytes: AtomicUsize,
    objects: AtomicUsize,
    max_bytes: usize,
    max_objects: usize,
}

impl GemUsage {
    fn new(max_bytes: usize, max_objects: usize) -> Self {
        Self {
            bytes: AtomicUsize::new(0),
            objects: AtomicUsize::new(0),
            max_bytes,
            max_objects,
        }
    }

    fn reserve(self: &Arc<Self>, bytes: usize) -> Result<GemCharge, RknpuError> {
        // These atomics only account resources; Arc and the pool's exclusive
        // borrow publish buffers. Reserve each independent limit before DMA
        // allocation. A partial reservation is conservative and rolls back on
        // failure; it can never admit usage beyond either limit.
        self.objects
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                used.checked_add(1).filter(|next| *next <= self.max_objects)
            })
            .map_err(|_| RknpuError::OutOfMemory)?;
        if self
            .bytes
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                used.checked_add(bytes)
                    .filter(|next| *next <= self.max_bytes)
            })
            .is_err()
        {
            self.objects.fetch_sub(1, Ordering::Relaxed);
            return Err(RknpuError::OutOfMemory);
        }
        Ok(GemCharge {
            usage: self.clone(),
            bytes,
        })
    }
}

struct GemCharge {
    usage: Arc<GemUsage>,
    bytes: usize,
}

impl Drop for GemCharge {
    fn drop(&mut self) {
        self.usage.bytes.fetch_sub(self.bytes, Ordering::Relaxed);
        self.usage.objects.fetch_sub(1, Ordering::Relaxed);
    }
}

struct OwnedGem {
    // Declaration order matters: release the DMA allocation before making its
    // quota available again. Mappings and PRIME exports retain this entire
    // object, so removing a handle cannot hide still-pinned DMA memory.
    data: ContiguousArray<u8>,
    _owner_charge: GemCharge,
    _device_charge: GemCharge,
}

impl core::ops::Deref for OwnedGem {
    type Target = ContiguousArray<u8>;

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

/// An externally-owned buffer imported by dma-buf fd (e.g. from `/dev/dma_heap`,
/// the same buffer a vendor lib hands across engines for zero-copy). The NPU runs
/// IOMMU-bypassed, so `dma_addr` is the device-reachable physical base; `obj_addr`
/// is the exporter's kernel CPU virtual base (for `mmap`). `retainer` keeps the
/// exporting allocation alive for as long as this handle exists (UAF guard).
struct ImportedBuffer {
    obj_addr: usize,
    dma_addr: u64,
    size: usize,
    flags: u32,
    retainer: Arc<dyn Any + Send + Sync>,
}

/// A GEM handle's backing: either allocated by this pool, or imported by fd.
///
/// Owned allocations live behind an `Arc` so a live mapping (card1 `mmap` /
/// PRIME export) can pin them past a `destroy`, avoiding a use-after-free.
enum GemBuffer {
    Owned { data: Arc<OwnedGem>, flags: u32 },
    Imported(ImportedBuffer),
}

pub struct GemPool {
    dma: DeviceDma,
    pool: BTreeMap<u32, GemBuffer>,
    handle_counter: u32,
    usage: Arc<GemUsage>,
}

impl GemPool {
    pub fn new(dma: DeviceDma) -> Self {
        GemPool {
            dma,
            pool: BTreeMap::new(),
            handle_counter: 1,
            usage: Arc::new(GemUsage::new(MAX_DEVICE_BYTES, MAX_DEVICE_OBJECTS)),
        }
    }

    fn next_handle(&mut self) -> u32 {
        let handle = self.handle_counter;
        self.handle_counter = self.handle_counter.wrapping_add(1);
        handle
    }

    /// Allocate page-rounded DMA backing charged to the file and device until
    /// the last handle, mapping, or PRIME retainer drops. Zero/overflowing sizes
    /// are invalid; allocation and quota exhaustion return `OutOfMemory`.
    pub fn create(
        &mut self,
        owner: &GemOwner,
        args: &mut RknpuMemCreate,
    ) -> Result<(), RknpuError> {
        let requested_size =
            usize::try_from(args.size).map_err(|_| RknpuError::InvalidParameter)?;
        // Owned GEMs are exposed through mmap as device mappings. Allocate and
        // zero the complete final page so that mmap never publishes bytes past
        // the initialized DMA backing. Imported buffers keep their exact size
        // and are capped to complete pages by the mmap path.
        let allocation_size = page_align_size(requested_size, self.dma.page_size())?;
        if allocation_size == 0 {
            return Err(RknpuError::InvalidParameter);
        }
        if allocation_size > MAX_ALLOCATION_BYTES {
            return Err(RknpuError::OutOfMemory);
        }
        let owner_charge = owner.usage.reserve(allocation_size)?;
        let device_charge = self.usage.reserve(allocation_size)?;
        let data = self
            .dma
            .contiguous_array_zero_with_align::<u8>(
                allocation_size,
                0x1000,
                DmaDirection::Bidirectional,
            )
            .map_err(|error| match error {
                dma_api::DmaError::NoMemory => RknpuError::OutOfMemory,
                _ => RknpuError::DmaError,
            })?;

        let handle = self.next_handle();

        args.handle = handle;
        args.sram_size = data.len() as _;
        args.dma_addr = data.dma_addr().as_u64();
        args.obj_addr = data.as_ptr().as_ptr() as _;
        self.pool.insert(
            args.handle,
            GemBuffer::Owned {
                data: Arc::new(OwnedGem {
                    data,
                    _owner_charge: owner_charge,
                    _device_charge: device_charge,
                }),
                flags: args.flags,
            },
        );
        Ok(())
    }

    /// Register an externally-owned, physically-contiguous buffer (imported by
    /// dma-buf fd) as a GEM handle, so the existing `MemMap`/`mmap`/submit
    /// resolution chain (all of which funnel through [`Self::get_buffer_info`])
    /// works for it. `retainer` is held until the handle is destroyed, keeping
    /// the exporter's pages alive. Returns the new handle.
    pub fn import(
        &mut self,
        dma_addr: u64,
        obj_addr: usize,
        size: usize,
        flags: u32,
        retainer: Arc<dyn Any + Send + Sync>,
    ) -> u32 {
        let handle = self.next_handle();
        self.pool.insert(
            handle,
            GemBuffer::Imported(ImportedBuffer {
                obj_addr,
                dma_addr,
                size,
                flags,
                retainer,
            }),
        );
        handle
    }

    pub fn get_buffer_info(&self, handle: u32) -> Option<GemBufferInfo> {
        self.pool.get(&handle).map(|buffer| match buffer {
            GemBuffer::Owned { data, flags } => GemBufferInfo {
                obj_addr: data.as_ptr().as_ptr() as usize,
                dma_addr: data.dma_addr().as_u64(),
                size: data.len(),
                flags: *flags,
                cache_policy: GemCachePolicy::from_flags(*flags),
            },
            GemBuffer::Imported(buffer) => GemBufferInfo {
                obj_addr: buffer.obj_addr,
                dma_addr: buffer.dma_addr,
                size: buffer.size,
                flags: buffer.flags,
                cache_policy: GemCachePolicy::from_flags(buffer.flags),
            },
        })
    }

    /// A lifetime retainer for the buffer backing `handle`. Cloning the returned
    /// `Arc` keeps the physical pages alive independent of the pool, so a mapping
    /// (card1 `mmap` / PRIME export) can outlive a `destroy` without dangling.
    pub fn buffer_retainer(&self, handle: u32) -> Option<Arc<dyn Any + Send + Sync>> {
        self.pool.get(&handle).map(|buffer| match buffer {
            GemBuffer::Owned { data, .. } => data.clone() as Arc<dyn Any + Send + Sync>,
            GemBuffer::Imported(buffer) => buffer.retainer.clone(),
        })
    }

    /// Get the physical address and size of the memory object.
    pub fn get_phys_addr_and_size(&self, handle: u32) -> Option<(u64, usize)> {
        self.get_buffer_info(handle)
            .map(|info| (info.dma_addr, info.size))
    }

    /// Get the CPU-visible virtual address and size of the memory object.
    pub fn get_obj_addr_and_size(&self, handle: u32) -> Option<(usize, usize)> {
        self.get_buffer_info(handle)
            .map(|info| (info.obj_addr, info.size))
    }

    pub fn sync(&mut self, args: &mut RknpuMemSync) -> Result<(), RknpuError> {
        const RKNPU_MEM_SYNC_TO_DEVICE: u32 = 1 << 0;
        const RKNPU_MEM_SYNC_FROM_DEVICE: u32 = 1 << 1;

        // Locate the buffer whose CPU range covers `args.obj_addr`.
        for buffer in self.pool.values_mut() {
            match buffer {
                GemBuffer::Owned { data, .. } => {
                    let base = data.as_ptr().as_ptr() as u64;
                    let Some(end) = base.checked_add(data.bytes_len() as u64) else {
                        continue;
                    };
                    if args.obj_addr < base || args.obj_addr >= end {
                        continue;
                    }
                    let base_offset = args.obj_addr - base;
                    let offset = usize::try_from(args.offset.saturating_add(base_offset))
                        .map_err(|_| RknpuError::InvalidParameter)?;
                    let requested_size =
                        usize::try_from(args.size).map_err(|_| RknpuError::InvalidParameter)?;
                    let size = if requested_size == 0 {
                        data.bytes_len().saturating_sub(offset)
                    } else {
                        requested_size
                    };

                    if offset > data.bytes_len() || size > data.bytes_len().saturating_sub(offset) {
                        return Err(RknpuError::InvalidParameter);
                    }

                    if args.flags & RKNPU_MEM_SYNC_TO_DEVICE != 0 {
                        data.prepare_for_device(offset..offset + size);
                    }
                    if args.flags & RKNPU_MEM_SYNC_FROM_DEVICE != 0 {
                        data.complete_for_cpu(offset..offset + size);
                    }
                    return Ok(());
                }
                GemBuffer::Imported(buffer) => {
                    let base = buffer.obj_addr as u64;
                    let Some(end) = base.checked_add(buffer.size as u64) else {
                        continue;
                    };
                    if args.obj_addr < base || args.obj_addr >= end {
                        continue;
                    }
                    // Imported buffers come from the coherent dma-heap, so there
                    // is nothing to flush/invalidate — sync is a no-op.
                    return Ok(());
                }
            }
        }

        Err(RknpuError::InvalidHandle)
    }

    pub fn destroy(&mut self, handle: u32) {
        self.pool.remove(&handle);
    }

    pub fn comfirm_write_all(&mut self) -> Result<(), RknpuError> {
        for buffer in self.pool.values_mut() {
            if let GemBuffer::Owned { data, .. } = buffer {
                data.prepare_for_device(0..data.bytes_len());
            }
        }
        Ok(())
    }

    pub fn prepare_read_all(&mut self) -> Result<(), RknpuError> {
        for buffer in self.pool.values_mut() {
            if let GemBuffer::Owned { data, .. } = buffer {
                data.complete_for_cpu(0..data.bytes_len());
            }
        }
        Ok(())
    }
}

fn page_align_size(size: usize, page_size: usize) -> Result<usize, RknpuError> {
    if page_size == 0 || !page_size.is_power_of_two() {
        return Err(RknpuError::InvalidParameter);
    }
    size.checked_add(page_size - 1)
        .map(|size| size & !(page_size - 1))
        .ok_or(RknpuError::InvalidParameter)
}

#[cfg(test)]
mod tests {
    use core::{
        alloc::Layout,
        num::NonZeroUsize,
        ptr::NonNull,
        sync::atomic::{AtomicBool, Ordering},
    };

    use dma_api::{DmaAllocHandle, DmaConstraints, DmaError, DmaMapHandle, DmaOp};

    use super::*;

    /// A device-DMA backend that must never allocate. The imported-buffer path
    /// stores a caller-supplied retainer and never touches the allocator, so any
    /// call here means the code under test regressed into allocating — a loud
    /// failure is exactly what we want.
    struct NoAllocOp;

    impl DmaOp for NoAllocOp {
        fn page_size(&self) -> usize {
            4096
        }
        unsafe fn alloc_contiguous(&self, _: DmaConstraints, _: Layout) -> Option<DmaAllocHandle> {
            panic!("imported-buffer path must not allocate")
        }
        unsafe fn dealloc_contiguous(&self, _: DmaAllocHandle) {
            panic!("imported-buffer path must not deallocate")
        }
        unsafe fn alloc_coherent(&self, _: DmaConstraints, _: Layout) -> Option<DmaAllocHandle> {
            panic!("imported-buffer path must not allocate")
        }
        unsafe fn dealloc_coherent(&self, _: DmaAllocHandle) -> Result<(), DmaError> {
            panic!("imported-buffer path must not deallocate")
        }
        unsafe fn map_streaming(
            &self,
            _: DmaConstraints,
            _: NonNull<u8>,
            _: NonZeroUsize,
            _: DmaDirection,
        ) -> Result<DmaMapHandle, DmaError> {
            panic!("imported-buffer path must not map streaming")
        }
        unsafe fn unmap_streaming(&self, _: DmaMapHandle) {
            panic!("imported-buffer path must not unmap streaming")
        }
    }

    fn import_only_pool() -> GemPool {
        static OP: NoAllocOp = NoAllocOp;
        GemPool::new(DeviceDma::new(
            dma_api::DmaDeviceInfo::new(
                dma_api::DmaDomainId::Direct,
                dma_api::DmaCoherency::NonCoherent,
                dma_api::DmaConstraints::new(u32::MAX as u64),
            ),
            &OP,
        ))
    }

    /// A retainer whose drop is observable, standing in for an exporter's backing
    /// allocation (e.g. a `/dev/dma_heap` buffer shared into the NPU).
    struct DropSpy(Arc<AtomicBool>);

    impl Drop for DropSpy {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn imported_retainer_pins_backing_until_last_drop() {
        let mut pool = import_only_pool();
        let freed = Arc::new(AtomicBool::new(false));
        let retainer: Arc<dyn Any + Send + Sync> = Arc::new(DropSpy(freed.clone()));

        let handle = pool.import(0x4000_0000, 0xffff_0000, 0x1000, 0, retainer);

        // A mapping (card1 mmap / PRIME export) clones the retainer as its anchor.
        let anchor = pool
            .buffer_retainer(handle)
            .expect("imported handle must expose a retainer");

        // Destroying the handle drops the pool's reference, but the live anchor
        // must keep the exporter's backing alive — this is the use-after-free guard.
        pool.destroy(handle);
        assert!(
            !freed.load(Ordering::SeqCst),
            "backing freed while a mapping anchor is still live (use-after-free)"
        );
        assert!(
            pool.buffer_retainer(handle).is_none(),
            "a destroyed handle must no longer resolve"
        );

        // The backing is freed only once the last reference (the anchor) is gone.
        drop(anchor);
        assert!(
            freed.load(Ordering::SeqCst),
            "backing not freed after the last retainer dropped"
        );
    }

    #[test]
    fn imported_buffer_info_roundtrips() {
        let mut pool = import_only_pool();
        let retainer: Arc<dyn Any + Send + Sync> = Arc::new(());
        let handle = pool.import(0x8000_0000, 0x1234_0000, 0x2000, 0, retainer);

        let info = pool
            .get_buffer_info(handle)
            .expect("imported handle resolves");
        assert_eq!(info.dma_addr, 0x8000_0000);
        assert_eq!(info.obj_addr, 0x1234_0000);
        assert_eq!(info.size, 0x2000);
    }

    #[test]
    fn owned_gem_backing_is_page_aligned_before_mmap_export() {
        assert_eq!(page_align_size(0x1000, 0x1000), Ok(0x1000));
        assert_eq!(page_align_size(0x1001, 0x1000), Ok(0x2000));
        assert_eq!(page_align_size(0, 0x1000), Ok(0));
        assert_eq!(
            page_align_size(usize::MAX, 0x1000),
            Err(RknpuError::InvalidParameter)
        );
        assert_eq!(
            page_align_size(0x1001, 0),
            Err(RknpuError::InvalidParameter)
        );
    }

    #[test]
    fn buffer_retainer_is_none_for_unknown_handle() {
        let pool = import_only_pool();
        assert!(pool.buffer_retainer(0xdead_beef).is_none());
    }
    #[derive(Default)]
    struct AllocOp {
        live_bytes: AtomicUsize,
        calls: AtomicUsize,
        fail: AtomicBool,
    }

    impl DmaOp for AllocOp {
        fn page_size(&self) -> usize {
            4096
        }

        unsafe fn alloc_contiguous(
            &self,
            _: DmaConstraints,
            layout: Layout,
        ) -> Option<DmaAllocHandle> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if self.fail.load(Ordering::Relaxed) {
                return None;
            }
            // SAFETY: layout comes from DeviceDma; this backend owns the new
            // allocation until the matching dealloc_contiguous call. There is
            // no hardware; the CPU allocation is also the test DMA address.
            let ptr = NonNull::new(unsafe { alloc::alloc::alloc_zeroed(layout) })?;
            self.live_bytes.fetch_add(layout.size(), Ordering::Relaxed);
            Some(unsafe { DmaAllocHandle::new(ptr, ptr, (ptr.as_ptr() as u64).into(), layout) })
        }

        unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
            // SAFETY: DeviceDma returns the unique handle produced above,
            // with the same pointer and layout and no remaining buffer users.
            unsafe { alloc::alloc::dealloc(handle.allocation_ptr().as_ptr(), handle.layout()) };
            self.live_bytes.fetch_sub(handle.size(), Ordering::Relaxed);
        }

        unsafe fn alloc_coherent(&self, _: DmaConstraints, _: Layout) -> Option<DmaAllocHandle> {
            panic!("test DMA device is coherent")
        }
        unsafe fn dealloc_coherent(&self, _: DmaAllocHandle) -> Result<(), DmaError> {
            panic!("test DMA device is coherent")
        }
        unsafe fn map_streaming(
            &self,
            _: DmaConstraints,
            _: NonNull<u8>,
            _: NonZeroUsize,
            _: DmaDirection,
        ) -> Result<DmaMapHandle, DmaError> {
            panic!("GEM uses contiguous allocations")
        }
        unsafe fn unmap_streaming(&self, _: DmaMapHandle) {
            panic!("GEM uses contiguous allocations")
        }
    }

    fn allocation_pool(bytes: usize, objects: usize) -> (GemPool, &'static AllocOp) {
        let op = alloc::boxed::Box::leak(alloc::boxed::Box::new(AllocOp::default()));
        let mut pool = GemPool::new(DeviceDma::new(
            dma_api::DmaDeviceInfo::new(
                dma_api::DmaDomainId::Direct,
                dma_api::DmaCoherency::Coherent,
                DmaConstraints::new(u64::MAX),
            ),
            op,
        ));
        pool.usage = Arc::new(GemUsage::new(bytes, objects));
        (pool, op)
    }

    fn owner(bytes: usize, objects: usize) -> GemOwner {
        GemOwner {
            usage: Arc::new(GemUsage::new(bytes, objects)),
        }
    }

    fn create(pool: &mut GemPool, owner: &GemOwner, size: usize) -> Result<u32, RknpuError> {
        let mut args = RknpuMemCreate {
            size: size as u64,
            ..Default::default()
        };
        pool.create(owner, &mut args)?;
        Ok(args.handle)
    }

    #[test]
    fn allocation_limits_reject_before_dma_and_recover_after_release() {
        let (mut pool, op) = allocation_pool(4 * 4096, 16);
        let first = owner(2 * 4096, 16);
        let second = owner(2 * 4096, 16);
        for size in [0, usize::MAX] {
            assert_eq!(
                create(&mut pool, &first, size),
                Err(RknpuError::InvalidParameter)
            );
        }
        assert_eq!(
            create(&mut pool, &first, MAX_ALLOCATION_BYTES + 1),
            Err(RknpuError::OutOfMemory)
        );
        assert_eq!(op.calls.load(Ordering::Relaxed), 0);
        // A byte past the page boundary consumes the whole second page.
        let a = create(&mut pool, &first, 4097).unwrap();
        let calls = op.calls.load(Ordering::Relaxed);
        assert_eq!(create(&mut pool, &first, 1), Err(RknpuError::OutOfMemory));
        assert_eq!(op.calls.load(Ordering::Relaxed), calls);
        assert_eq!(op.live_bytes.load(Ordering::Relaxed), 8192);
        create(&mut pool, &second, 8192).unwrap();
        let third = owner(8192, 16);
        assert_eq!(create(&mut pool, &third, 1), Err(RknpuError::OutOfMemory));
        assert_eq!(op.calls.load(Ordering::Relaxed), calls + 1);
        assert_eq!(op.live_bytes.load(Ordering::Relaxed), 16384);
        pool.destroy(a);
        pool.destroy(a); // Repeated destroy must not return quota twice.
        let recovered = create(&mut pool, &third, 8192).unwrap();
        pool.destroy(recovered);
        create(&mut pool, &first, 8192).unwrap();
        drop(pool);
        assert_eq!(op.live_bytes.load(Ordering::Relaxed), 0);
        // A failed DMA allocation must return both owner and device quota.
        let (mut pool, op) = allocation_pool(8192, 1);
        op.fail.store(true, Ordering::Relaxed);
        assert_eq!(
            create(&mut pool, &first, 8192),
            Err(RknpuError::OutOfMemory)
        );
        op.fail.store(false, Ordering::Relaxed);
        create(&mut pool, &first, 8192).unwrap();
        drop(pool);
        assert_eq!(op.live_bytes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn object_limits_remain_charged_through_export_and_import() {
        let (mut pool, op) = allocation_pool(16 * 4096, 2);
        let first = owner(16 * 4096, 1);
        let second = owner(16 * 4096, 1);
        let a = create(&mut pool, &first, 1).unwrap();
        let info = pool.get_buffer_info(a).unwrap();
        let anchor = pool.buffer_retainer(a).unwrap();
        let imported = pool.import(info.dma_addr, info.obj_addr, info.size, 0, anchor.clone());
        pool.destroy(a);
        assert_eq!(create(&mut pool, &first, 1), Err(RknpuError::OutOfMemory));
        let b = create(&mut pool, &second, 1).unwrap();
        let third = owner(16 * 4096, 1);
        assert_eq!(create(&mut pool, &third, 1), Err(RknpuError::OutOfMemory));
        assert_eq!(op.calls.load(Ordering::Relaxed), 2);
        drop(anchor);
        assert_eq!(create(&mut pool, &first, 1), Err(RknpuError::OutOfMemory));
        pool.destroy(imported);
        assert_eq!(op.live_bytes.load(Ordering::Relaxed), 4096);
        create(&mut pool, &first, 1).unwrap();
        let anchor = pool.buffer_retainer(b).unwrap();
        drop(second);
        drop(pool);
        assert_eq!(op.live_bytes.load(Ordering::Relaxed), 4096);
        drop(anchor);
        assert_eq!(op.live_bytes.load(Ordering::Relaxed), 0);
    }
}
