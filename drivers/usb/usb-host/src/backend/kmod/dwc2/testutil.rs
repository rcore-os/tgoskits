//! 共享的 host 单元测试基础设施：
//! - `TestKernel`：确定性 DMA 假分配器（与 ehci 测试同款 mock）
//! - 内存兜底寄存器窗口（`Vec<u32>` + `Dwc2Registers`），按
//!   `register_structs!` 布局直接映射，无需真实硬件
//! - `Dwc2::new` 需要的只读硬件参数（GHWCFG2/GHWCFG4）预置

extern crate std;

use alloc::{
    alloc::{alloc_zeroed, dealloc},
    sync::Arc,
    vec,
    vec::Vec,
};
use core::{
    alloc::Layout,
    num::NonZeroUsize,
    ptr::NonNull,
    sync::atomic::{AtomicU64, Ordering as AtomicOrdering},
};

use dma_api::{
    DmaAllocHandle, DmaCoherency, DmaConstraints, DmaDeviceInfo, DmaDirection, DmaDomainId,
    DmaError, DmaMapHandle, DmaOp,
};

use super::{
    channel::{Dwc2ChannelCompletions, Dwc2PeriodicSchedule, HostChannelPool},
    reg::{Dwc2Registers, GHWCFG2, GHWCFG4},
};
use crate::backend::kmod::osal::{Kernel, KernelOp};

pub struct TestKernel;

static TEST_DMA_ADDR: AtomicU64 = AtomicU64::new(0x1000);

impl DmaOp for TestKernel {
    fn page_size(&self) -> usize {
        4096
    }

    unsafe fn alloc_contiguous(
        &self,
        constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        // SAFETY: The test kernel models contiguous DMA with the same
        // heap-backed allocation used for coherent DMA below.
        unsafe { self.alloc_coherent(constraints, layout) }
    }

    unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
        // SAFETY: Handles returned by `alloc_contiguous` are created by
        // `alloc_coherent`, so they must be released through the same mock
        // deallocation path.
        unsafe { self.dealloc_coherent(handle) }.expect("test coherent DMA release must succeed")
    }

    unsafe fn alloc_coherent(
        &self,
        constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        // SAFETY: Unit tests request valid `Layout` values. The returned
        // pointer is either null or points to a heap allocation owned by the
        // DMA handle until `dealloc_coherent` consumes it.
        let ptr = unsafe { alloc_zeroed(layout) };
        let ptr = NonNull::new(ptr)?;
        let align = constraints.align.max(layout.align()).max(1) as u64;
        let size = layout.size().max(1) as u64;
        let current = TEST_DMA_ADDR.fetch_add(size + align, AtomicOrdering::Relaxed);
        let dma_addr = (current + align - 1) & !(align - 1);
        // SAFETY: `ptr` and `layout` describe the allocation above, and
        // `dma_addr` is a deterministic fake bus address used only by unit
        // tests that never reaches real hardware.
        Some(unsafe { DmaAllocHandle::new(ptr, ptr, dma_addr.into(), layout) })
    }

    unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) -> Result<(), DmaError> {
        // SAFETY: The mock only creates coherent handles from `alloc_zeroed`
        // with the stored layout, so deallocating with the same layout
        // releases exactly that allocation.
        unsafe { dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
        Ok(())
    }

    unsafe fn map_streaming(
        &self,
        _constraints: DmaConstraints,
        addr: NonNull<u8>,
        size: NonZeroUsize,
        _direction: DmaDirection,
    ) -> core::result::Result<DmaMapHandle, DmaError> {
        let layout = Layout::from_size_align(size.get(), 1)?;
        // SAFETY: This mock streaming map does not transfer ownership of
        // `addr`; it records the caller-provided live buffer and a fake bus
        // address for tests that only inspect programming values.
        Ok(unsafe { DmaMapHandle::new(addr, (addr.as_ptr() as u64).into(), layout, None) })
    }

    unsafe fn unmap_streaming(&self, _handle: DmaMapHandle) {}
}

impl KernelOp for TestKernel {
    fn delay(&self, _duration: core::time::Duration) {}
}

pub static TEST_KERNEL: TestKernel = TestKernel;

pub fn test_kernel() -> Kernel {
    Kernel::new(
        DmaDeviceInfo::new(
            DmaDomainId::Direct,
            DmaCoherency::NonCoherent,
            DmaConstraints::new(u64::MAX),
        ),
        &TEST_KERNEL,
    )
}

/// 测试寄存器窗口：4KB 零初始化，按 `Dwc2Regs` 布局映射。
/// 返回持有内存的 `Vec` 与指向同一内存的寄存器句柄；`Vec` 存活期内
/// 不得重新分配。
pub fn test_regs() -> (Vec<u32>, Dwc2Registers) {
    let mut backing = vec![0u32; 1024];
    let base = NonNull::new(backing.as_mut_ptr().cast::<u8>()).unwrap();
    (backing, Dwc2Registers::new(base))
}

/// 预置只读硬件参数（GHWCFG2 为 InternalDMA + 8 通道，GHWCFG4 支持
/// 描述符 DMA 与 16 位 UTMI）。只读寄存器无法经 tock 写入，直接按
/// `register_structs!` 布局写回兜底内存。
pub fn preset_hw_caps(backing: &mut Vec<u32>) {
    let base = backing.as_mut_ptr().cast::<u8>();
    unsafe {
        // GHWCFG2 @ 0x048（InternalDMA、NUM_HOST_CHAN = 7 → 8 通道）。
        core::ptr::write_volatile(
            base.add(0x048).cast::<u32>(),
            (GHWCFG2::ARCHITECTURE::InternalDma + GHWCFG2::NUM_HOST_CHAN.val(7)).value,
        );
        // GHWCFG4 @ 0x050（DESC_DMA 支持、UTMI 16 位）。
        core::ptr::write_volatile(
            base.add(0x050).cast::<u32>(),
            (GHWCFG4::DESC_DMA::SET + GHWCFG4::UTMI_PHY_DATA_WIDTH::Width16).value,
        );
    }
}

/// 构造通道池基础设施
/// 返回的 `Vec<u32>` 是寄存器兜底内存，调用方必须保持存活到测试结束。
#[allow(clippy::type_complexity)]
pub fn channel_fixture(
    channels: u8,
) -> (
    Vec<u32>,
    Dwc2Registers,
    Kernel,
    Dwc2ChannelCompletions,
    HostChannelPool,
) {
    let (backing, regs) = test_regs();
    let kernel = test_kernel();
    let completions = Dwc2ChannelCompletions::new();
    let periodic = Arc::new(Dwc2PeriodicSchedule::new(&kernel).unwrap());
    let pool = HostChannelPool::new(channels, completions.clone(), periodic);
    (backing, regs, kernel, completions, pool)
}
