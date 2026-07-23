use alloc::{collections::btree_map::BTreeMap, sync::Arc};
use core::any::Any;

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
    Owned {
        data: Arc<ContiguousArray<u8>>,
        flags: u32,
    },
    Imported(ImportedBuffer),
}

pub struct GemPool {
    dma: DeviceDma,
    pool: BTreeMap<u32, GemBuffer>,
    handle_counter: u32,
}

impl GemPool {
    pub fn new(dma: DeviceDma) -> Self {
        GemPool {
            dma,
            pool: BTreeMap::new(),
            handle_counter: 1,
        }
    }

    fn next_handle(&mut self) -> u32 {
        let handle = self.handle_counter;
        self.handle_counter = self.handle_counter.wrapping_add(1);
        handle
    }

    pub fn create(&mut self, args: &mut RknpuMemCreate) -> Result<(), RknpuError> {
        let data = self
            .dma
            .contiguous_array_zero_with_align::<u8>(
                args.size as _,
                0x1000,
                DmaDirection::Bidirectional,
            )
            .map_err(|_| RknpuError::DmaError)?;

        let handle = self.next_handle();

        args.handle = handle;
        args.sram_size = data.len() as _;
        args.dma_addr = data.dma_addr().as_u64();
        args.obj_addr = data.as_ptr().as_ptr() as _;
        self.pool.insert(
            args.handle,
            GemBuffer::Owned {
                data: Arc::new(data),
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
                        data.prepare_for_device(offset, size);
                    }
                    if args.flags & RKNPU_MEM_SYNC_FROM_DEVICE != 0 {
                        data.complete_for_cpu(offset, size);
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
                data.prepare_for_device_all();
            }
        }
        Ok(())
    }

    pub fn prepare_read_all(&mut self) -> Result<(), RknpuError> {
        for buffer in self.pool.values_mut() {
            if let GemBuffer::Owned { data, .. } = buffer {
                data.complete_for_cpu_all();
            }
        }
        Ok(())
    }
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
        unsafe fn dealloc_coherent(&self, _: DmaAllocHandle) {
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
        GemPool::new(DeviceDma::new_identity(u32::MAX as u64, &OP))
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
    fn buffer_retainer_is_none_for_unknown_handle() {
        let pool = import_only_pool();
        assert!(pool.buffer_retainer(0xdead_beef).is_none());
    }
}
