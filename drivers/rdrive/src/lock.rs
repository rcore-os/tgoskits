use alloc::{
    boxed::Box,
    sync::{Arc, Weak},
};
use core::{
    any::{Any, TypeId},
    mem::ManuallyDrop,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicUsize, Ordering},
};

use rdif_base::DriverGeneric;

use crate::{Descriptor, Pid, get_pid, relax};

pub struct DeviceOwner {
    lock: Arc<LockInner>,
}

impl DeviceOwner {
    pub fn new<T: DriverGeneric>(descriptor: Descriptor, device: T) -> Self {
        Self {
            lock: Arc::new(LockInner::new(descriptor, device)),
        }
    }

    pub fn weak<T: DriverGeneric>(&self) -> Result<Device<T>, GetDeviceError> {
        Device::new(&self.lock)
    }

    pub fn is<T: DriverGeneric>(&self) -> bool {
        self.lock.type_id == TypeId::of::<T>()
    }
}

impl Drop for LockInner {
    fn drop(&mut self) {
        // SAFETY: new transfers one Box into this pointer. The final Arc is
        // gone, so no guard can still access the allocation.
        unsafe { drop(Box::from_raw(self.ptr)) };
    }
}

struct LockInner {
    borrowed: AtomicUsize,
    ptr: *mut dyn Any,
    type_id: TypeId,
    descriptor: Descriptor,
}

// SAFETY: construction requires a Send driver. The allocation remains owned
// by the Arc; mutable access requires the exclusive borrow bit. Type checks
// read separate immutable metadata without borrowing the driver.
unsafe impl Send for LockInner {}
// SAFETY: shared handles only access immutable metadata and the atomic gate.
unsafe impl Sync for LockInner {}

impl LockInner {
    fn new<T: DriverGeneric>(descriptor: Descriptor, device: T) -> Self {
        Self {
            borrowed: AtomicUsize::new(Pid::NOT_SET),
            ptr: Box::into_raw(Box::new(device)),
            type_id: TypeId::of::<T>(),
            descriptor,
        }
    }

    /// Acquire exclusive access. The PID is diagnostic metadata only.
    fn try_lock(&self, pid: Pid) -> Result<(), GetDeviceError> {
        let owner = if pid.is_not_set() {
            Pid::INVALID
        } else {
            pid.raw()
        };
        // Acquire observes device writes published by the previous guard's Drop.
        self.borrowed
            .compare_exchange(Pid::NOT_SET, owner, Ordering::Acquire, Ordering::Relaxed)
            .map(|_| ())
            .map_err(|owner| {
                if owner == Pid::INVALID {
                    GetDeviceError::UsedByUnknown
                } else {
                    GetDeviceError::UsedByOthers(owner.into())
                }
            })
    }

    fn lock(&self) -> Result<(), GetDeviceError> {
        let pid = get_pid();
        loop {
            match self.try_lock(pid) {
                Ok(()) => return Ok(()),
                Err(GetDeviceError::UsedByOthers(_)) | Err(GetDeviceError::UsedByUnknown) => {
                    relax();
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
    }
}

/// Exclusive device borrow. Only dropping this guard releases the borrow.
///
/// Non-Send projections must not cross threads:
/// ```compile_fail
/// fn require_send<T: Send>() {}
/// require_send::<rdrive::DeviceGuard<std::rc::Rc<()>>>();
/// ```
///
/// Moving it to a worker transfers the borrow; the acquiring process's exit
/// does not revoke it. Leaking a guard keeps the device borrowed.
pub struct DeviceGuard<T> {
    lock: Arc<LockInner>,
    ptr: *mut T,
}

// SAFETY: the guard transfers exclusive access and pins the driver allocation.
// A projected target must itself permit transfer between threads.
unsafe impl<T: Send> Send for DeviceGuard<T> {}

impl<T> Drop for DeviceGuard<T> {
    fn drop(&mut self) {
        // No other path can unlock while this guard or its references exist.
        self.lock.borrowed.store(Pid::NOT_SET, Ordering::Release);
    }
}

impl<T> Deref for DeviceGuard<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: this guard pins the allocation and retains the exclusive gate.
        unsafe { &*self.ptr }
    }
}

impl<T> DerefMut for DeviceGuard<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: the gate excludes other guards, and &mut self excludes other
        // references derived from this guard during this borrow.
        unsafe { &mut *self.ptr }
    }
}

impl<T> DeviceGuard<T> {
    pub fn descriptor(&self) -> &Descriptor {
        &self.lock.descriptor
    }
}

/// Weak access to a registered device. Subtype projection requires a guard.
/// ```compile_fail
/// fn project(device: rdrive::Device<rdrive::driver::Empty>) {
///     let _ = device.downcast::<u32>();
/// }
/// ```
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

