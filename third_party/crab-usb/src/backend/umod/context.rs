use std::sync::Arc;

use crate::err::*;
use libusb1_sys::*;

pub struct Context(*mut libusb1_sys::libusb_context);

unsafe impl Send for Context {}
unsafe impl Sync for Context {}

impl Context {
    pub fn new() -> Result<Arc<Self>> {
        let mut ctx = std::ptr::null_mut();
        usb!(libusb1_sys::libusb_init(&mut ctx))?;
        Ok(Arc::new(Self(ctx)))
    }

    pub fn device_list(&self) -> crate::err::Result<DeviceList> {
        let mut list: *const *mut libusb_device = std::ptr::null_mut();
        let count = unsafe { libusb1_sys::libusb_get_device_list(self.0, &mut list) };
        Ok(DeviceList {
            list,
            len: count as usize,
        })
    }

    pub fn handle_events(&self) -> Result<()> {
        usb!(libusb1_sys::libusb_handle_events(self.0))?;
        Ok(())
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        unsafe {
            libusb1_sys::libusb_exit(self.0);
        }
    }
}

pub struct DeviceList {
    list: *const *mut libusb_device,
    len: usize,
}

impl Iterator for DeviceList {
    type Item = *mut libusb_device;

    fn next(&mut self) -> Option<Self::Item> {
        if self.len == 0 {
            return None;
        }
        unsafe {
            let device = *self.list;
            self.list = self.list.add(1);
            self.len -= 1;
            Some(device)
        }
    }
}

impl Drop for DeviceList {
    fn drop(&mut self) {
        if self.len == 0 {
            return;
        }
        unsafe {
            libusb1_sys::libusb_free_device_list(self.list, 1);
        }
    }
}
