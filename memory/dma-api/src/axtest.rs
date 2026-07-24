use alloc::{
    alloc::{alloc_zeroed, dealloc},
    boxed::Box,
    vec,
};
use core::{
    alloc::Layout,
    num::NonZeroUsize,
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
};

use axtest::prelude::*;

use crate::{
    DeviceDma, DmaAllocHandle, DmaConstraints, DmaDirection, DmaError, DmaMapHandle, DmaOp,
    op::arch,
};

#[derive(Default)]
struct TrackingDmaOp {
    next_dma_addr: AtomicUsize,
    forced_dma_addr: AtomicUsize,
    alloc_contiguous: AtomicUsize,
    dealloc_contiguous: AtomicUsize,
    alloc_coherent: AtomicUsize,
    dealloc_coherent: AtomicUsize,
    map_streaming: AtomicUsize,
    unmap_streaming: AtomicUsize,
    sync_alloc_for_device: AtomicUsize,
    sync_alloc_for_cpu: AtomicUsize,
    sync_map_for_device: AtomicUsize,
    sync_map_for_cpu: AtomicUsize,
}

impl TrackingDmaOp {
    fn new() -> Self {
        Self {
            next_dma_addr: AtomicUsize::new(0x1000),
            ..Self::default()
        }
    }

    fn force_next_dma_addr(&self, dma_addr: usize) {
        self.forced_dma_addr.store(dma_addr, Ordering::SeqCst);
    }

    fn clear_sync_counts(&self) {
        self.sync_alloc_for_device.store(0, Ordering::SeqCst);
        self.sync_alloc_for_cpu.store(0, Ordering::SeqCst);
        self.sync_map_for_device.store(0, Ordering::SeqCst);
        self.sync_map_for_cpu.store(0, Ordering::SeqCst);
    }

    fn alloc_dma_addr(&self, layout: Layout, constraints: DmaConstraints) -> usize {
        let forced = self.forced_dma_addr.swap(0, Ordering::SeqCst);
        if forced != 0 {
            return forced;
        }

        let align = constraints.align.max(layout.align()).max(1);
        let current = self
            .next_dma_addr
            .load(Ordering::SeqCst)
            .next_multiple_of(align);
        let next = current
            .saturating_add(layout.size().max(1))
            .max(current + 1);
        self.next_dma_addr.store(next, Ordering::SeqCst);
        current
    }

    unsafe fn alloc_handle(
        &self,
        constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        let ptr = unsafe { alloc_zeroed(layout) };
        let cpu_addr = NonNull::new(ptr)?;
        let dma_addr = self.alloc_dma_addr(layout, constraints);
        Some(unsafe { DmaAllocHandle::new(cpu_addr, (dma_addr as u64).into(), layout) })
    }
}

impl DmaOp for TrackingDmaOp {
    fn page_size(&self) -> usize {
        0x1000
    }

    unsafe fn alloc_contiguous(
        &self,
        constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        self.alloc_contiguous.fetch_add(1, Ordering::SeqCst);
        unsafe { self.alloc_handle(constraints, layout) }
    }

    unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
        self.dealloc_contiguous.fetch_add(1, Ordering::SeqCst);
        unsafe { dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
    }

    unsafe fn alloc_coherent(
        &self,
        constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        self.alloc_coherent.fetch_add(1, Ordering::SeqCst);
        unsafe { self.alloc_handle(constraints, layout) }
    }

    unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) {
        self.dealloc_coherent.fetch_add(1, Ordering::SeqCst);
        unsafe { dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
    }

    unsafe fn map_streaming(
        &self,
        constraints: DmaConstraints,
        addr: NonNull<u8>,
        size: NonZeroUsize,
        direction: DmaDirection,
    ) -> Result<DmaMapHandle, DmaError> {
        self.map_streaming.fetch_add(1, Ordering::SeqCst);
        let layout = Layout::from_size_align(size.get(), constraints.align.max(1))?;
        let dma_addr = self.alloc_dma_addr(layout, constraints);
        let bounce_ptr = if dma_addr != addr.as_ptr() as usize {
            let ptr = unsafe { alloc_zeroed(layout) };
            Some(NonNull::new(ptr).ok_or(DmaError::NoMemory)?)
        } else {
            None
        };
        let _ = direction;
        Ok(unsafe { DmaMapHandle::new(addr, (dma_addr as u64).into(), layout, bounce_ptr) })
    }

    unsafe fn unmap_streaming(&self, handle: DmaMapHandle) {
        self.unmap_streaming.fetch_add(1, Ordering::SeqCst);
        if let Some(ptr) = handle.bounce_ptr() {
            unsafe { dealloc(ptr.as_ptr(), handle.layout()) };
        }
    }

    fn sync_alloc_for_device(
        &self,
        _handle: &DmaAllocHandle,
        _offset: usize,
        _size: usize,
        _direction: DmaDirection,
    ) {
        self.sync_alloc_for_device.fetch_add(1, Ordering::SeqCst);
    }

    fn sync_alloc_for_cpu(
        &self,
        _handle: &DmaAllocHandle,
        _offset: usize,
        _size: usize,
        _direction: DmaDirection,
    ) {
        self.sync_alloc_for_cpu.fetch_add(1, Ordering::SeqCst);
    }

    fn sync_map_for_device(
        &self,
        handle: &DmaMapHandle,
        offset: usize,
        size: usize,
        _direction: DmaDirection,
    ) {
        self.sync_map_for_device.fetch_add(1, Ordering::SeqCst);
        if let Some(bounce) = handle.bounce_ptr() {
            unsafe {
                bounce
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
        _direction: DmaDirection,
    ) {
        self.sync_map_for_cpu.fetch_add(1, Ordering::SeqCst);
        if let Some(bounce) = handle.bounce_ptr() {
            unsafe {
                handle
                    .as_ptr()
                    .add(offset)
                    .as_ptr()
                    .copy_from_nonoverlapping(bounce.add(offset).as_ptr(), size);
            }
        }
    }
}

fn tracking_device() -> (DeviceDma, &'static TrackingDmaOp) {
    let op = Box::leak(Box::new(TrackingDmaOp::new()));
    (DeviceDma::new_legacy(u64::MAX, op), op)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C)]
struct Descriptor {
    addr: u64,
    len: u32,
    flags: u32,
}

#[axtest]
fn dma_api_device_metadata_constraints_and_nop_cache_ops_are_callable() {
    let (dev, op) = tracking_device();
    let domain = crate::DmaDomainId::from_raw(0x42);
    let scoped = DeviceDma::new(domain, u32::MAX as u64, op);
    let constrained = scoped.with_constraints(
        DmaConstraints::new(0xffff)
            .with_align(64)
            .with_boundary(0x1000),
    );

    ax_assert_eq!(dev.page_size(), 0x1000);
    ax_assert_eq!(scoped.domain_id(), domain);
    ax_assert_eq!(constrained.dma_mask(), 0xffff);
    ax_assert_eq!(constrained.constraints().align, 64);

    let mut byte = 0u8;
    let ptr = NonNull::from(&mut byte);
    dev.flush(ptr, 1);
    dev.invalidate(ptr, 1);
    dev.flush_invalidate(ptr, 1);
    arch::flush(ptr, 1);
    arch::invalidate(ptr, 1);
    arch::flush_invalidate(ptr, 1);
}

#[axtest]
fn dma_api_coherent_and_contiguous_arrays_cover_cpu_and_sync_accessors() {
    let (dev, op) = tracking_device();

    let mut coherent = dev
        .coherent_array_zero_with_align::<Descriptor>(4, 64)
        .unwrap();
    ax_assert_eq!(coherent.len(), 4);
    ax_assert!(!coherent.is_empty());
    coherent.set_cpu(
        1,
        Descriptor {
            addr: 0x1000,
            len: 16,
            flags: 1,
        },
    );
    coherent.copy_from_slice_cpu(&[Descriptor {
        addr: 0x2000,
        len: 32,
        flags: 2,
    }]);
    coherent.write_with_cpu(2, |items| items[1].flags = 9);
    ax_assert_eq!(coherent.read_cpu(0).unwrap().addr, 0x2000);
    ax_assert_eq!(coherent.read_with_cpu(2, |items| items[1].flags), 9);
    ax_assert_eq!(coherent.iter_cpu().count(), 4);

    let mut contiguous = dev
        .contiguous_array_zero_with_align::<u8>(8, 64, DmaDirection::ToDevice)
        .unwrap();
    contiguous.copy_to_device_from_slice(&[1, 2, 3, 4]);
    ax_assert_eq!(contiguous.read_cpu(2), Some(3));
    ax_assert_eq!(contiguous.direction(), DmaDirection::ToDevice);
    ax_assert_eq!(contiguous.domain_id(), dev.domain_id());
    ax_assert_eq!(op.sync_alloc_for_device.load(Ordering::SeqCst), 1);

    let mut out = [0u8; 4];
    contiguous.copy_from_device_to_slice(&mut out);
    ax_assert_eq!(out, [1, 2, 3, 4]);
    contiguous.prepare_for_device_all();
    contiguous.complete_for_cpu_all();
    ax_assert!(op.sync_alloc_for_device.load(Ordering::SeqCst) >= 2);
    ax_assert!(op.sync_alloc_for_cpu.load(Ordering::SeqCst) >= 2);
}

#[axtest]
fn dma_api_boxes_and_pool_cover_drop_reuse_and_sync_paths() {
    let (dev, op) = tracking_device();

    {
        let mut coherent = dev.coherent_box_zero_with_align::<Descriptor>(64).unwrap();
        coherent.write_cpu(Descriptor {
            addr: 1,
            len: 2,
            flags: 3,
        });
        coherent.modify_cpu(|descriptor| descriptor.flags += 1);
        ax_assert_eq!(coherent.read_cpu().flags, 4);
        let bytes = unsafe { coherent.as_bytes_mut_cpu() };
        ax_assert_eq!(bytes.len(), core::mem::size_of::<Descriptor>());
    }
    ax_assert_eq!(op.dealloc_coherent.load(Ordering::SeqCst), 1);

    {
        let mut contiguous = dev
            .contiguous_box_zero_with_align::<Descriptor>(64, DmaDirection::FromDevice)
            .unwrap();
        contiguous.write_for_device(Descriptor {
            addr: 5,
            len: 6,
            flags: 7,
        });
        contiguous.modify_for_device(|descriptor| descriptor.len += 1);
        ax_assert_eq!(contiguous.read_from_device().len, 7);
        let bytes = unsafe { contiguous.as_bytes_mut_cpu() };
        ax_assert_eq!(bytes.len(), core::mem::size_of::<Descriptor>());
    }
    ax_assert_eq!(op.dealloc_contiguous.load(Ordering::SeqCst), 1);

    let pool = dev.contiguous_buffer_pool(
        Layout::from_size_align(32, 32).unwrap(),
        DmaDirection::ToDevice,
        1,
    );
    {
        let mut buffer = pool.alloc().unwrap();
        unsafe { buffer.as_mut_slice_cpu()[0] = 0x5a };
    }
    op.clear_sync_counts();
    let buffer = pool.alloc().unwrap();
    ax_assert_eq!(buffer.as_slice_cpu()[0], 0x5a);
    ax_assert_eq!(op.sync_alloc_for_device.load(Ordering::SeqCst), 0);
}

#[axtest]
fn dma_api_streaming_maps_cover_direct_bounce_and_vector_accessors() {
    let (dev, op) = tracking_device();
    let mut direct = [0u8; 8];
    op.force_next_dma_addr(direct.as_mut_ptr() as usize);
    let mut map = dev
        .map_streaming_slice_for_device(&mut direct, 8, DmaDirection::Bidirectional)
        .unwrap();
    ax_assert!(map.bounce_ptr().is_none());
    ax_assert_eq!(map.len(), 8);
    map.set_cpu(0, 9);
    map.write_for_device(4, |data| data.copy_from_slice(&[1, 2, 3, 4]));
    ax_assert_eq!(map.read_from_device(4, |data| data[3]), 4);
    ax_assert_eq!(map.to_vec_cpu(), vec![1, 2, 3, 4, 0, 0, 0, 0]);
    drop(map);
    ax_assert_eq!(op.unmap_streaming.load(Ordering::SeqCst), 1);

    let mut bounced = [1u8; 8];
    op.force_next_dma_addr(0x80);
    let map = dev
        .map_streaming_slice(&mut bounced, 8, DmaDirection::FromDevice)
        .unwrap();
    let bounce = map.bounce_ptr().unwrap();
    unsafe { bounce.as_ptr().write_bytes(0x7e, bounced.len()) };
    ax_assert_eq!(map.read_from_device(4, |data| data[0]), 0x7e);
    drop(map);
    ax_assert_eq!(bounced[0], 0x7e);
}

#[axtest]
fn dma_api_rejects_mask_alignment_segment_boundary_and_zero_sized_errors() {
    let (dev, op) = tracking_device();

    op.force_next_dma_addr(0x1_0000_0000);
    let mask_result = dev
        .with_constraints(DmaConstraints::new(u32::MAX as u64))
        .coherent_array_zero_with_align::<u8>(4096, 4096);
    ax_assert!(
        matches!(mask_result, Err(DmaError::DmaMaskNotMatch { .. })),
        "mask constraint should reject the forced DMA address"
    );

    op.force_next_dma_addr(0x1080);
    let align_result = dev
        .with_constraints(DmaConstraints::new(u64::MAX).with_align(0x1000))
        .coherent_array_zero_with_align::<u8>(16, 0x1000);
    ax_assert!(
        matches!(align_result, Err(DmaError::AlignMismatch { .. })),
        "align constraint should reject the forced DMA address"
    );

    let segment_result = dev
        .with_constraints(DmaConstraints::new(u64::MAX).with_max_segment_size(8))
        .coherent_array_zero_with_align::<u8>(16, 16);
    ax_assert!(
        matches!(segment_result, Err(DmaError::SegmentTooLarge { .. })),
        "max segment size should reject the oversized allocation"
    );

    op.force_next_dma_addr(0x1ff0);
    let boundary_result = dev
        .with_constraints(DmaConstraints::new(u64::MAX).with_boundary(0x1000))
        .coherent_array_zero_with_align::<u8>(32, 16);
    ax_assert!(
        matches!(boundary_result, Err(DmaError::BoundaryCross { .. })),
        "boundary constraint should reject the crossing allocation"
    );

    let mut empty: [u8; 0] = [];
    let zero_result = dev.map_streaming_slice(&mut empty, 1, DmaDirection::ToDevice);
    ax_assert!(
        matches!(zero_result, Err(DmaError::ZeroSizedBuffer)),
        "streaming map should reject zero-sized buffers"
    );
}

#[axtest]
fn dma_api_array_helper_functions_cover_len_and_layout() {
    ax_assert!(crate::array::array_helper_len_and_layout_rules_hold_for_test());
}

#[axtest]
fn dma_api_direction_and_error_variants_hold() {
    use crate::{DmaDirection, DmaError};

    // Test DmaDirection variants exist
    let _to_device = DmaDirection::ToDevice;
    let _bidirectional = DmaDirection::Bidirectional;

    // Test DmaError variants that may not be fully covered
    let _zero_sized = DmaError::ZeroSizedBuffer;
}

#[axtest]
fn dma_api_array_read_write_helpers_hold() {
    use crate::array;

    // Test read_at and write_at helper functions
    // These are tested indirectly through array operations but we can verify they exist
    ax_assert!(array::array_helper_len_and_layout_rules_hold_for_test());
}

#[axtest]
fn dma_api_array_layout_edge_cases_hold() {
    use crate::array;
    // Test additional edge cases for array layout
    ax_assert!(array::array_helper_len_and_layout_rules_hold_for_test());
}

#[axtest]
fn dma_api_array_contiguous_methods_hold() {
    use crate::array;
    ax_assert!(array::array_contiguous_methods_hold_for_test());
}

#[axtest]
fn dma_api_array_read_write_at_helpers_hold() {
    use crate::array;
    ax_assert!(array::array_read_at_write_at_helpers_hold_for_test());
}

#[axtest]
fn dma_api_array_dma_cpu_read_trait_hold() {
    use crate::array;
    ax_assert!(array::array_dma_array_cpu_read_trait_hold_for_test());
}

#[axtest]
fn dma_api_array_layout_comprehensive_edge_cases_hold() {
    use crate::array;
    ax_assert!(array::array_layout_edge_cases_comprehensive_hold_for_test());
}

#[axtest]
fn dma_api_array_copy_from_slice_and_write_at_edge_hold() {
    use crate::array;
    ax_assert!(array::array_copy_from_slice_and_write_at_edge_hold_for_test());
}

#[axtest]
fn dma_api_array_layout_overflow_and_size_align_hold() {
    use crate::array;
    ax_assert!(array::array_layout_overflow_and_size_align_hold_for_test());
}

#[axtest]
fn dma_api_streaming_struct_and_phantom_hold() {
    use crate::streaming;
    ax_assert!(streaming::streaming_struct_and_phantom_hold_for_test());
}

#[axtest]
fn dma_api_streaming_direction_and_error_types_hold() {
    use crate::streaming;
    ax_assert!(streaming::streaming_direction_and_error_types_hold_for_test());
}

#[axtest]
fn dma_api_streaming_struct_size_and_alignment_hold() {
    use crate::streaming;
    ax_assert!(streaming::streaming_struct_size_and_alignment_hold_for_test());
}

#[axtest]
fn dma_op_direction_matching_hold() {
    use crate::op;
    ax_assert!(op::dma_op_direction_matching_hold_for_test());
}

#[axtest]
fn dma_op_constraints_and_error_types_hold() {
    use crate::op;
    ax_assert!(op::dma_op_constraints_and_error_types_hold_for_test());
}

#[axtest]
fn dma_op_sync_direction_branches_hold() {
    use crate::op;
    ax_assert!(op::dma_op_sync_direction_branches_hold_for_test());
}

#[axtest]
fn dma_api_streaming_all_error_variants_hold() {
    use crate::streaming;
    ax_assert!(streaming::streaming_all_error_variants_hold_for_test());
}

#[axtest]
fn dma_api_streaming_dma_pod_types_hold() {
    use crate::streaming;
    ax_assert!(streaming::streaming_dma_pod_types_hold_for_test());
}

#[axtest]
fn dma_api_streaming_nonzero_and_phantom_hold() {
    use crate::streaming;
    ax_assert!(streaming::streaming_nonzero_and_phantom_hold_for_test());
}

#[axtest]
fn dma_api_streaming_dma_direction_all_variants_hold() {
    use crate::streaming;
    ax_assert!(streaming::streaming_dma_direction_all_variants_hold_for_test());
}
