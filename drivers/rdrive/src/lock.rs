use alloc::{
    boxed::Box,
    sync::{Arc, Weak},
};
use core::{
    any::Any,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicU64, Ordering},
};

use rdif_base::DriverGeneric;

use crate::{Descriptor, Pid, get_pid, relax};

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

    /// Frees this device's lock if it is currently held by `pid`.
    ///
    /// Thin visibility shim over [`LockInner::reclaim_if_held_by`] so the
    /// registry (`manager.rs`) can reclaim locks without reaching into the
    /// otherwise-private `lock` field.
    pub(crate) fn reclaim_if_held_by(&self, pid: u32) -> bool {
        self.lock.reclaim_if_held_by(pid)
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

/// Marks a token's owner field as "no one holds this lock".
const OWNER_FREE: u32 = 0xFFFF_FFFF;
/// Marks a token's owner field as "held, but by an unknown/untracked pid"
/// (e.g. no `Osal` wired up yet, or acquired outside of a process context).
const OWNER_INVALID: u32 = 0xFFFF_FFFE;

/// Pack a generation counter and an owner into a single atomic word.
///
/// Low 32 bits: owner (`OWNER_FREE`, `OWNER_INVALID`, or a pid as `u32`).
/// High 32 bits: generation, bumped on every successful acquire/release so a
/// stale token (captured before a reclaim + re-acquire cycle) can never
/// CAS-succeed against a newer owner.
fn pack(generation: u32, owner: u32) -> u64 {
    ((generation as u64) << 32) | (owner as u64)
}

/// Inverse of [`pack`]: returns `(generation, owner)`.
fn unpack(token: u64) -> (u32, u32) {
    ((token >> 32) as u32, token as u32)
}

/// Encode a [`Pid`] as the owner half of a token.
fn owner_from_pid(pid: Pid) -> u32 {
    if pid.is_not_set() {
        OWNER_FREE
    } else if pid.is_invalid() {
        OWNER_INVALID
    } else {
        pid.raw() as u32
    }
}

/// Decode the owner half of a token back into a [`Pid`].
///
/// Only meaningful for a non-`OWNER_FREE` owner (callers check that first).
fn pid_from_owner(owner: u32) -> Pid {
    if owner == OWNER_INVALID {
        Pid::INVALID.into()
    } else {
        (owner as usize).into()
    }
}

struct LockInner {
    borrowed: AtomicU64,
    ptr: *mut dyn Any,
    descriptor: Descriptor,
}

unsafe impl Send for LockInner {}
unsafe impl Sync for LockInner {}

impl LockInner {
    fn new(descriptor: Descriptor, ptr: *mut dyn Any) -> Self {
        Self {
            borrowed: AtomicU64::new(pack(0, OWNER_FREE)),
            ptr,
            descriptor,
        }
    }

    /// Try to acquire the lock for `pid`, returning the exact token acquired
    /// on success so the caller (a [`DeviceGuard`]) can release precisely
    /// that acquisition later.
    pub fn try_lock(self: &Arc<Self>, pid: Pid) -> Result<u64, GetDeviceError> {
        let mut pid = pid;
        if pid.is_not_set() {
            pid = Pid::INVALID.into();
        }
        let owner = owner_from_pid(pid);

        loop {
            let current = self.borrowed.load(Ordering::Relaxed);
            let (generation, cur_owner) = unpack(current);
            if cur_owner != OWNER_FREE {
                return Err(if cur_owner == OWNER_INVALID {
                    GetDeviceError::UsedByUnknown
                } else {
                    GetDeviceError::UsedByOthers(pid_from_owner(cur_owner))
                });
            }

            let new = pack(generation.wrapping_add(1), owner);
            match self
                .borrowed
                .compare_exchange(current, new, Ordering::Acquire, Ordering::Relaxed)
            {
                Ok(_) => return Ok(new),
                // Someone else raced us for the free slot; reload and re-check.
                Err(_) => continue,
            }
        }
    }

    pub fn lock(self: &Arc<Self>) -> Result<u64, GetDeviceError> {
        let pid = get_pid();
        loop {
            match self.try_lock(pid) {
                Ok(token) => return Ok(token),
                Err(GetDeviceError::UsedByOthers(_)) | Err(GetDeviceError::UsedByUnknown) => {
                    relax();
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Free this lock if it is currently held by `pid`.
    ///
    /// Used by the death-reclaim path (`reclaim_all_held_by`, via
    /// [`DeviceOwner::reclaim_if_held_by`]) so a process that dies while
    /// holding a device lock doesn't wedge it forever. The CAS uses the
    /// exact token observed by this call, so a pid reused by a brand-new
    /// process can never be reclaimed out from under it: any intervening
    /// acquire/release bumps the generation and the CAS here simply fails,
    /// harmlessly.
    pub fn reclaim_if_held_by(&self, pid: u32) -> bool {
        let current = self.borrowed.load(Ordering::Relaxed);
        let (generation, owner) = unpack(current);
        if owner != pid {
            return false;
        }

        let freed = pack(generation.wrapping_add(1), OWNER_FREE);
        self.borrowed
            .compare_exchange(current, freed, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
    }
}

pub struct DeviceGuard<T> {
    lock: Arc<LockInner>,
    ptr: *mut T,
    /// The exact token observed at acquire time; released with a CAS against
    /// this precise value so a reclaim that already freed (and possibly
    /// re-acquired) this lock is never stomped.
    token: u64,
}

unsafe impl<T> Send for DeviceGuard<T> {}

impl<T> Drop for DeviceGuard<T> {
    fn drop(&mut self) {
        let (generation, _owner) = unpack(self.token);
        let released = pack(generation.wrapping_add(1), OWNER_FREE);
        if self
            .lock
            .borrowed
            .compare_exchange(self.token, released, Ordering::Release, Ordering::Relaxed)
            .is_err()
        {
            warn!(
                "device lock (id={:?}) token stale on drop, likely reclaimed from a dead holder; \
                 not releasing to avoid stomping a newer owner",
                self.lock.descriptor.device_id()
            );
        }
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
        let token = lock.lock()?;

        Ok(DeviceGuard {
            lock,
            ptr: self.ptr,
            token,
        })
    }
    pub fn try_lock(&self) -> Result<DeviceGuard<T>, GetDeviceError> {
        let lock = self.lock.upgrade().ok_or(GetDeviceError::DeviceReleased)?;
        let token = lock.try_lock(get_pid())?;

        Ok(DeviceGuard {
            lock,
            ptr: self.ptr,
            token,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::Empty;

    fn new_owner() -> DeviceOwner {
        DeviceOwner::new(Descriptor::new(), Empty)
    }

    #[test]
    fn reacquire_after_normal_drop_bumps_generation() {
        let owner = new_owner();
        let lock = owner.lock.clone();
        let device: Device<Empty> = owner.weak::<Empty>().unwrap();

        let pid_a: Pid = 100usize.into();
        let token_a = lock.try_lock(pid_a).expect("first acquire succeeds");
        let (gen_a, owner_a) = unpack(token_a);
        assert_eq!(owner_a, 100);

        let guard = DeviceGuard {
            lock: lock.clone(),
            ptr: device.ptr,
            token: token_a,
        };
        drop(guard);

        let pid_b: Pid = 200usize.into();
        let token_b = lock.try_lock(pid_b).expect("reacquire after drop succeeds");
        let (gen_b, owner_b) = unpack(token_b);
        assert_eq!(owner_b, 200);
        assert!(
            gen_b > gen_a,
            "generation must strictly increase across acquire cycles"
        );
    }

    #[test]
    fn reclaim_frees_only_matching_pid() {
        let owner = new_owner();
        let lock = owner.lock.clone();

        let pid: Pid = 42usize.into();
        let token = lock.try_lock(pid).unwrap();

        assert!(
            !lock.reclaim_if_held_by(43),
            "reclaim for a non-matching pid must be a no-op"
        );
        let (_, owner_after) = unpack(lock.borrowed.load(Ordering::Relaxed));
        assert_eq!(
            owner_after, 42,
            "non-matching reclaim must not touch the lock"
        );

        assert!(
            lock.reclaim_if_held_by(42),
            "reclaim for the matching pid must free the lock"
        );
        let (_, owner_after2) = unpack(lock.borrowed.load(Ordering::Relaxed));
        assert_eq!(owner_after2, OWNER_FREE);

        let _ = token;
    }

    #[test]
    fn stale_token_cas_is_noop_after_recycled_pid_reacquire() {
        let owner = new_owner();
        let lock = owner.lock.clone();

        let dead_pid: Pid = 7usize.into();
        let stale_token = lock.try_lock(dead_pid).expect("dead process acquires");

        // The reaper frees the lock at do_exit.
        assert!(lock.reclaim_if_held_by(7));

        // pid 7 gets recycled by the OS and a brand-new, unrelated process
        // legitimately re-acquires the very same device.
        let recycled_pid: Pid = 7usize.into();
        let new_token = lock
            .try_lock(recycled_pid)
            .expect("recycled pid re-acquires");
        assert_ne!(
            stale_token, new_token,
            "generation must differ across acquisitions even with an identical numeric pid"
        );

        // A late/duplicate reclaim-style CAS using the stale (pre-recycle)
        // token must not be able to free the new legitimate holder's lock.
        let (stale_gen, _) = unpack(stale_token);
        let stale_release = pack(stale_gen.wrapping_add(1), OWNER_FREE);
        assert!(
            lock.borrowed
                .compare_exchange(
                    stale_token,
                    stale_release,
                    Ordering::AcqRel,
                    Ordering::Relaxed
                )
                .is_err(),
            "stale token must not CAS-succeed against the recycled holder's live token"
        );

        let (_, owner_now) = unpack(lock.borrowed.load(Ordering::Relaxed));
        assert_eq!(owner_now, 7);
        assert_eq!(lock.borrowed.load(Ordering::Relaxed), new_token);
    }

    #[test]
    fn guard_drop_after_reclaim_does_not_stomp() {
        let owner = new_owner();
        let lock = owner.lock.clone();
        let device: Device<Empty> = owner.weak::<Empty>().unwrap();

        let dead_pid: Pid = 9usize.into();
        let token = lock.try_lock(dead_pid).expect("dead process acquires");
        let guard = DeviceGuard {
            lock: lock.clone(),
            ptr: device.ptr,
            token,
        };

        // The process holding `guard` dies; the reaper reclaims its lock
        // before the guard itself is ever dropped.
        assert!(lock.reclaim_if_held_by(9));

        // pid 9 is recycled and a brand-new process legitimately acquires
        // the same device.
        let recycled_pid: Pid = 9usize.into();
        let new_token = lock
            .try_lock(recycled_pid)
            .expect("recycled pid re-acquires");

        // The dead process's guard is finally dropped (e.g. late task
        // teardown): it must not stomp the new legitimate holder.
        drop(guard);

        let (_, owner_now) = unpack(lock.borrowed.load(Ordering::Relaxed));
        assert_eq!(
            owner_now, 9,
            "guard drop must not free the new holder's lock"
        );
        assert_eq!(lock.borrowed.load(Ordering::Relaxed), new_token);
    }
}
