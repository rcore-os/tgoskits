#![cfg(all(test, any(unix, windows)))]

mod test_helpers;

use std::{num::NonZeroUsize, ptr::NonNull};

use dma_api::*;
use test_helpers::{DmaOperation, TrackingDmaOp};

#[test]
fn test_read() {
    let mut dma: DArray<u32> = new_api()
        .array_zero_with_align(10, 0x1000, DmaDirection::FromDevice)
        .unwrap();

    dma.set(0, 1);

    let o = dma.read(0).unwrap();

    assert_eq!(o, 1);
}

#[test]
fn test_write() {
    let mut dma: DArray<u32> = new_api()
        .array_zero_with_align(10, 0x1000, DmaDirection::ToDevice)
        .unwrap();

    dma.set(0, 1);

    let o = dma.read(0).unwrap();

    assert_eq!(o, 1);
}
#[derive(Debug, PartialEq, Eq)]
struct Foo {
    foo: u32,
    bar: u32,
}

#[test]
fn test_modify() {
    let mut dma: DBox<Foo> = new_api()
        .box_zero_with_align(64, DmaDirection::Bidirectional)
        .unwrap();

    dma.modify(|f| f.bar = 1);

    assert_eq!(dma.read(), Foo { foo: 0, bar: 1 });
}

#[test]
fn test_copy() {
    let mut dma = new_api()
        .array_zero_with_align::<u32>(0x40, 0x1000, DmaDirection::Bidirectional)
        .unwrap();

    println!("new dma ok");

    let src = [1u32; 0x40];

    dma.copy_from_slice(&src);

    println!("copy ok");

    for (i, &v) in src.iter().enumerate() {
        assert_eq!(dma.read(i).unwrap(), v);
    }
}

#[test]
fn test_index() {
    let dma = new_api()
        .array_zero_with_align::<u64>(0x40, 0x1000, DmaDirection::Bidirectional)
        .unwrap();

    println!("new dma ok");

    let a = dma.read(0).unwrap();

    assert_eq!(a, 0);
}

#[test]
fn mask_check_rejects_overflow_alloc() {
    static DMA: MaskedDma = MaskedDma;
    let dev = DeviceDma::new(0x0fff, &DMA);

    let err = dev.array_zero_with_align::<u8>(0x1000, 0x1000, DmaDirection::ToDevice);

    assert!(matches!(err, Err(DmaError::DmaMaskNotMatch { .. })));
}

#[test]
fn mask_check_rejects_overflow_map() {
    static DMA: MaskedDma = MaskedDma;
    let dev = DeviceDma::new(0x0fff, &DMA);

    let mut buf = [0u8; 0x1000];

    let err = dev.map_single_array(&buf, 64, DmaDirection::FromDevice);

    assert!(matches!(err, Err(DmaError::DmaMaskNotMatch { .. })));
}

fn new_api() -> DeviceDma {
    static IMPL: Impled = Impled;
    DeviceDma::new(u64::MAX, &IMPL)
}

struct Impled;

impl DmaOp for Impled {
    fn page_size(&self) -> usize {
        0x1000
    }

    unsafe fn map_single(
        &self,
        _dma_mask: u64,
        addr: NonNull<u8>,
        size: NonZeroUsize,
        align: usize,
        _direction: DmaDirection,
    ) -> Result<DmaMapHandle, DmaError> {
        println!(
            "map_single @{:?}, size {:#x}, align: {:#x}",
            addr,
            size.get(),
            align
        );
        let layout = core::alloc::Layout::from_size_align(size.get(), align)?;
        Ok(unsafe { DmaMapHandle::new(addr, (addr.as_ptr() as u64).into(), layout, None) })
    }

    unsafe fn unmap_single(&self, handle: DmaMapHandle) {
        println!(
            "unmap_single @{:?}, size {:#x}",
            handle.as_ptr(),
            handle.size()
        );
    }

    fn flush(&self, addr: std::ptr::NonNull<u8>, size: usize) {
        println!("flush @{:?}, size {size:#x}", addr);
    }

    fn invalidate(&self, addr: std::ptr::NonNull<u8>, size: usize) {
        println!("invalidate @{:?}, size {size:#x}", addr);
    }

    unsafe fn alloc_coherent(
        &self,
        _dma_mask: u64,
        layout: core::alloc::Layout,
    ) -> Option<DmaHandle> {
        println!(
            "alloc_coherent size: {:#x}, align: {:#x}",
            layout.size(),
            layout.align()
        );
        let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
        if ptr.is_null() {
            return None;
        }
        Some(unsafe { DmaHandle::new(NonNull::new(ptr).unwrap(), (ptr as u64).into(), layout) })
    }

