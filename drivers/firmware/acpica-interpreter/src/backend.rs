//! OS-specific facilities supplied by the caller; no scheduler/HAL dependency.
use core::{
    ffi::c_void,
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
};

use crate::{SUPPORT, Status};

pub type IrqHandler = unsafe extern "C" fn(*mut c_void) -> u32;
pub type Work = unsafe extern "C" fn(*mut c_void);
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct PciId {
    pub segment: u16,
    pub bus: u16,
    pub device: u16,
    pub function: u16,
}

/// # Safety
/// `root_pointer` must identify stable ACPI tables. A successful mapping must
/// cover the requested range, remain live until `unmap`, and be aligned for any
/// access ACPICA performs through it. MMIO, port, and PCI operations may access
/// only ranges admitted by the integration. IRQ removal must prevent new calls
/// and synchronize in-flight callbacks. `execute` must defer work rather than
/// invoke it inline and `wait_events`/`quiesce` must drain accepted work before
/// interpreter teardown. Timer values must be monotonic in 100 ns units;
/// `thread_id` must be nonzero and stable while a task holds AML locks. IRQ
/// save/restore must be nest-safe for the target kernel. Any non-null context
/// registered for an EC operation region must remain valid until engine
/// teardown and while an access callback is in flight.
pub unsafe trait Backend: Sync {
    fn root_pointer(&self) -> u64;
    fn map(&self, address: u64, size: usize) -> *mut c_void;
    /// Release a mapping previously returned by `map`.
    ///
    /// # Safety
    /// `address` and `size` must exactly match a successful, still-live mapping
    /// issued by this backend, and no access may be in flight.
    unsafe fn unmap(&self, address: *mut c_void, size: usize);
    fn physical_address(&self, _address: *mut c_void) -> Result<u64, Status> {
        Err(SUPPORT)
    }
    fn readable(&self, _address: *mut c_void, _size: usize) -> bool {
        false
    }
    fn writable(&self, _address: *mut c_void, _size: usize) -> bool {
        false
    }
    fn timer_100ns(&self) -> u64;
    fn sleep(&self, millis: u64) -> bool;
    fn stall(&self, micros: u32);
    fn thread_id(&self) -> u64;
    fn irq_save(&self) -> usize;
    fn irq_restore(&self, flags: usize);
    /// # Safety
    /// ACPICA must keep `handler`'s code and `context` live until matching
    /// `remove_irq` completes. An implementation may not retain the context
    /// after successful removal and must synchronize callbacks during removal.
    unsafe fn install_irq(&self, _irq: u32, _handler: IrqHandler, _context: *mut c_void) -> Status {
        SUPPORT
    }
    /// # Safety
    /// If a matching handler was installed, remove it so no callback may be
    /// running or start with its context after return. ACPICA also calls this
    /// during termination when handler initialization was explicitly skipped;
    /// in that case, an implementation must treat the absent handler as a
    /// successful no-op. An implementation that supports installation must
    /// override this method and synchronize removal.
    unsafe fn remove_irq(&self, _irq: u32, _handler: IrqHandler) -> Status {
        SUPPORT
    }
    /// # Safety
    /// `function` and `context` must remain valid until deferred work finishes.
    /// If an error is returned, the implementation must not retain either one.
    unsafe fn execute(&self, _kind: u32, _function: Work, _context: *mut c_void) -> Status {
        SUPPORT
    }
    fn wait_events(&self);
    /// Mask/synchronize SCI and drain deferred work before namespace teardown.
    fn quiesce(&self);
    fn read_port(&self, _address: u16, _width: u32) -> Result<u32, Status> {
        Err(SUPPORT)
    }
    fn write_port(&self, _address: u16, _width: u32, _value: u32) -> Status {
        SUPPORT
    }
    fn read_pci(&self, _id: PciId, _reg: u32, _width: u32) -> Result<u64, Status> {
        Err(SUPPORT)
    }
    fn write_pci(&self, _id: PciId, _reg: u32, _width: u32, _value: u64) -> Status {
        SUPPORT
    }
    /// Service an ACPICA EC operation-region access; `function` is 0 for read
    /// and 1 for write. `context` is the opaque value passed to registration and
    /// is valid only for the duration of this call.
    /// # Safety
    /// A non-null context must be the still-live value registered through
    /// `Engine::install_ec_handler`. The implementation may only dereference
    /// it during this call and must synchronize concurrent operation-region
    /// accesses.
    unsafe fn ec_access(
        &self,
        _function: u32,
        _address: u64,
        _width: u32,
        _value: &mut u64,
        _context: *mut c_void,
    ) -> Status {
        SUPPORT
    }
    fn log(&self, message: &[u8]);
}

/// Permanent registration for the single process-global ACPICA instance.
/// The backend must outlive the registration and every `Engine`.
pub struct BackendRegistration(pub &'static dyn Backend);
static BACKEND: AtomicPtr<BackendRegistration> = AtomicPtr::new(ptr::null_mut());
/// # Safety
/// Install only with no live ACPICA instance; registration and mapped firmware
/// must outlive it. Replacement is rejected rather than silently swapping OSL.
pub unsafe fn install_backend(registration: &'static BackendRegistration) -> Result<(), Status> {
    BACKEND
        .compare_exchange(
            ptr::null_mut(),
            ptr::from_ref(registration).cast_mut(),
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .map(|_| ())
        .or_else(|old| {
            if core::ptr::eq(old, registration) {
                Ok(())
            } else {
                Err(crate::ALREADY_EXISTS)
            }
        })
}
pub(crate) fn backend() -> &'static dyn Backend {
    let p = BACKEND.load(Ordering::Acquire);
    assert!(!p.is_null(), "ACPICA used before backend installation");
    // SAFETY: install_backend publishes a static registration once.
    unsafe { (*p).0 }
}
