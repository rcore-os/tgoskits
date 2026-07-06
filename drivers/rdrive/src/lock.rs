use alloc::{
    boxed::Box,
    sync::{Arc, Weak},
};
use core::{
    any::Any,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicI64, Ordering},
};

use rdif_base::DriverGeneric;

use crate::{Descriptor, Pid, get_pid};

pub struct DeviceOwner {
    lock: Arc<LockInner>,
}

impl DeviceOwner {
    pub fn new<T: DriverGeneric>(descriptor: Descriptor, device: T) -> Self {
        Self {
            lock: Arc::new(LockInner::new(descriptor, Box::into_raw(Box::new(device)))),
        }
    }

    pub fn weak<T: DriverGeneric>(&self) -> Result<Device<T>, GetDeviceError> {
        Device::new(&self.lock)
    }

    pub fn is<T: DriverGeneric>(&self) -> bool {
        unsafe { &*self.lock.ptr }.is::<T>()
    }
}

impl Drop for LockInner {
    fn drop(&mut self) {
        unsafe {
            let ptr = self.ptr;
            let _ = Box::from_raw(ptr);
        }
    }
}

struct LockInner {
    borrowed: AtomicI64,
    ptr: *mut dyn Any,
    descriptor: Descriptor,
}

unsafe impl Send for LockInner {}
unsafe impl Sync for LockInner {}

impl LockInner {
    fn new(descriptor: Descriptor, ptr: *mut dyn Any) -> Self {
        Self {
            borrowed: AtomicI64::new(-1),
            ptr,
            descriptor,
        }
    }

    pub fn try_lock(self: &Arc<Self>, pid: Pid) -> Result<(), GetDeviceError> {
        let mut pid = pid;
        if pid.is_not_set() {
            pid = Pid::INVALID.into();
        }

        let id: usize = pid.into();

        match self.borrowed.compare_exchange(
            Pid::NOT_SET as _,
            id as _,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
            Ok(_) => Ok(()),
            Err(old) => {
                if old as usize == Pid::INVALID {
                    Err(GetDeviceError::UsedByUnknown)
                } else {
                    let pid: Pid = (old as usize).into();
                    Err(GetDeviceError::UsedByOthers(pid))
                }
            }
        }
    }

    pub fn lock(self: &Arc<Self>) -> Result<(), GetDeviceError> {
        let pid = get_pid();
        loop {
            match self.try_lock(pid) {
                Ok(guard) => return Ok(guard),
                Err(GetDeviceError::UsedByOthers(_)) | Err(GetDeviceError::UsedByUnknown) => {
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
    }
}

pub struct DeviceGuard<T> {
    lock: Arc<LockInner>,
    ptr: *mut T,
}

unsafe impl<T> Send for DeviceGuard<T> {}

impl<T> Drop for DeviceGuard<T> {
    fn drop(&mut self) {
        self.lock
            .borrowed
            .store(Pid::NOT_SET as _, Ordering::Release);
    }
}

impl<T> Deref for DeviceGuard<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.ptr }
    }
}

impl<T> DerefMut for DeviceGuard<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.ptr }
    }
}

impl<T> DeviceGuard<T> {
    pub fn descriptor(&self) -> &Descriptor {
        &self.lock.descriptor
    }
}

pub struct Device<T> {
    lock: Weak<LockInner>,
    descriptor: Descriptor,
    ptr: *mut T,
}

impl<T> Clone for Device<T> {
    fn clone(&self) -> Self {
        Self {
            lock: self.lock.clone(),
            descriptor: self.descriptor.clone(),
            ptr: self.ptr,
        }
    }
}

unsafe impl<T> Send for Device<T> {}
unsafe impl<T> Sync for Device<T> {}

impl<T: Any> Device<T> {
    fn new(lock: &Arc<LockInner>) -> Result<Self, GetDeviceError> {
        let ptr = match unsafe { &*lock.ptr }.downcast_ref::<T>() {
            Some(v) => v as *const T as *mut T,
            None => return Err(GetDeviceError::TypeNotMatch),
        };

        Ok(Self {
            lock: Arc::downgrade(lock),
            descriptor: lock.descriptor.clone(),
            ptr,
        })
    }

    /// Locks the device for exclusive mutable access.
    ///
    /// Not hard-IRQ safe: this may spin until the current owner drops the
    /// device and it queries OS ownership state. Hard IRQ handlers must use a
    /// pre-registered IRQ endpoint or other IRQ-side state instead of locking a
    /// rdrive device.
    pub fn lock(&self) -> Result<DeviceGuard<T>, GetDeviceError> {
        let lock = self.lock.upgrade().ok_or(GetDeviceError::DeviceReleased)?;
        lock.lock()?;

        Ok(DeviceGuard {
            lock,
            ptr: self.ptr,
        })
    }
    pub fn try_lock(&self) -> Result<DeviceGuard<T>, GetDeviceError> {
        let lock = self.lock.upgrade().ok_or(GetDeviceError::DeviceReleased)?;
        lock.try_lock(get_pid())?;

        Ok(DeviceGuard {
            lock,
            ptr: self.ptr,
        })
    }

    pub fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }

    pub fn type_name(&self) -> &'static str {
        core::any::type_name::<T>()
    }

    /// Returns the raw device pointer without taking the rdrive device lock.
    ///
    /// # Safety
    ///
    /// Not hard-IRQ safe: this is not an interrupt-context escape hatch. The
    /// caller must prove that the device is still alive and that no concurrent
    /// mutable or shared access can race with the returned pointer. Hard IRQ
    /// handlers must use pre-registered IRQ-side state instead of reaching back
    /// into rdrive devices.
    pub unsafe fn force_use(&self) -> *mut T {
        self.ptr
    }
}

impl<T: DriverGeneric> Device<T> {
    pub fn downcast<T2: 'static>(&self) -> Result<Device<T2>, GetDeviceError> {
        let lock = self.lock.upgrade().ok_or(GetDeviceError::DeviceReleased)?;

        let t2_any = unsafe { &mut *self.ptr }
            .raw_any_mut()
            .ok_or(GetDeviceError::TypeNotMatch)?;

        let t2_type = t2_any
            .downcast_mut::<T2>()
            .ok_or(GetDeviceError::TypeNotMatch)?;

        Ok(Device {
            lock: Arc::downgrade(&lock),
            descriptor: self.descriptor.clone(),
            ptr: t2_type as *mut T2,
        })
    }
}

#[derive(thiserror::Error, Debug, Clone, Copy)]
pub enum GetDeviceError {
    #[error("Used by pid: {0:?}")]
    UsedByOthers(Pid),
    #[error("Used by unknown pid")]
    UsedByUnknown,
    #[error("Device type not match")]
    TypeNotMatch,
    #[error("Device released")]
    DeviceReleased,
    #[error("Device not found")]
    NotFound,
}
