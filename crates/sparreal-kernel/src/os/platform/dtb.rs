use core::{fmt::Debug, ptr::NonNull};

use fdt_raw::Fdt;

#[derive(Clone, Copy)]
pub struct DeviceTree {
    ptr: usize,
}

impl Debug for DeviceTree {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "DeviceTree@{:p}", self.ptr as *const u8)
    }
}

impl DeviceTree {
    pub(crate) fn new(ptr: NonNull<u8>) -> Self {
        DeviceTree {
            ptr: ptr.as_ptr() as usize,
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        let ptr = self.ptr as *mut u8;
        let fdt = unsafe { Fdt::from_ptr(ptr).unwrap() };
        fdt.as_slice()
    }
}
