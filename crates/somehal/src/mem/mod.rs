use core::{cell::UnsafeCell, ops::Deref};

pub use os_helper::memory::{MemoryDescriptor, MemoryType};

use crate::ArchTrait;

pub(crate) mod address;
pub(crate) mod ram;

static MEMORY_MAP: StaticCell<heapless::Vec<MemoryDescriptor, 64>> =
    StaticCell::new(Some(heapless::Vec::new()));

static mut KERNEL_LINER_OFFSET_CURRENT: usize = 0;

pub const MB: usize = 1024 * 1024;

pub(crate) fn set_mmu_enabled() {
    unsafe {
        KERNEL_LINER_OFFSET_CURRENT = crate::consts::KERNEL_LINER_OFFSET;
    }
}

pub fn virt_to_phys(vaddr: usize) -> usize {
    vaddr - unsafe { KERNEL_LINER_OFFSET_CURRENT }
}

pub fn phys_to_virt(paddr: usize) -> usize {
    paddr + unsafe { KERNEL_LINER_OFFSET_CURRENT }
}

pub(crate) fn early_init() {
    ram::init();
    crate::fdt::save_fdt();
}

pub(crate) fn kernel_range() -> core::ops::Range<usize> {
    let kernel = crate::arch::Arch::kernel_code().as_ptr_range();
    let start = kernel.start as usize;
    let end = ram::current() as usize;
    start..end
}

pub fn page_size() -> usize {
    unsafe extern "C" {
        static PAGE_SIZE: usize;
    }
    core::ptr::addr_of!(PAGE_SIZE) as usize
}

pub(crate) fn add_memory_descriptor(desc: MemoryDescriptor) {
    MEMORY_MAP.update(|map| {
        let _ = map.push(desc);
    });
}

pub fn get_memory_map() -> &'static [MemoryDescriptor] {
    &MEMORY_MAP
}

pub(crate) struct StaticCell<T> {
    value: UnsafeCell<Option<T>>,
}

impl<T> StaticCell<T> {
    pub const fn new(v: Option<T>) -> Self {
        StaticCell {
            value: UnsafeCell::new(v),
        }
    }

    pub fn set(&self, v: T) {
        unsafe {
            *self.value.get() = Some(v);
        }
    }

    pub fn update<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        unsafe {
            let val = &mut *self.value.get();
            f(val.as_mut().unwrap())
        }
    }
}

impl<T> Deref for StaticCell<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { (*self.value.get()).as_ref().unwrap() }
    }
}

unsafe impl<T> Sync for StaticCell<T> {}
unsafe impl<T> Send for StaticCell<T> {}
