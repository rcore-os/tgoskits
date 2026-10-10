//! Original Rust implementations of ACPICA's operating-system services.
use alloc::alloc::{alloc, dealloc};
use core::{
    alloc::Layout,
    ffi::c_void,
    ptr,
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
};

use crate::{
    BAD_PARAMETER, LIMIT, NO_MEMORY, OK, Status, TIME,
    backend::{IrqHandler, PciId, Work, backend},
};

const HEADER: usize = 16;
fn allocation_layout(size: usize) -> Option<Layout> {
    Layout::from_size_align(size.checked_add(HEADER)?, HEADER).ok()
}
#[unsafe(no_mangle)]
extern "C" fn AcpiOsAllocate(size: usize) -> *mut c_void {
    let Some(layout) = allocation_layout(size) else {
        return ptr::null_mut();
    };
    // SAFETY: layout was validated, header is aligned and within allocation.
    unsafe {
        let base = alloc(layout);
        if base.is_null() {
            return ptr::null_mut();
        }
        base.cast::<usize>().write(size);
        base.add(HEADER).cast()
    }
}
#[unsafe(no_mangle)]
unsafe extern "C" fn AcpiOsFree(memory: *mut c_void) {
    if !memory.is_null() {
        // SAFETY: ACPICA returns only pointers allocated by AcpiOsAllocate.
        unsafe {
            let base = memory.cast::<u8>().sub(HEADER);
            let size = base.cast::<usize>().read();
            dealloc(
                base,
                allocation_layout(size).expect("OSL allocation header"),
            );
        }
    }
}
#[unsafe(no_mangle)]
extern "C" fn AcpiOsInitialize() -> Status {
    OK
}
#[unsafe(no_mangle)]
extern "C" fn AcpiOsTerminate() -> Status {
    backend().wait_events();
    OK
}
#[unsafe(no_mangle)]
extern "C" fn AcpiOsGetRootPointer() -> u64 {
    backend().root_pointer()
}
#[unsafe(no_mangle)]
unsafe extern "C" fn AcpiOsPredefinedOverride(_v: *const c_void, out: *mut *mut u8) -> Status {
    if out.is_null() {
        return BAD_PARAMETER;
    }
    // SAFETY: caller supplies an output pointer, no override is installed.
    unsafe { out.write(ptr::null_mut()) };
    OK
}
#[unsafe(no_mangle)]
unsafe extern "C" fn AcpiOsTableOverride(_v: *const c_void, out: *mut *mut c_void) -> Status {
    if out.is_null() {
        return BAD_PARAMETER;
    }
    // SAFETY: ACPICA supplies an output pointer.
    unsafe { out.write(ptr::null_mut()) };
    OK
}
#[unsafe(no_mangle)]
unsafe extern "C" fn AcpiOsPhysicalTableOverride(
    _v: *const c_void,
    address: *mut u64,
    length: *mut u32,
) -> Status {
    if address.is_null() || length.is_null() {
        return BAD_PARAMETER;
    }
    // SAFETY: ACPICA supplies both output pointers.
    unsafe {
        address.write(0);
        length.write(0);
    };
    OK
}
#[unsafe(no_mangle)]
extern "C" fn AcpiOsMapMemory(address: u64, size: usize) -> *mut c_void {
    if size == 0 || address.checked_add(size as u64).is_none() {
        return ptr::null_mut();
    }
    backend().map(address, size)
}
#[unsafe(no_mangle)]
extern "C" fn AcpiOsUnmapMemory(address: *mut c_void, size: usize) {
    // SAFETY: ACPICA pairs each successful map with the same address and size
    // after its accesses to the mapping have finished.
    unsafe { backend().unmap(address, size) };
}
#[unsafe(no_mangle)]
unsafe extern "C" fn AcpiOsGetPhysicalAddress(address: *mut c_void, out: *mut u64) -> Status {
    if out.is_null() {
        return BAD_PARAMETER;
    }
    match backend().physical_address(address) {
        // SAFETY: output storage belongs to ACPICA.
        Ok(value) => {
            unsafe { out.write(value) };
            OK
        }
        Err(e) => e,
    }
}
struct Semaphore {
    units: AtomicU32,
    maximum: u32,
}
#[unsafe(no_mangle)]
unsafe extern "C" fn AcpiOsCreateSemaphore(
    max: u32,
    initial: u32,
    out: *mut *mut c_void,
) -> Status {
    if out.is_null() || max == 0 || initial > max {
        return BAD_PARAMETER;
    }
    let p = AcpiOsAllocate(size_of::<Semaphore>()).cast::<Semaphore>();
    if p.is_null() {
        return NO_MEMORY;
    }
    // SAFETY: allocation fits Semaphore, output storage belongs to ACPICA.
    unsafe {
        p.write(Semaphore {
            units: AtomicU32::new(initial),
            maximum: max,
        });
        out.write(p.cast());
    };
    OK
}
#[unsafe(no_mangle)]
unsafe extern "C" fn AcpiOsDeleteSemaphore(handle: *mut c_void) -> Status {
    if handle.is_null() {
        return BAD_PARAMETER;
    }
    // SAFETY: ACPICA deletes the semaphore only after all users have stopped.
    unsafe { AcpiOsFree(handle) };
    OK
}
#[unsafe(no_mangle)]
unsafe extern "C" fn AcpiOsWaitSemaphore(handle: *mut c_void, units: u32, timeout: u16) -> Status {
    if handle.is_null() || units == 0 {
        return BAD_PARAMETER;
    }
    // SAFETY: live handle returned by AcpiOsCreateSemaphore.
    let s = unsafe { &*handle.cast::<Semaphore>() };
    if units > s.maximum {
        return LIMIT;
    }
    let started = backend().timer_100ns();
    loop {
        if s.units
            .try_update(Ordering::AcqRel, Ordering::Acquire, |v| {
                v.checked_sub(units)
            })
            .is_ok()
        {
            return OK;
        }
        if timeout == 0
            || (timeout != u16::MAX
                && backend().timer_100ns().wrapping_sub(started) >= u64::from(timeout) * 10_000)
        {
            return TIME;
        }
        // Wait with IRQs enabled in task context, never hold an OSL spinlock.
        if !backend().sleep(1) {
            return crate::ERROR;
        }
    }
}
#[unsafe(no_mangle)]
unsafe extern "C" fn AcpiOsSignalSemaphore(handle: *mut c_void, units: u32) -> Status {
    if handle.is_null() || units == 0 {
        return BAD_PARAMETER;
    }
    // SAFETY: live handle returned by AcpiOsCreateSemaphore.
    let s = unsafe { &*handle.cast::<Semaphore>() };
    match s
        .units
        .try_update(Ordering::Release, Ordering::Relaxed, |v| {
            v.checked_add(units).filter(|n| *n <= s.maximum)
        }) {
        Ok(_) => OK,
        Err(_) => LIMIT,
    }
}
#[unsafe(no_mangle)]
unsafe extern "C" fn AcpiOsCreateLock(out: *mut *mut c_void) -> Status {
    if out.is_null() {
        return BAD_PARAMETER;
    }
    let p = AcpiOsAllocate(size_of::<AtomicBool>()).cast::<AtomicBool>();
    if p.is_null() {
        return NO_MEMORY;
    }
    // SAFETY: allocation fits AtomicBool, out belongs to ACPICA.
    unsafe {
        p.write(AtomicBool::new(false));
        out.write(p.cast());
    };
    OK
}
#[unsafe(no_mangle)]
unsafe extern "C" fn AcpiOsDeleteLock(handle: *mut c_void) {
    // SAFETY: ACPICA destroys locks only after all holders have released them.
    unsafe { AcpiOsFree(handle) };
}
#[unsafe(no_mangle)]
unsafe extern "C" fn AcpiOsAcquireLock(handle: *mut c_void) -> usize {
    let flags = backend().irq_save();
    // SAFETY: valid OSL lock, IRQs disabled before testing ownership.
    let lock = unsafe { &*handle.cast::<AtomicBool>() };
    while lock
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        core::hint::spin_loop();
    }
    flags
}
#[unsafe(no_mangle)]
unsafe extern "C" fn AcpiOsReleaseLock(handle: *mut c_void, flags: usize) {
    // SAFETY: caller owns this OSL lock, no references to protected data escape.
    unsafe { (*handle.cast::<AtomicBool>()).store(false, Ordering::Release) };
    backend().irq_restore(flags);
}
#[unsafe(no_mangle)]
extern "C" fn AcpiOsInstallInterruptHandler(
    irq: u32,
    handler: IrqHandler,
    context: *mut c_void,
) -> Status {
    // SAFETY: ACPICA retains handler/context until the matching remove call;
    // Backend::remove_irq synchronizes and drains in-flight callbacks.
    unsafe { backend().install_irq(irq, handler, context) }
}
#[unsafe(no_mangle)]
extern "C" fn AcpiOsRemoveInterruptHandler(irq: u32, handler: IrqHandler) -> Status {
    // SAFETY: Backend guarantees no callback can start or remain in flight
    // after removal, and treats removal of a skipped/absent handler as a no-op.
    unsafe { backend().remove_irq(irq, handler) }
}
#[unsafe(no_mangle)]
extern "C" fn AcpiOsExecute(kind: u32, function: Work, context: *mut c_void) -> Status {
    // SAFETY: ACPICA owns function/context until deferred work finishes;
    // wait_events and quiesce drain every callback accepted by the backend.
    unsafe { backend().execute(kind, function, context) }
}
#[unsafe(no_mangle)]
extern "C" fn AcpiOsWaitEventsComplete() {
    backend().wait_events();
}
#[unsafe(no_mangle)]
extern "C" fn AcpiOsGetThreadId() -> u64 {
    backend().thread_id()
}
#[unsafe(no_mangle)]
extern "C" fn AcpiOsGetTimer() -> u64 {
    backend().timer_100ns()
}
#[unsafe(no_mangle)]
extern "C" fn AcpiOsSleep(millis: u64) {
    if !backend().sleep(millis) {
        backend().log(b"ACPICA OSL sleep failed");
    }
}
#[unsafe(no_mangle)]
extern "C" fn AcpiOsStall(micros: u32) {
    backend().stall(micros);
}
fn valid_port(address: u64, width: u32) -> bool {
    matches!(width, 8 | 16 | 32)
        && address
            .checked_add(u64::from(width / 8))
            .is_some_and(|end| end <= 0x10000)
}
#[unsafe(no_mangle)]
unsafe extern "C" fn AcpiOsReadPort(address: u64, out: *mut u32, width: u32) -> Status {
    if out.is_null() || !valid_port(address, width) {
        return BAD_PARAMETER;
    }
    match backend().read_port(address as u16, width) {
        // SAFETY: output storage belongs to ACPICA.
        Ok(v) => {
            unsafe { out.write(v) };
            OK
        }
        Err(e) => e,
    }
}
#[unsafe(no_mangle)]
extern "C" fn AcpiOsWritePort(address: u64, value: u32, width: u32) -> Status {
    if !valid_port(address, width) {
        return BAD_PARAMETER;
    }
    backend().write_port(address as u16, width, value)
}
fn valid_memory(address: u64, width: u32) -> bool {
    matches!(width, 8 | 16 | 32 | 64)
        && address.is_multiple_of(u64::from(width / 8))
        && address.checked_add(u64::from(width / 8)).is_some()
}
#[unsafe(no_mangle)]
unsafe extern "C" fn AcpiOsReadMemory(address: u64, out: *mut u64, width: u32) -> Status {
    if out.is_null() || !valid_memory(address, width) {
        return BAD_PARAMETER;
    }
    let p = AcpiOsMapMemory(address, (width / 8) as usize);
    if p.is_null() {
        return NO_MEMORY;
    }
    // SAFETY: backend admitted a live aligned mapping of the requested width.
    unsafe {
        out.write(match width {
            8 => p.cast::<u8>().read_volatile() as u64,
            16 => p.cast::<u16>().read_volatile() as u64,
            32 => p.cast::<u32>().read_volatile() as u64,
            _ => p.cast::<u64>().read_volatile(),
        });
    }
    AcpiOsUnmapMemory(p, (width / 8) as usize);
    OK
}
#[unsafe(no_mangle)]
extern "C" fn AcpiOsWriteMemory(address: u64, value: u64, width: u32) -> Status {
    if !valid_memory(address, width) {
        return BAD_PARAMETER;
    }
    let p = AcpiOsMapMemory(address, (width / 8) as usize);
    if p.is_null() {
        return NO_MEMORY;
    }
    // SAFETY: backend admitted a live aligned writable mapping.
    unsafe {
        match width {
            8 => p.cast::<u8>().write_volatile(value as u8),
            16 => p.cast::<u16>().write_volatile(value as u16),
            32 => p.cast::<u32>().write_volatile(value as u32),
            _ => p.cast::<u64>().write_volatile(value),
        };
    }
    AcpiOsUnmapMemory(p, (width / 8) as usize);
    OK
}
fn valid_pci(id: PciId, reg: u32, width: u32) -> bool {
    id.bus <= 255
        && id.device <= 31
        && id.function <= 7
        && matches!(width, 8 | 16 | 32 | 64)
        && reg.is_multiple_of(width / 8)
        && reg.checked_add(width / 8).is_some_and(|e| e <= 4096)
}
#[unsafe(no_mangle)]
unsafe extern "C" fn AcpiOsReadPciConfiguration(
    id: *const PciId,
    reg: u32,
    out: *mut u64,
    width: u32,
) -> Status {
    if id.is_null() || out.is_null() {
        return BAD_PARAMETER;
    }
    // SAFETY: ACPICA supplies an initialized PCI ID.
    let id = unsafe { id.read() };
    if !valid_pci(id, reg, width) {
        return BAD_PARAMETER;
    }
    match backend().read_pci(id, reg, width) {
        // SAFETY: output storage belongs to ACPICA.
        Ok(v) => {
            unsafe { out.write(v) };
            OK
        }
        Err(e) => e,
    }
}
#[unsafe(no_mangle)]
unsafe extern "C" fn AcpiOsWritePciConfiguration(
    id: *const PciId,
    reg: u32,
    value: u64,
    width: u32,
) -> Status {
    if id.is_null() {
        return BAD_PARAMETER;
    }
    // SAFETY: ACPICA supplies an initialized PCI ID.
    let id = unsafe { id.read() };
    if !valid_pci(id, reg, width) {
        return BAD_PARAMETER;
    }
    backend().write_pci(id, reg, width, value)
}
#[unsafe(no_mangle)]
extern "C" fn AcpiOsReadable(p: *mut c_void, size: usize) -> u8 {
    backend().readable(p, size).into()
}
#[unsafe(no_mangle)]
extern "C" fn AcpiOsWritable(p: *mut c_void, size: usize) -> u8 {
    backend().writable(p, size).into()
}
#[unsafe(no_mangle)]
extern "C" fn AcpiOsEnterSleep(_state: u8, _a: u32, _b: u32) -> Status {
    OK
}
#[unsafe(no_mangle)]
extern "C" fn AcpiOsSignal(kind: u32, _info: *mut c_void) -> Status {
    backend().log(b"ACPICA firmware signalled fatal/breakpoint");
    if kind > 1 { BAD_PARAMETER } else { OK }
}
#[unsafe(no_mangle)]
extern "C" fn AcpiOsRedirectOutput(_destination: *mut c_void) {}