    unsafe fn dealloc_coherent(&self, handle: DmaHandle) {
        unsafe { std::alloc::dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
    }
}

struct MaskedDma;

impl DmaOp for MaskedDma {
    fn page_size(&self) -> usize {
        0x1000
    }

    unsafe fn map_single(
        &self,
        _dma_mask: u64,
        addr: NonNull<u8>,
        size: NonZeroUsize,
        align: usize,
        _direction: DmaDirection,
    ) -> Result<DmaMapHandle, DmaError> {
        let layout = core::alloc::Layout::from_size_align(size.get(), align)?;
        Ok(unsafe { DmaMapHandle::new(addr, 0x1000u64.into(), layout, None) })
    }

    unsafe fn unmap_single(&self, _handle: DmaMapHandle) {}

    fn flush(&self, _addr: std::ptr::NonNull<u8>, _size: usize) {}

    fn invalidate(&self, _addr: std::ptr::NonNull<u8>, _size: usize) {}

    unsafe fn alloc_coherent(
        &self,
        _dma_mask: u64,
        layout: core::alloc::Layout,
    ) -> Option<DmaHandle> {
        let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
        if ptr.is_null() {
            return None;
        }
        Some(unsafe { DmaHandle::new(NonNull::new(ptr).unwrap(), 0x1000u64.into(), layout) })
    }

