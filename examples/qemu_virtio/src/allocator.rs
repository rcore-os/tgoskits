//! 简易内存分配器
//! 使用 buddy_system_allocator

use buddy_system_allocator::LockedHeap;
use core::alloc::Layout;

/// 堆大小：700MB
const HEAP_SIZE: usize = 1024*1024*400;

/// 静态堆空间
static mut HEAP_SPACE: [u8; HEAP_SIZE] = [0; HEAP_SIZE];

/// 全局堆分配器
#[global_allocator]
static HEAP: LockedHeap<32> = LockedHeap::empty();

/// 初始化堆分配器
pub fn init_heap() {
    unsafe {
        HEAP.lock()
            .init(HEAP_SPACE.as_ptr() as usize, HEAP_SIZE);
    }
    log::info!("堆分配器初始化完成: 大小 {} MB", HEAP_SIZE / (1024 * 1024));
}

/// 分配错误处理
#[alloc_error_handler]
fn alloc_error_handler(layout: Layout) -> ! {
    panic!("内存分配失败: {:?}", layout);
}
