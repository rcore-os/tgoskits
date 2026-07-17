#![cfg(all(test, any(unix, windows)))]

mod test_helpers;

use ax_kspin_test_runtime as _;
use dma_api::*;
use test_helpers::{DmaOperation, TrackingDmaOp};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
struct Descriptor {
    addr: u64,
    len: u32,
    flags: u32,
}

fn new_tracking_device() -> (DeviceDma, &'static TrackingDmaOp) {
    let tracker = Box::new(TrackingDmaOp::new());
    let tracker = Box::leak(tracker);
    (DeviceDma::new_legacy(u64::MAX, tracker), tracker)
}

#[test]
fn cpu_owned_buffer_is_a_small_ownership_handle() {
    assert!(
        size_of::<CpuDmaBuffer>() <= 3 * size_of::<usize>(),
        "request transport must not copy allocation metadata through every submit result"
    );
}

#[test]
fn dma_ownership_transitions_deallocate_the_backing_exactly_once() {
    let (device, tracker) = new_tracking_device();
    let buffer = CpuDmaBuffer::new_zero(
        &device,
        core::num::NonZeroUsize::new(4096).unwrap(),
        4096,
        DmaDirection::FromDevice,
    )
    .unwrap();
    tracker.clear();

    let prepared = buffer.prepare_for_device();
    let inflight = unsafe {
        // SAFETY: this test models one accepted command and immediately proves
        // that its synthetic device has stopped before returning ownership.
        prepared.into_in_flight()
    };
    let completed = unsafe {
        // SAFETY: no device exists in this host test, so the DMA backing cannot
        // still be accessed after the modeled command boundary above.
        inflight.complete_after_quiesce()
    };
    drop(completed.into_cpu_buffer());

    let operations = tracker.operations();
    assert_eq!(
        operations
            .iter()
            .filter(|operation| matches!(operation, DmaOperation::DeallocContiguous { .. }))
            .count(),
        1
    );
    assert_eq!(tracker.count_sync_alloc_for_device(), 1);
    assert_eq!(tracker.count_sync_alloc_for_cpu(), 1);
}

#[test]
fn coherent_array_access_does_not_sync_cache() {
    let (dev, tracker) = new_tracking_device();
    let mut ring = dev
        .coherent_array_zero_with_align::<Descriptor>(8, 64)
        .unwrap();

    tracker.clear();
    ring.set_cpu(
        0,
        Descriptor {
            addr: 0x1000,
            len: 16,
            flags: 1,
        },
    );
    assert_eq!(ring.read_cpu(0).unwrap().addr, 0x1000);

    assert_eq!(tracker.count_sync_alloc_for_device(), 0);
    assert_eq!(tracker.count_sync_alloc_for_cpu(), 0);
}

#[test]
fn contiguous_array_cpu_accessors_do_not_sync_cache() {
    let (dev, tracker) = new_tracking_device();
    let mut buff = dev
        .contiguous_array_zero_with_align::<u32>(16, 64, DmaDirection::ToDevice)
        .unwrap();

    tracker.clear();
    buff.set_cpu(3, 0xA5A5_A5A5);
    assert_eq!(tracker.count_sync_alloc_for_device(), 0);
    assert_eq!(tracker.count_sync_alloc_for_cpu(), 0);

    buff.sync_for_device(3 * size_of::<u32>(), size_of::<u32>());
    assert_eq!(tracker.count_sync_alloc_for_device(), 1);
    assert_eq!(tracker.count_sync_alloc_for_cpu(), 0);

    let ops = tracker.operations();
    assert!(ops.iter().any(|op| matches!(
        op,
        DmaOperation::SyncAllocForDevice {
            size: 4,
            direction: DmaDirection::ToDevice,
            ..
        }
    )));
}

#[test]
fn contiguous_box_supports_cpu_sync() {
    let (dev, tracker) = new_tracking_device();
    let mut status = dev
        .contiguous_box_zero_with_align::<Descriptor>(64, DmaDirection::FromDevice)
        .unwrap();

    tracker.clear();
    status.write_cpu(Descriptor {
        addr: 0,
        len: 0,
        flags: 0,
    });
    status.sync_for_cpu_all();

    assert_eq!(tracker.count_sync_alloc_for_device(), 0);
    assert_eq!(tracker.count_sync_alloc_for_cpu(), 1);
}

