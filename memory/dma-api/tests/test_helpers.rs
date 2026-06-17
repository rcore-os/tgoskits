use std::{
    alloc::{alloc_zeroed, dealloc},
    collections::HashMap,
    num::NonZeroUsize,
    ptr::NonNull,
    sync::{Arc, Mutex},
};

use dma_api::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DmaOperation {
    AllocContiguous {
        size: usize,
        align: usize,
        mask: u64,
    },
    DeallocContiguous {
        size: usize,
    },
    AllocCoherent {
        size: usize,
        align: usize,
        mask: u64,
    },
    DeallocCoherent {
        size: usize,
    },
    MapStreaming {
        virt_addr: usize,
        size: usize,
        align: usize,
        direction: DmaDirection,
        mask: u64,
    },
    UnmapStreaming {
        size: usize,
    },
    SyncAllocForDevice {
        addr: usize,
        size: usize,
        direction: DmaDirection,
    },
    SyncAllocForCpu {
        addr: usize,
        size: usize,
        direction: DmaDirection,
    },
    SyncMapForDevice {
        addr: usize,
        size: usize,
        direction: DmaDirection,
    },
    SyncMapForCpu {
        addr: usize,
        size: usize,
        direction: DmaDirection,
    },
}

#[derive(Clone)]
pub struct TrackingDmaOp {
    operations: Arc<Mutex<Vec<DmaOperation>>>,
    next_dma_addr: Arc<Mutex<u64>>,
    forced_dma_addr: Arc<Mutex<Option<u64>>>,
    map_allocations: Arc<Mutex<HashMap<usize, core::alloc::Layout>>>,
}