// SAFETY: weak handles only dereference after upgrading and acquiring the gate.
unsafe impl<T: Send> Send for Device<T> {}
// SAFETY: concurrent callers serialize access through the shared borrow gate.
unsafe impl<T: Send> Sync for Device<T> {}

impl<T: Any> Device<T> {
    fn new(lock: &Arc<LockInner>) -> Result<Self, GetDeviceError> {
        if lock.type_id != TypeId::of::<T>() {
            return Err(GetDeviceError::TypeNotMatch);
        }
        // The recorded TypeId proves this erased allocation contains T. Do not
        // create a reference here: an existing guard may be mutably borrowing it.
        let ptr = lock.ptr.cast::<T>();

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

impl<T: DriverGeneric> DeviceGuard<T> {
    /// Project this exclusive borrow into a driver's subtype.
    ///
    /// The original guard is consumed, so the outer driver cannot replace the
    /// subtype while it is borrowed. On mismatch this guard is dropped and the
    /// device becomes available again.
    pub fn downcast<T2: 'static>(mut self) -> Result<DeviceGuard<T2>, GetDeviceError> {
        let ptr = self
            .raw_any_mut()
            .and_then(|subtype| subtype.downcast_mut::<T2>())
            .ok_or(GetDeviceError::TypeNotMatch)? as *mut T2;
        let guard = ManuallyDrop::new(self);
        // SAFETY: move the owning Arc without dropping the old guard (which
        // would release the gate). The new guard owns the same acquisition and
        // keeps the outer driver, including its projected subtype, alive.
        let lock = unsafe { core::ptr::read(&guard.lock) };
        Ok(DeviceGuard { lock, ptr })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::Empty;

    #[test]
    fn projection_retains_borrow_and_releases_on_mismatch() {
        struct Outer(u32);
        impl DriverGeneric for Outer {
            fn name(&self) -> &str {
                "outer"
            }
            fn raw_any_mut(&mut self) -> Option<&mut dyn Any> {
                Some(&mut self.0)
            }
        }
        let owner = DeviceOwner::new(Descriptor::new(), Outer(7));
        let device = owner.weak::<Outer>().unwrap();
        let mut projected = device.lock().unwrap().downcast::<u32>().unwrap();
        *projected = 9;
        assert!(device.try_lock().is_err());
        drop(projected);
        assert_eq!(device.lock().unwrap().0, 9);
        assert!(matches!(
            device.lock().unwrap().downcast::<u64>(),
            Err(GetDeviceError::TypeNotMatch)
        ));
        assert!(device.try_lock().is_ok());
    }

    #[test]
    fn diagnostic_pid_does_not_grant_recursive_access() {
        let owner = DeviceOwner::new(Descriptor::new(), Empty);
        let device = owner.weak::<Empty>().unwrap();
        owner.lock.try_lock(42usize.into()).unwrap();
        let guard = DeviceGuard {
            lock: owner.lock.clone(),
            ptr: device.ptr,
        };
        assert!(
            matches!(owner.lock.try_lock(42usize.into()), Err(GetDeviceError::UsedByOthers(pid)) if pid.raw() == 42)
        );
        assert!(matches!(
            owner.lock.try_lock(43usize.into()),
            Err(GetDeviceError::UsedByOthers(_))
        ));
        drop(guard);
        assert!(device.try_lock().is_ok());
    }

    #[test]
    fn unset_pid_still_acquires_exclusively() {
        let owner = DeviceOwner::new(Descriptor::new(), Empty);
        owner.lock.try_lock(Pid::NOT_SET.into()).unwrap();
        assert!(matches!(
            owner.lock.try_lock(1usize.into()),
            Err(GetDeviceError::UsedByUnknown)
        ));
    }

    #[test]
    fn pid_metadata_preserves_pointer_width() {
        let owner = DeviceOwner::new(Descriptor::new(), Empty);
        let pid = usize::MAX - 2;
        owner.lock.try_lock(pid.into()).unwrap();
        assert!(
            matches!(owner.lock.try_lock(1usize.into()), Err(GetDeviceError::UsedByOthers(held)) if held.raw() == pid)
        );
    }

    #[test]
    fn transferred_guard_keeps_device_alive_and_exclusive() {
        extern crate std;
        let owner = DeviceOwner::new(Descriptor::new(), Empty);
        let device = owner.weak::<Empty>().unwrap();
        let guard = device.lock().unwrap();
        drop(owner);
        std::thread::spawn(move || {
            assert_eq!(guard.name(), "Empty Driver");
            assert!(matches!(
                device.try_lock(),
                Err(GetDeviceError::UsedByUnknown) | Err(GetDeviceError::UsedByOthers(_))
            ));
            drop(guard);
            assert!(matches!(
                device.try_lock(),
                Err(GetDeviceError::DeviceReleased)
            ));
        })
        .join()
        .unwrap();
    }
}