#[test]
fn streaming_map_has_explicit_device_and_cpu_sync() {
    let tracker = Box::new(TrackingDmaOp::new().with_next_dma_addr(0x4000));
    let tracker = Box::leak(tracker);
    let dev = DeviceDma::new_legacy(u64::MAX, tracker);
    let mut backing = [0u8; 128];
    let map = dev
        .map_streaming_slice(&mut backing, 64, DmaDirection::Bidirectional)
        .unwrap();

    tracker.clear();
    map.prepare_for_device_all();
    map.complete_for_cpu_all();
    drop(map);

    assert_eq!(tracker.count_sync_map_for_device(), 1);
    assert_eq!(tracker.count_sync_map_for_cpu(), 1);
    assert!(
        tracker
            .operations()
            .iter()
            .any(|op| matches!(op, DmaOperation::UnmapStreaming { size: 128 }))
    );
}

#[test]
fn streaming_map_for_device_syncs_after_mapping() {
    let tracker = Box::new(TrackingDmaOp::new().with_next_dma_addr(0x4000));
    let tracker = Box::leak(tracker);
    let dev = DeviceDma::new_legacy(u64::MAX, tracker);
    let mut backing = [0u8; 128];

    tracker.clear();
    let map = dev
        .map_streaming_slice_for_device(&mut backing, 64, DmaDirection::FromDevice)
        .unwrap();

    assert_eq!(tracker.count_sync_map_for_device(), 1);
    assert_eq!(tracker.count_sync_map_for_cpu(), 0);
    assert!(tracker.operations().iter().any(|op| matches!(
        op,
        DmaOperation::SyncMapForDevice {
            size: 128,
            direction: DmaDirection::FromDevice,
            ..
        }
    )));
    drop(map);
}

#[test]
fn streaming_write_for_device_syncs_after_cpu_write() {
    let tracker = Box::new(TrackingDmaOp::new().with_next_dma_addr(0x4000));
    let tracker = Box::leak(tracker);
    let dev = DeviceDma::new_legacy(u64::MAX, tracker);
    let mut backing = [0u8; 16];
    let mut map = dev
        .map_streaming_slice(&mut backing, 4, DmaDirection::ToDevice)
        .unwrap();

    tracker.clear();
    map.write_for_device(4, |data| data.copy_from_slice(&[1, 2, 3, 4]));

    assert_eq!(map.read_cpu(0), Some(1));
    assert_eq!(tracker.count_sync_map_for_device(), 1);
    assert!(tracker.operations().iter().any(|op| matches!(
        op,
        DmaOperation::SyncMapForDevice {
            size: 4,
            direction: DmaDirection::ToDevice,
            ..
        }
    )));
}

#[test]
fn streaming_read_from_device_syncs_before_cpu_read_and_copies_bounce_buffer() {
    let tracker = Box::new(TrackingDmaOp::new().with_next_dma_addr(0x80));
    let tracker = Box::leak(tracker);
    let dev = DeviceDma::new_legacy(0xff, tracker);
    let mut backing = [1u8; 16];
    let map = dev
        .map_streaming_slice(&mut backing, 16, DmaDirection::FromDevice)
        .unwrap();

    assert!(map.bounce_ptr().is_some());
    unsafe {
        map.bounce_ptr()
            .unwrap()
            .as_ptr()
            .write_bytes(0x5a, backing.len());
    }

    tracker.clear();
    let first = map.read_from_device(4, |data| data[0]);

    assert_eq!(first, 0x5a);
    assert_eq!(backing[0], 0x5a);
    assert_eq!(tracker.count_sync_map_for_cpu(), 1);
    assert!(tracker.operations().iter().any(|op| matches!(
        op,
        DmaOperation::SyncMapForCpu {
            size: 4,
            direction: DmaDirection::FromDevice,
            ..
        }
    )));
}

#[test]
fn streaming_bounce_buffer_copies_back_on_cpu_sync() {
    let tracker = Box::new(TrackingDmaOp::new().with_next_dma_addr(0x80));
    let tracker = Box::leak(tracker);
    let dev = DeviceDma::new_legacy(0xff, tracker);
    let mut backing = [1u8; 16];
    let map = dev
        .map_streaming_slice(&mut backing, 16, DmaDirection::FromDevice)
        .unwrap();

    assert!(map.bounce_ptr().is_some());
    unsafe {
        map.bounce_ptr()
            .unwrap()
            .as_ptr()
            .write_bytes(0x5a, backing.len());
    }
    map.complete_for_cpu_all();
    drop(map);

    assert_eq!(backing, [0x5a; 16]);
}