static LOG_WINDOW: AtomicU64 = AtomicU64::new(u64::MAX);
static LOG_COUNT: AtomicU32 = AtomicU32::new(0);
static AML_ERRORS: AtomicU64 = AtomicU64::new(0);
pub fn aml_error_count() -> u64 {
    AML_ERRORS.load(Ordering::Relaxed)
}
#[unsafe(no_mangle)]
unsafe extern "C" fn acpica_interpreter_log(p: *const u8, length: usize) {
    // SAFETY: variadic bridge supplies its stack buffer for this call only.
    let message = unsafe { core::slice::from_raw_parts(p, length.min(511)) };
    if message.windows(10).any(|w| w == b"ACPI Error") {
        AML_ERRORS.fetch_add(1, Ordering::Relaxed);
    }
    let window = backend().timer_100ns() / 10_000_000;
    if LOG_WINDOW.swap(window, Ordering::Relaxed) != window {
        LOG_COUNT.store(0, Ordering::Relaxed);
    }
    if LOG_COUNT.fetch_add(1, Ordering::Relaxed) < 32 {
        backend().log(message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn allocation_alignment_and_overflow() {
        assert!(allocation_layout(usize::MAX).is_none());
        for size in [0, 1, 16, 4096] {
            let p = AcpiOsAllocate(size);
            assert!(!p.is_null());
            assert_eq!(p as usize % 16, 0);
            // SAFETY: test frees exactly its own OSL allocation.
            unsafe { AcpiOsFree(p) };
        }
    }
    #[test]
    fn access_validation() {
        assert!(valid_port(0xfffe, 16));
        assert!(!valid_port(0xffff, 16));
        assert!(!valid_port(0, 64));
        assert!(!valid_memory(3, 32));
        assert!(!valid_memory(u64::MAX, 8));
        let id = PciId {
            segment: 0,
            bus: 0,
            device: 31,
            function: 7,
        };
        assert!(valid_pci(id, 4092, 32));
        assert!(!valid_pci(id, 4095, 32));
        assert!(!valid_pci(id, 0, 0));
    }
    #[test]
    fn semaphore_unit_accounting() {
        let mut p = ptr::null_mut();
        // SAFETY: private live handles, no concurrent users, valid output.
        unsafe {
            assert_eq!(AcpiOsCreateSemaphore(2, 3, &mut p), BAD_PARAMETER);
            assert_eq!(AcpiOsCreateSemaphore(2, 1, &mut p), OK);
            assert_eq!(AcpiOsSignalSemaphore(p, 1), OK);
            assert_eq!(AcpiOsSignalSemaphore(p, 1), LIMIT);
            assert_eq!(AcpiOsDeleteSemaphore(p), OK);
        }
    }
}