impl TrackingDmaOp {
    pub fn new() -> Self {
        Self {
            operations: Arc::new(Mutex::new(Vec::new())),
            next_dma_addr: Arc::new(Mutex::new(0x1000)),
            forced_dma_addr: Arc::new(Mutex::new(None)),
            map_allocations: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn with_next_dma_addr(self, dma_addr: u64) -> Self {
        *self.next_dma_addr.lock().unwrap() = dma_addr;
        self
    }

    pub fn force_next_dma_addr(&self, dma_addr: u64) {
        *self.forced_dma_addr.lock().unwrap() = Some(dma_addr);
    }

    pub fn operations(&self) -> Vec<DmaOperation> {
        self.operations.lock().unwrap().clone()
    }

    pub fn clear(&self) {
        self.operations.lock().unwrap().clear();
    }

    pub fn count_sync_alloc_for_device(&self) -> usize {
        self.operations
            .lock()
            .unwrap()
            .iter()
            .filter(|op| matches!(op, DmaOperation::SyncAllocForDevice { .. }))
            .count()
    }

    pub fn count_sync_alloc_for_cpu(&self) -> usize {
        self.operations
            .lock()
            .unwrap()
            .iter()
            .filter(|op| matches!(op, DmaOperation::SyncAllocForCpu { .. }))
            .count()
    }

    pub fn count_sync_map_for_device(&self) -> usize {
        self.operations
            .lock()
            .unwrap()
            .iter()
            .filter(|op| matches!(op, DmaOperation::SyncMapForDevice { .. }))
            .count()
    }

    pub fn count_sync_map_for_cpu(&self) -> usize {
        self.operations
            .lock()
            .unwrap()
            .iter()
            .filter(|op| matches!(op, DmaOperation::SyncMapForCpu { .. }))
            .count()
    }

    fn alloc_dma_addr(&self, layout: core::alloc::Layout, constraints: DmaConstraints) -> u64 {
        if let Some(addr) = self.forced_dma_addr.lock().unwrap().take() {
            return addr;
        }

        let mut next = self.next_dma_addr.lock().unwrap();
        let align = constraints.align.max(layout.align()).max(1) as u64;
        *next = next.next_multiple_of(align);
        let addr = *next;
        *next = next
            .saturating_add(layout.size().max(1) as u64)
            .max(addr + 1);
        addr
    }

    unsafe fn alloc_handle(
        &self,
        constraints: DmaConstraints,
        layout: core::alloc::Layout,
    ) -> Option<DmaAllocHandle> {
        let ptr = unsafe { alloc_zeroed(layout) };
        let cpu_addr = NonNull::new(ptr)?;
        let dma_addr = self.alloc_dma_addr(layout, constraints);
        Some(unsafe { DmaAllocHandle::new(cpu_addr, dma_addr.into(), layout) })
    }
}

impl DmaOp for TrackingDmaOp {
    fn page_size(&self) -> usize {
        0x1000
    }

    unsafe fn alloc_contiguous(
        &self,
        constraints: DmaConstraints,
        layout: core::alloc::Layout,
    ) -> Option<DmaAllocHandle> {
        self.operations
            .lock()
            .unwrap()
            .push(DmaOperation::AllocContiguous {
                size: layout.size(),
                align: layout.align(),
                mask: constraints.addr_mask,
            });
        unsafe { self.alloc_handle(constraints, layout) }
    }

    unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
        self.operations
            .lock()
            .unwrap()
            .push(DmaOperation::DeallocContiguous {
                size: handle.size(),
            });
        unsafe { dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
    }

    unsafe fn alloc_coherent(
        &self,
        constraints: DmaConstraints,
        layout: core::alloc::Layout,
    ) -> Option<DmaAllocHandle> {
        self.operations
            .lock()
            .unwrap()
            .push(DmaOperation::AllocCoherent {
                size: layout.size(),
                align: layout.align(),
                mask: constraints.addr_mask,
            });
        unsafe { self.alloc_handle(constraints, layout) }
    }

    unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) {
        self.operations
            .lock()
            .unwrap()
            .push(DmaOperation::DeallocCoherent {
                size: handle.size(),
            });
        unsafe { dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
    }

    unsafe fn map_streaming(
        &self,
        constraints: DmaConstraints,
        addr: NonNull<u8>,
        size: NonZeroUsize,
        direction: DmaDirection,
    ) -> Result<DmaMapHandle, DmaError> {
        let align = constraints.align.max(1);
        let layout = core::alloc::Layout::from_size_align(size.get(), align)?;
        self.operations
            .lock()
            .unwrap()
            .push(DmaOperation::MapStreaming {
                virt_addr: addr.as_ptr() as usize,
                size: size.get(),
                align,
                direction,
                mask: constraints.addr_mask,
            });

        let dma_addr = self.alloc_dma_addr(layout, constraints);
        let bounce_ptr = if dma_addr != addr.as_ptr() as u64 {
            let ptr = unsafe { alloc_zeroed(layout) };
            let ptr = NonNull::new(ptr).ok_or(DmaError::NoMemory)?;
            self.map_allocations
                .lock()
                .unwrap()
                .insert(ptr.as_ptr() as usize, layout);
            Some(ptr)
        } else {
            None
        };

        Ok(unsafe { DmaMapHandle::new(addr, dma_addr.into(), layout, bounce_ptr) })
    }

    unsafe fn unmap_streaming(&self, handle: DmaMapHandle) {
        self.operations
            .lock()
            .unwrap()
            .push(DmaOperation::UnmapStreaming {
                size: handle.size(),
            });
        if let Some(ptr) = handle.bounce_ptr()
            && let Some(layout) = self
                .map_allocations
                .lock()
                .unwrap()
                .remove(&(ptr.as_ptr() as usize))
        {
            unsafe { dealloc(ptr.as_ptr(), layout) };
        }
    }

    fn sync_alloc_for_device(
        &self,
        handle: &DmaAllocHandle,
        offset: usize,
        size: usize,
        direction: DmaDirection,
    ) {
        self.operations
            .lock()
            .unwrap()
            .push(DmaOperation::SyncAllocForDevice {
                addr: unsafe { handle.as_ptr().add(offset).as_ptr() as usize },
                size,
                direction,
            });
    }

    fn sync_alloc_for_cpu(
        &self,
        handle: &DmaAllocHandle,
        offset: usize,
        size: usize,
        direction: DmaDirection,
    ) {
        self.operations
            .lock()
            .unwrap()
            .push(DmaOperation::SyncAllocForCpu {
                addr: unsafe { handle.as_ptr().add(offset).as_ptr() as usize },
                size,
                direction,
            });
    }

    fn sync_map_for_device(
        &self,
        handle: &DmaMapHandle,
        offset: usize,
        size: usize,
        direction: DmaDirection,
    ) {
        self.operations
            .lock()
            .unwrap()
            .push(DmaOperation::SyncMapForDevice {
                addr: unsafe { handle.as_ptr().add(offset).as_ptr() as usize },
                size,
                direction,
            });
        if let Some(map_virt) = handle.bounce_ptr() {
            unsafe {
                map_virt
                    .add(offset)
                    .as_ptr()
                    .copy_from_nonoverlapping(handle.as_ptr().add(offset).as_ptr(), size);
            }
        }
    }

    fn sync_map_for_cpu(
        &self,
        handle: &DmaMapHandle,
        offset: usize,
        size: usize,
        direction: DmaDirection,
    ) {
        self.operations
            .lock()
            .unwrap()
            .push(DmaOperation::SyncMapForCpu {
                addr: unsafe { handle.as_ptr().add(offset).as_ptr() as usize },
                size,
                direction,
            });
        if let Some(map_virt) = handle.bounce_ptr() {
            unsafe {
                handle
                    .as_ptr()
                    .add(offset)
                    .as_ptr()
                    .copy_from_nonoverlapping(map_virt.add(offset).as_ptr(), size);
            }
        }
    }
}
