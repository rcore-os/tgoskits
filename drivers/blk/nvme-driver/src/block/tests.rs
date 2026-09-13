use alloc::{
    alloc::{alloc_zeroed, dealloc},
    vec,
};
use core::{alloc::Layout, num::NonZeroUsize, ptr::NonNull};

use dma_api::{
    DeviceDma, DmaAllocHandle, DmaCoherency, DmaConstraints, DmaDeviceInfo, DmaDirection,
    DmaDomainId, DmaError, DmaMapHandle, DmaOp,
};

use super::*;
use crate::Config;

struct TestDmaOp;

impl DmaOp for TestDmaOp {
    fn page_size(&self) -> usize {
        4096
    }

    unsafe fn alloc_contiguous(
        &self,
        _constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        // SAFETY: `layout` is valid and the returned allocation is owned by the test DMA handle.
        let ptr = NonNull::new(unsafe { alloc_zeroed(layout) })?;
        // SAFETY: `ptr` names the allocation above for exactly `layout` bytes in this direct domain.
        Some(unsafe {
            DmaAllocHandle::new(ptr, ptr, (ptr.as_ptr() as usize as u64).into(), layout)
        })
    }

    unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
        // SAFETY: the DMA contract returns the same live allocation and layout produced above.
        unsafe { dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
    }

    unsafe fn alloc_coherent(
        &self,
        constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        // SAFETY: coherent test allocations use the same ownership contract as contiguous ones.
        unsafe { self.alloc_contiguous(constraints, layout) }
    }

    unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) -> Result<(), DmaError> {
        // SAFETY: `handle` was allocated by the matching coherent allocation method.
        unsafe { self.dealloc_contiguous(handle) };
        Ok(())
    }

    unsafe fn map_streaming(
        &self,
        _constraints: DmaConstraints,
        addr: NonNull<u8>,
        size: NonZeroUsize,
        _direction: DmaDirection,
    ) -> Result<DmaMapHandle, DmaError> {
        let layout = Layout::from_size_align(size.get(), 1).map_err(DmaError::LayoutError)?;
        Ok(
            // SAFETY: the caller guarantees `addr..addr + size` remains valid for this mapping.
            unsafe {
                DmaMapHandle::new(addr, (addr.as_ptr() as usize as u64).into(), layout, None)
            },
        )
    }

    unsafe fn unmap_streaming(&self, _handle: DmaMapHandle) {}
}

fn test_dma() -> DeviceDma {
    static OP: TestDmaOp = TestDmaOp;
    DeviceDma::new(
        DmaDeviceInfo::new(
            DmaDomainId::Direct,
            DmaCoherency::Coherent,
            DmaConstraints::new(u64::MAX),
        ),
        &OP,
    )
}

#[test]
fn ready_online_smp_does_not_reissue_resources_or_change_device_info() {
    let mut registers = vec![0_u64; 0x2000 / core::mem::size_of::<u64>()];
    registers[0] = 63;
    let config = Config::msix(4096, [0, 1, 2]).unwrap();
    // SAFETY: the aligned register backing stays alive and exclusively borrowed for this test.
    let nvme = unsafe {
        Nvme::from_borrowed_registers_for_test(
            NonNull::new(registers.as_mut_ptr().cast()).unwrap(),
            test_dma(),
            config,
        )
    }
    .unwrap();
    let namespace = Namespace {
        id: 1,
        lba_size: 512,
        lba_count: 4096,
        metadata_size: 0,
    };
    let mut controller = NvmeBlockDriver::from_nvme_with_queue_depth(nvme, 32);
    controller.namespace = Some(namespace);
    controller.initialization_started = true;
    controller.ready = true;
    controller.bootstrap_target = 2;
    controller.next_queue_id = 2;
    let expected_info = controller.device_info();

    for target_queues in [2, 1] {
        let mut update = controller
            .advance(ControllerEvent::OnlineSmp { target_queues })
            .unwrap();

        assert_eq!(update.controller_state(), ControllerState::Ready);
        assert!(update.take_queues().is_empty());
        assert!(update.take_irq_endpoints().is_empty());
        assert_eq!(update.take_device_info(), None);
        assert_eq!(controller.device_info(), expected_info);
    }
}

#[test]
fn rearm_during_initialization_preserves_waiting_for_irq_state() {
    let mut registers = vec![0_u64; 0x2000 / core::mem::size_of::<u64>()];
    registers[0] = 63;
    let config = Config::msix(4096, [0, 1]).unwrap();
    // SAFETY: the aligned register backing stays alive and exclusively borrowed for this test.
    let nvme = unsafe {
        Nvme::from_borrowed_registers_for_test(
            NonNull::new(registers.as_mut_ptr().cast()).unwrap(),
            test_dma(),
            config,
        )
    }
    .unwrap();
    let mut controller = NvmeBlockDriver::from_nvme(nvme);
    controller.initialization_started = true;

    let update = controller
        .advance(ControllerEvent::Rearm { source_id: 0 })
        .unwrap();

    assert_eq!(update.controller_state(), ControllerState::WaitingForIrq);
}