#[test]
fn contiguous_array_high_level_accessors_sync_expected_ranges() {
    let (dev, tracker) = new_tracking_device();
    let mut tx = dev
        .contiguous_array_zero_with_align::<u8>(16, 64, DmaDirection::ToDevice)
        .unwrap();

    tracker.clear();
    tx.write_for_device(4, |data| data.copy_from_slice(&[1, 2, 3, 4]));
    assert_eq!(tracker.count_sync_alloc_for_device(), 1);
    assert!(tracker.operations().iter().any(|op| matches!(
        op,
        DmaOperation::SyncAllocForDevice {
            size: 4,
            direction: DmaDirection::ToDevice,
            ..
        }
    )));

    let mut rx = dev
        .contiguous_array_zero_with_align::<u8>(16, 64, DmaDirection::FromDevice)
        .unwrap();
    rx.copy_from_slice_cpu(&[5, 6, 7, 8]);
    tracker.clear();
    let mut out = [0u8; 4];
    rx.copy_from_device_to_slice(&mut out);
    assert_eq!(out, [5, 6, 7, 8]);
    assert_eq!(tracker.count_sync_alloc_for_cpu(), 1);
    assert!(tracker.operations().iter().any(|op| matches!(
        op,
        DmaOperation::SyncAllocForCpu {
            size: 4,
            direction: DmaDirection::FromDevice,
            ..
        }
    )));
}

#[test]
fn contiguous_box_high_level_accessors_sync_for_device_and_cpu() {
    let (dev, tracker) = new_tracking_device();
    let mut tx = dev
        .contiguous_box_zero_with_align::<Descriptor>(64, DmaDirection::ToDevice)
        .unwrap();

    tracker.clear();
    tx.write_for_device(Descriptor {
        addr: 0x1000,
        len: 64,
        flags: 1,
    });
    assert_eq!(tracker.count_sync_alloc_for_device(), 1);

    let mut rx = dev
        .contiguous_box_zero_with_align::<Descriptor>(64, DmaDirection::FromDevice)
        .unwrap();
    rx.write_cpu(Descriptor {
        addr: 0x2000,
        len: 128,
        flags: 2,
    });

    tracker.clear();
    let value = rx.read_from_device();
    assert_eq!(value.addr, 0x2000);
    assert_eq!(tracker.count_sync_alloc_for_cpu(), 1);
}

#[test]
fn allocation_rejects_backend_address_outside_mask() {
    let (dev, tracker) = new_tracking_device();
    tracker.force_next_dma_addr(0x1_0000_0000);
    let result = dev
        .with_constraints(DmaConstraints::new(u32::MAX as u64))
        .coherent_array_zero_with_align::<u8>(4096, 4096);

    assert!(matches!(result, Err(DmaError::DmaMaskNotMatch { .. })));
}

#[test]
fn explicit_dma_domain_survives_constraint_updates() {
    let tracker = Box::new(TrackingDmaOp::new());
    let tracker = Box::leak(tracker);
    let domain = DmaDomainId::from_raw(0x42);
    let dev = DeviceDma::new(domain, u64::MAX, tracker);

    assert_eq!(dev.domain_id(), domain);
    assert_eq!(
        dev.with_constraints(DmaConstraints::new(u32::MAX as u64))
            .domain_id(),
        domain
    );
}

#[test]
fn low_32bit_allocations_are_validated() {
    let tracker = Box::new(TrackingDmaOp::new().with_next_dma_addr(0xffff_f000));
    let tracker = Box::leak(tracker);
    let dev = DeviceDma::new_legacy(u32::MAX as u64, tracker);
    let buff = dev
        .contiguous_array_zero_with_align::<u8>(0x1000, 0x1000, DmaDirection::ToDevice)
        .unwrap();

    assert!(buff.dma_addr().as_u64() <= u32::MAX as u64);
    assert!(tracker.operations().iter().any(|op| matches!(
        op,
        DmaOperation::AllocContiguous {
            mask,
            ..
        } if *mask == u32::MAX as u64
    )));
}

#[test]
fn streaming_map_rejects_backend_address_outside_mask() {
    let (dev, tracker) = new_tracking_device();
    let mut backing = [0u8; 128];
    tracker.force_next_dma_addr(0x1_0000_0000);

    let result = dev
        .with_constraints(DmaConstraints::new(u32::MAX as u64))
        .map_streaming_slice(&mut backing, 64, DmaDirection::FromDevice);

    assert!(matches!(result, Err(DmaError::DmaMaskNotMatch { .. })));
}

#[test]
fn pool_reuses_contiguous_buffers_without_implicit_zeroing() {
    let (dev, tracker) = new_tracking_device();
    let pool = dev.contiguous_buffer_pool(
        core::alloc::Layout::from_size_align(64, 64).unwrap(),
        DmaDirection::ToDevice,
        1,
    );

    {
        let mut buff = pool.alloc().unwrap();
        unsafe {
            buff.as_mut_slice_cpu()[0] = 0x7e;
        }
    }

    tracker.clear();
    let buff = pool.alloc().unwrap();
    assert_eq!(buff.as_slice_cpu()[0], 0x7e);
    assert_eq!(tracker.count_sync_alloc_for_device(), 0);
    assert_eq!(tracker.count_sync_alloc_for_cpu(), 0);
}
