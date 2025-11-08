use core::{cell::UnsafeCell, ops::Deref};

#[derive(Debug, Clone, Copy)]
pub struct MemoryDescriptor {
    pub physical_start: usize,
    pub size_in_bytes: usize,
    pub memory_type: MemoryType,
}

#[derive(Debug, Clone, Copy)]
pub enum MemoryType {
    Usable,
    Reserved,
}

#[unsafe(link_section = ".data")]
static MEMORY_MAP: StaticCell<heapless::Vec<MemoryDescriptor, 64>> =
    StaticCell::new(Some(heapless::Vec::new()));

pub const MB: usize = 1024 * 1024;

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

    pub fn update<F>(&self, f: F)
    where
        F: FnOnce(&mut T),
    {
        unsafe {
            if let Some(ref mut val) = *self.value.get() {
                f(val);
            }
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