    unsafe fn dealloc_coherent(&self, handle: DmaHandle) {
        unsafe { std::alloc::dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
    }
}

// ============================================================================
// 新增测试: 地址正确性测试
// ============================================================================

#[test]
fn test_single_element_address() {
    let tracker = Box::new(TrackingDmaOp::new(0));
    let tracker: &'static TrackingDmaOp = Box::leak(tracker);
    let dev = DeviceDma::new(u64::MAX, tracker);

    let mut dma: DArray<u32> = dev
        .array_zero_with_align(1, 64, DmaDirection::ToDevice)
        .unwrap();

    tracker.clear();
    dma.set(0, 0x12345678);

    // 验证有且仅有一次 flush 操作,大小为 4 字节 (u32)
    let ops = tracker.get_operations();
    let flush_ops: Vec<_> = ops
        .iter()
        .filter_map(|op| {
            if let DmaOperation::Flush { size, .. } = op {
                Some(*size)
            } else {
                None
            }
        })
        .collect();

    assert_eq!(flush_ops.len(), 1);
    assert_eq!(flush_ops[0], 4); // u32 = 4 bytes
}

#[test]
fn test_multi_element_offset() {
    let tracker = Box::new(TrackingDmaOp::new(0));
    let tracker: &'static TrackingDmaOp = Box::leak(tracker);
    let dev = DeviceDma::new(u64::MAX, tracker);

    let mut dma: DArray<u32> = dev
        .array_zero_with_align(10, 64, DmaDirection::ToDevice)
        .unwrap();
    tracker.clear();

    // 设置 index 0, 1, 2
    dma.set(0, 0);
    dma.set(1, 1);
    dma.set(2, 2);

    // u32 = 4 bytes, 应该有 3 次 flush 操作,每次大小都是 4 字节
    let ops = tracker.get_operations();
    let flush_ops: Vec<_> = ops
        .iter()
        .filter_map(|op| {
            if let DmaOperation::Flush { size, addr } = op {
                Some((*addr, *size))
            } else {
                None
            }
        })
        .collect();

    assert_eq!(flush_ops.len(), 3);

    // 验证每次 flush 的大小都是 4 字节
    for (addr, size) in &flush_ops {
        assert_eq!(*size, 4);
    }

    // 验证地址是递增的,每次增加 4 字节
    assert_eq!(flush_ops[1].0 - flush_ops[0].0, 4); // index 1 - index 0
    assert_eq!(flush_ops[2].0 - flush_ops[1].0, 4); // index 2 - index 1
}

#[test]
fn test_different_type_sizes() {
    let tracker = Box::new(TrackingDmaOp::new(0));
    let tracker: &'static TrackingDmaOp = Box::leak(tracker);
    let dev = DeviceDma::new(u64::MAX, tracker);

    // u8 = 1 byte
    let mut dma_u8: DArray<u8> = dev
        .array_zero_with_align(5, 8, DmaDirection::ToDevice)
        .unwrap();
    tracker.clear();
    dma_u8.set(3, 1);

    let ops = tracker.get_operations();
    if let Some(DmaOperation::Flush { size, .. }) = ops.last() {
        assert_eq!(*size, 1); // u8 = 1 byte
    } else {
        panic!("Expected Flush operation");
    }

    // u64 = 8 bytes
    let mut dma_u64: DArray<u64> = dev
        .array_zero_with_align(5, 8, DmaDirection::ToDevice)
        .unwrap();
    tracker.clear();
    dma_u64.set(2, 2);

    let ops = tracker.get_operations();
    if let Some(DmaOperation::Flush { size, .. }) = ops.last() {
        assert_eq!(*size, 8); // u64 = 8 bytes
    } else {
        panic!("Expected Flush operation");
    }
}

// ============================================================================
// 新增测试: DmaDirection 行为测试
// ============================================================================

#[test]
fn test_direction_to_device() {
    let tracker = Box::new(TrackingDmaOp::new(0x1000));
    let tracker: &'static TrackingDmaOp = Box::leak(tracker);
    let dev = DeviceDma::new(u64::MAX, tracker);
    let mut dma: DArray<u32> = dev
        .array_zero_with_align(10, 64, DmaDirection::ToDevice)
        .unwrap();

    tracker.clear();

    // set 应该 flush
    dma.set(0, 1);
    assert_eq!(tracker.count_flush(), 1);
    assert_eq!(tracker.count_invalidate(), 0);

    // read 不应该 inv
    dma.read(0);
    assert_eq!(tracker.count_flush(), 1); // 没有增加
    assert_eq!(tracker.count_invalidate(), 0); // 没有增加
}

#[test]
fn test_direction_from_device() {
    let tracker = Box::new(TrackingDmaOp::new(0x1000));
    let tracker: &'static TrackingDmaOp = Box::leak(tracker);
    let dev = DeviceDma::new(u64::MAX, tracker);
    let mut dma: DArray<u32> = dev
        .array_zero_with_align(10, 64, DmaDirection::FromDevice)
        .unwrap();

    tracker.clear();

    // read 应该 inv
    dma.read(0);
    assert_eq!(tracker.count_flush(), 0);
    assert_eq!(tracker.count_invalidate(), 1);

    // set 不应该 flush
    tracker.clear();
    dma.set(0, 1);
    assert_eq!(tracker.count_flush(), 0);
    assert_eq!(tracker.count_invalidate(), 0);
}

#[test]
fn test_direction_bidirectional() {
    let tracker = Box::new(TrackingDmaOp::new(0x1000));
    let tracker: &'static TrackingDmaOp = Box::leak(tracker);
    let dev = DeviceDma::new(u64::MAX, tracker);
    let mut dma: DArray<u32> = dev
        .array_zero_with_align(10, 64, DmaDirection::Bidirectional)
        .unwrap();

    tracker.clear();

    // set 应该 flush
    dma.set(0, 1);
    assert_eq!(tracker.count_flush(), 1);

    // read 应该 inv
    dma.read(0);
    assert_eq!(tracker.count_invalidate(), 1);
}

// ============================================================================
// 新增测试: Drop 行为测试
// ============================================================================

// 简单的对齐缓冲区模块
mod align_alloc {
    use std::ptr::NonNull;

    pub struct AlignedBuffer<const ALIGNMENT: usize> {
        ptr: NonNull<u8>,
        _size: usize,
    }

    impl<const ALIGNMENT: usize> AlignedBuffer<ALIGNMENT> {
        pub fn new() -> Self {
            // 分配对齐的内存
            let layout = std::alloc::Layout::from_size_align(0x1000, ALIGNMENT).unwrap();
            let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
            assert!(!ptr.is_null(), "Failed to allocate aligned buffer");

            Self {
                ptr: NonNull::new(ptr).unwrap(),
                _size: 0x1000,
            }
        }

        pub fn as_mut_ptr(&mut self) -> *mut u8 {
            self.ptr.as_ptr()
        }
    }

    impl<const ALIGNMENT: usize> Drop for AlignedBuffer<ALIGNMENT> {
        fn drop(&mut self) {
            let layout = std::alloc::Layout::from_size_align(0x1000, ALIGNMENT).unwrap();
            unsafe { std::alloc::dealloc(self.ptr.as_ptr(), layout) };
        }
    }
}
