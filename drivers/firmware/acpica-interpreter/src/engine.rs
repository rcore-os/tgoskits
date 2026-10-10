//! Owned public API; the global ACPICA instance has one lifecycle owner.
use alloc::{ffi::CString, string::String, vec::Vec};
use core::{
    ffi::{CStr, c_char, c_void},
    marker::PhantomData,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use crate::{
    BAD_PARAMETER, LIMIT, NO_MEMORY, OK, Status,
    backend::{BackendRegistration, install_backend},
};
static LIVE: AtomicBool = AtomicBool::new(false);
static POWER: AtomicUsize = AtomicUsize::new(0);
static NOTIFY: AtomicUsize = AtomicUsize::new(0);

struct InitializationGuard {
    release_claim: bool,
    terminate_subsystem: bool,
}

impl Drop for InitializationGuard {
    fn drop(&mut self) {
        if self.terminate_subsystem {
            crate::backend::backend().quiesce();
            // SAFETY: AcpiInitializeSubsystem completed and no Engine was
            // published, so no public operation can overlap this teardown.
            unsafe {
                let _ = AcpiTerminate();
            }
        }
        NOTIFY.store(0, Ordering::Release);
        POWER.store(0, Ordering::Release);
        if self.release_claim {
            LIVE.store(false, Ordering::Release);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Null,
    Integer(u64),
    String(Vec<u8>),
    Buffer(Vec<u8>),
    Package(Vec<Value>),
    Reference(Vec<u8>),
}
#[derive(Clone, Debug)]
pub struct Node {
    pub path: String,
    pub kind: u32,
}
#[derive(Clone, Copy, Debug)]
pub enum Mode {
    Hardware,
    Offline,
}
pub struct Engine {
    _not_send_sync: PhantomData<*mut ()>,
    notify: bool,
    objects_initialization_attempted: bool,
    initialized: bool,
}
unsafe extern "C" {
    fn AcpiInitializeSubsystem() -> Status;
    fn AcpiInitializeTables(storage: *mut c_void, count: u32, resize: u8) -> Status;
    fn AcpiLoadTables() -> Status;
    fn AcpiEnableSubsystem(flags: u32) -> Status;
    fn AcpiInitializeObjects(flags: u32) -> Status;
    fn AcpiTerminate() -> Status;
    fn AcpiUpdateAllGpes() -> Status;
    fn AcpiDisableAllGpes() -> Status;
    fn AcpiEnterSleepStatePrep(state: u8) -> Status;
    fn AcpiEnterSleepState(state: u8) -> Status;
    fn acpica_interpreter_evaluate(
        path: *const c_char,
        args: *const u64,
        argc: u32,
        out: *mut u8,
        capacity: usize,
        used: *mut usize,
    ) -> Status;
    fn acpica_interpreter_walk(
        callback: unsafe extern "C" fn(*const c_char, u32, *mut c_void) -> Status,
        context: *mut c_void,
    ) -> Status;
    fn acpica_interpreter_install_notify() -> Status;
    fn acpica_interpreter_remove_notify();
    fn acpica_interpreter_resources(
        path: *const c_char,
        possible: u8,
        out: *mut u8,
        capacity: usize,
        used: *mut usize,
    ) -> Status;
    fn acpica_interpreter_platform_osc() -> Status;
    fn acpica_interpreter_has_fixed_power() -> u8;
    fn acpica_interpreter_install_fixed_power() -> Status;
    fn acpica_interpreter_install_ec(path: *const c_char, context: *mut c_void) -> Status;
    fn acpica_interpreter_table_count() -> u32;
    fn acpica_interpreter_resolve(
        parent: *const c_char,
        source: *const c_char,
        out: *mut u8,
        capacity: usize,
        used: *mut usize,
    ) -> Status;
    fn acpica_interpreter_table(
        index: u32,
        out: *mut u8,
        capacity: usize,
        used: *mut usize,
    ) -> Status;
}
pub fn status(status: Status) -> Result<(), Status> {
    if status == OK { Ok(()) } else { Err(status) }
}
// SAFETY: ACPICA public operations synchronize their internal shared state.
// Rust methods borrow the lifecycle owner; Drop cannot overlap a live borrow.
// The caller's installed Backend is Sync and promises deferred-work draining.
unsafe impl Send for Engine {}
// SAFETY: same ACPICA public-interface synchronization and backend contract.
unsafe impl Sync for Engine {}
impl Engine {
    /// # Safety
    /// Caller owns ACPI hardware, SCI and mappings and has initialized allocation,
    /// scheduling and deferred work. Firmware AML may write hardware. Offline
    /// mode requires a backend that simulates *all* hardware accesses.
    pub unsafe fn initialize(
        registration: &'static BackendRegistration,
        mode: Mode,
    ) -> Result<Self, Status> {
        // SAFETY: same initialization contract; no early handler is requested.
        unsafe { Self::initialize_with_tables(registration, mode, |_| Ok(())) }
    }
    /// Install table-described handlers before AML table loading. The hook may
    /// inspect tables and install operation-region handlers, but must not evaluate
    /// AML or enable events before namespace/hardware initialization completes.
    /// # Safety
    /// Same contract as initialize, plus the hook must obey this phase boundary.
    pub unsafe fn initialize_with_tables(
        registration: &'static BackendRegistration,
        mode: Mode,
        prepare: impl FnOnce(&Self) -> Result<(), Status>,
    ) -> Result<Self, Status> {
        LIVE.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| crate::ALREADY_EXISTS)?;
        let mut initialization = InitializationGuard {
            release_claim: true,
            terminate_subsystem: false,
        };
        // SAFETY: global lifecycle claim excludes another engine.
        unsafe { install_backend(registration) }?;
        let engine = Self {
            _not_send_sync: PhantomData,
            notify: false,
            objects_initialization_attempted: false,
            initialized: false,
        };
        // A failure inside subsystem initialization can leave ACPICA globals
        // partially initialized. Keep the claim poisoned rather than allowing
        // a second, unsafe initialization attempt.
        initialization.release_claim = false;
        // SAFETY: caller established the complete OSL contract; later failures
        // are rolled back after the subsystem has been initialized.
        unsafe {
            status(AcpiInitializeSubsystem())?;
            initialization.terminate_subsystem = true;
            initialization.release_claim = true;
            status(AcpiInitializeTables(core::ptr::null_mut(), 32, 1))?;
            prepare(&engine)?;
            status(AcpiLoadTables())?;
            let flags = match mode {
                Mode::Hardware => 0,
                Mode::Offline => 0x1 | 0x2 | 0x4 | 0x8 | 0x10,
            };
            status(AcpiEnableSubsystem(flags))?;
        }
        initialization.terminate_subsystem = false;
        initialization.release_claim = false;
        let mut engine = engine;
        engine.initialized = true;
        Ok(engine)
    }
    /// Run _REG/_STA/_INI after custom handlers (notably EC) are installed.
    /// An owned table copy. OEM tables may contain private data (e.g. MSDM);
    /// callers must apply root-only access policy and must never log the bytes.
    pub fn table_count(&self) -> Result<u32, Status> {
        // SAFETY: live instance, C reads the descriptor count under its mutex.
        let count = unsafe { acpica_interpreter_table_count() };
        if count > 4096 {
            return Err(LIMIT);
        }
        Ok(count)
    }
    pub fn table(&self, index: u32) -> Result<Vec<u8>, Status> {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(1024 * 1024)
            .map_err(|_| NO_MEMORY)?;
        bytes.resize(1024 * 1024, 0);
        let mut used = 0;
        // SAFETY: caller holds the live engine; table copy stays within buffer.
        unsafe {
            status(acpica_interpreter_table(
                index,
                bytes.as_mut_ptr(),
                bytes.len(),
                &mut used,
            ))?;
        }
        bytes.truncate(used);
        Ok(bytes)
    }
    pub fn resolve(&self, parent: &str, source: &str) -> Result<String, Status> {
        let parent = CString::new(parent).map_err(|_| BAD_PARAMETER)?;
        let source = CString::new(source).map_err(|_| BAD_PARAMETER)?;
        let mut out = [0u8; 4096];
        let mut used = 0;
        // SAFETY: bounded output and live CStrings throughout the C call.
        unsafe {
            status(acpica_interpreter_resolve(
                parent.as_ptr(),
                source.as_ptr(),
                out.as_mut_ptr(),
                out.len(),
                &mut used,
            ))?;
        }
        let path = core::str::from_utf8(&out[..used]).map_err(|_| BAD_PARAMETER)?;
        let mut owned = String::new();
        owned.try_reserve_exact(path.len()).map_err(|_| NO_MEMORY)?;
        owned.push_str(path);
        Ok(owned)
    }
    /// Run `_REG`/`_STA`/`_INI` after custom handlers (notably EC) are installed.
    /// ACPICA initialization may execute firmware methods with side effects, so
    /// this operation is attempted at most once. If it fails, discard the engine
    /// rather than retrying potentially partially completed firmware methods.
    pub fn initialize_objects(&mut self) -> Result<(), Status> {
        if self.objects_initialization_attempted {
            return Err(crate::ALREADY_EXISTS);
        }
        self.objects_initialization_attempted = true;
        // SAFETY: live instance, OSL is ready and hardware mode admits AML.
        unsafe { status(AcpiInitializeObjects(0)) }
    }
    pub fn evaluate(&self, path: &str, args: &[u64]) -> Result<Value, Status> {
        let path = CString::new(path).map_err(|_| BAD_PARAMETER)?;
        if args.len() > 4 {
            return Err(BAD_PARAMETER);
        }
        // Bounded output; do not retry AML methods on buffer overflow, because
        // repeating a method could repeat hardware side effects.
        let mut wire = Vec::new();
        wire.try_reserve_exact(1024 * 1024).map_err(|_| NO_MEMORY)?;
        wire.resize(1024 * 1024, 0);
        let mut used = 0;
        // SAFETY: all pointers refer to valid slices throughout the C call.
        unsafe {
            status(acpica_interpreter_evaluate(
                path.as_ptr(),
                args.as_ptr(),
                args.len() as u32,
                wire.as_mut_ptr(),
                wire.len(),
                &mut used,
            ))?;
        }
        wire.truncate(used);
        if wire.is_empty() {
            return Ok(Value::Null);
        }
        let mut cursor = 0;
        let value = decode(&wire, &mut cursor, 0)?;
        if cursor != wire.len() {
            return Err(BAD_PARAMETER);
        }
        Ok(value)
    }
    pub fn hardware_id(&self, path: &str) -> Result<String, Status> {
        match self.evaluate(&alloc::format!("{path}._HID"), &[])? {
            Value::String(s) => String::from_utf8(s).map_err(|_| BAD_PARAMETER),
            Value::Integer(v) => {
                let v = (v as u32).swap_bytes();
                let letters = [
                    (((v >> 26) & 31) as u8) + b'@',
                    (((v >> 21) & 31) as u8) + b'@',
                    (((v >> 16) & 31) as u8) + b'@',
                ];
                if !letters.iter().all(u8::is_ascii_uppercase) {
                    return Err(BAD_PARAMETER);
                }
                Ok(alloc::format!(
                    "{}{:04X}",
                    core::str::from_utf8(&letters).unwrap(),
                    v & 0xffff
                ))
            }
            _ => Err(crate::TYPE),
        }
    }
    pub fn integer(&self, path: &str) -> Result<u64, Status> {
        match self.evaluate(path, &[])? {
            Value::Integer(v) => Ok(v),
            _ => Err(crate::TYPE),
        }
    }
    pub fn namespace(&self) -> Result<Vec<Node>, Status> {
        let mut nodes = Vec::new();
        // SAFETY: synchronous callback borrows nodes only until walk returns.
        unsafe {
            status(acpica_interpreter_walk(
                walk_node,
                (&mut nodes as *mut Vec<Node>).cast(),
            ))?;
        }
        Ok(nodes)
    }
    /// Validated _CRS/_PRS returned as owned AML resource bytes (never native
    /// ACPICA resource pointers). Consumers must use a checked resource parser.
    pub fn resources(&self, path: &str, possible: bool) -> Result<Vec<u8>, Status> {
        let name = CString::new(path).map_err(|_| BAD_PARAMETER)?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(65536).map_err(|_| NO_MEMORY)?;
        bytes.resize(65536, 0);
        let mut used = 0;
        // SAFETY: live instance, initialized output slice; C validates and
        // converts the single method result without re-executing firmware AML.
        unsafe {
            status(acpica_interpreter_resources(
                name.as_ptr(),
                possible.into(),
                bytes.as_mut_ptr(),
                bytes.len(),
                &mut used,
            ))?;
        }
        bytes.truncate(used);
        Ok(bytes)
    }

    /// Install the one root Notify observer. Callback runs on deferred OSL work,
    /// must not panic, and must not retain its temporary pathname reference.
    pub fn install_notify(&mut self, callback: fn(&str, u32)) -> Result<(), Status> {
        if self.notify {
            return Err(crate::ALREADY_EXISTS);
        }
        NOTIFY.store(callback as usize, Ordering::Release);
        // SAFETY: root observer is static; owner removes and drains on drop.
        if let Err(e) = unsafe { status(acpica_interpreter_install_notify()) } {
            NOTIFY.store(0, Ordering::Release);
            return Err(e);
        }
        self.notify = true;
        Ok(())
    }
    /// FADT method-button/reduced-hardware flags exclude the fixed PM1 button.
    pub fn fixed_power_supported(&self) -> bool {
        // SAFETY: FADT globals belong to this live engine.
        unsafe { acpica_interpreter_has_fixed_power() != 0 }
    }
    /// Install the fixed power-button handler.
    ///
    /// `callback` runs in the ACPICA event context and must only latch a
    /// coalesced event; defer blocking work to the backend's task context.
    pub fn install_fixed_power(&self, callback: fn()) -> Result<(), Status> {
        let callback = callback as usize;
        POWER
            .compare_exchange(0, callback, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| crate::ALREADY_EXISTS)?;
        // SAFETY: the callback is static and only latches a coalesced event.
        let result = unsafe { status(acpica_interpreter_install_fixed_power()) };
        if result.is_err() {
            POWER.store(0, Ordering::Release);
        }
        result
    }
    /// Register the caller's EC operation-region service for one namespace
    /// object. The backend receives `context` in `Backend::ec_access`. ACPICA
    /// owns duplicate detection and reports failures without replacing an
    /// existing handler. On engine teardown the backend is quiesced before
    /// ACPICA removes the handler.
    ///
    /// # Safety
    /// If non-null, `context` must remain valid and correctly aligned for the
    /// backend's interpretation until this engine is dropped, or until failed
    /// initialization has returned after tearing ACPICA down. The backend must
    /// synchronize concurrent accesses and must not retain the pointer beyond
    /// that lifetime.
    pub unsafe fn install_ec_handler(
        &self,
        path: &str,
        context: *mut c_void,
    ) -> Result<(), Status> {
        let path = CString::new(path).map_err(|_| BAD_PARAMETER)?;
        // SAFETY: the caller upholds the context lifetime contract above.
        unsafe { status(acpica_interpreter_install_ec(path.as_ptr(), context)) }
    }
    pub fn update_gpes(&self) -> Result<(), Status> {
        // SAFETY: invoked after all namespace/custom handlers are initialized.
        unsafe { status(AcpiUpdateAllGpes()) }
    }
    pub fn disable_gpes(&self) -> Result<(), Status> {
        // SAFETY: ACPICA synchronizes event state against its SCI handler.
        unsafe { status(AcpiDisableAllGpes()) }
    }
    pub fn platform_osc(&self) -> Result<(), Status> {
        // SAFETY: original C bridge supplies correctly typed _OSC arguments.
        unsafe { status(acpica_interpreter_platform_osc()) }
    }
    pub fn prepare_s5(&self) -> Result<(), Status> {
        // SAFETY: live engine; call in task context before IRQs are disabled.
        unsafe { status(AcpiEnterSleepStatePrep(5)) }
    }
    /// # Safety
    /// Final power transition only, with IRQs disabled and filesystem flush done.
    pub unsafe fn enter_s5(&self) -> Result<(), Status> {
        // SAFETY: caller owns the final transition.
        unsafe { status(AcpiEnterSleepState(5)) }
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        if !self.initialized {
            return;
        }
        crate::backend::backend().quiesce();
        // SAFETY: exclusive lifecycle owner; terminate drains deferred callbacks
        // and removes the SCI handler before deleting interpreter locks/caches.
        unsafe {
            if self.notify {
                acpica_interpreter_remove_notify();
            }
            let _ = AcpiTerminate();
        }
        NOTIFY.store(0, Ordering::Release);
        POWER.store(0, Ordering::Release);
        LIVE.store(false, Ordering::Release);
    }
}
unsafe extern "C" fn walk_node(path: *const c_char, kind: u32, context: *mut c_void) -> Status {
    // SAFETY: C bridge passes a NUL terminated path and our live Vec context.
    let (path, nodes) = unsafe { (CStr::from_ptr(path), &mut *context.cast::<Vec<Node>>()) };
    if nodes.len() >= 65536 {
        return LIMIT;
    }
    let Ok(path) = path.to_str() else {
        return BAD_PARAMETER;
    };
    if nodes.try_reserve(1).is_err() {
        return NO_MEMORY;
    }
    let mut owned = String::new();
    if owned.try_reserve_exact(path.len()).is_err() {
        return NO_MEMORY;
    }
    owned.push_str(path);
    nodes.push(Node { path: owned, kind });
    OK
}
#[unsafe(no_mangle)]
unsafe extern "C" fn acpica_interpreter_notify(path: *const c_char, value: u32) {
    let callback = NOTIFY.load(Ordering::Acquire);
    if callback == 0 {
        return;
    }
    // SAFETY: install_notify published precisely this function pointer type;
    // C bridge owns the NUL-terminated path for the duration of the call.
    unsafe {
        if let Ok(path) = CStr::from_ptr(path).to_str() {
            let f = core::mem::transmute::<usize, fn(&str, u32)>(callback);
            f(path, value);
        }
    }
}
fn decode(wire: &[u8], cursor: &mut usize, depth: usize) -> Result<Value, Status> {
    if depth > 32 {
        return Err(LIMIT);
    }
    let header = wire
        .get(*cursor..cursor.checked_add(8).ok_or(LIMIT)?)
        .ok_or(BAD_PARAMETER)?;
    *cursor += 8;
    let tag = u32::from_le_bytes(header[..4].try_into().unwrap());
    let count = u32::from_le_bytes(header[4..].try_into().unwrap()) as usize;
    if tag == 4 {
        if count > 65536 {
            return Err(LIMIT);
        }
        let mut items = Vec::new();
        items.try_reserve_exact(count).map_err(|_| NO_MEMORY)?;
        for _ in 0..count {
            items.push(decode(wire, cursor, depth + 1)?);
        }
        return Ok(Value::Package(items));
    }
    let bytes = wire
        .get(*cursor..cursor.checked_add(count).ok_or(LIMIT)?)
        .ok_or(BAD_PARAMETER)?;
    *cursor += count;
    Ok(match tag {
        0 if count == 0 => Value::Null,
        1 if count == 8 => Value::Integer(u64::from_le_bytes(bytes.try_into().unwrap())),
        2 | 3 | 20 => {
            let mut b = Vec::new();
            b.try_reserve_exact(count).map_err(|_| NO_MEMORY)?;
            b.extend_from_slice(bytes);
            match tag {
                2 => Value::String(b),
                3 => Value::Buffer(b),
                _ => Value::Reference(b),
            }
        }
        _ => return Err(BAD_PARAMETER),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wire_rejects_truncation_and_bad_tags() {
        for n in 0..16 {
            let mut c = 0;
            let mut w = alloc::vec![0;16];
            w[0] = 1;
            w[4] = 8;
            assert!(decode(&w[..n], &mut c, 0).is_err());
        }
        let mut c = 0;
        assert!(decode(&[99, 0, 0, 0, 0, 0, 0, 0], &mut c, 0).is_err());
    }
}

#[unsafe(no_mangle)]
extern "C" fn acpica_interpreter_fixed_power() {
    let callback = POWER.load(Ordering::Acquire);
    if callback != 0 {
        // SAFETY: install_fixed_power publishes only static fn() callbacks.
        unsafe {
            core::mem::transmute::<usize, fn()>(callback)();
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn acpica_interpreter_ec_access(
    function: u32,
    address: u64,
    width: u32,
    value: *mut u64,
    context: *mut c_void,
) -> Status {
    if value.is_null() || function > 1 {
        return BAD_PARAMETER;
    }
    // SAFETY: ACPICA provides writable value storage for the callback duration;
    // the Backend contract covers the opaque registered context.
    let value = unsafe { &mut *value };
    // SAFETY: registration is only reachable through the unsafe Engine method,
    // which requires the opaque context to remain valid through teardown.
    unsafe { crate::backend::backend().ec_access(function, address, width, value, context) }
}

#[cfg(test)]
mod fixed_power_tests {
    unsafe extern "C" {
        fn acpica_interpreter_fixed_power_supported(flags: u32) -> u8;
    }
    #[test]
    fn method_button_and_reduced_hardware_do_not_enable_fixed_pm1() {
        // SAFETY: pure flags predicate; no hardware or global state access.
        unsafe {
            assert_eq!(acpica_interpreter_fixed_power_supported(0), 1);
            assert_eq!(acpica_interpreter_fixed_power_supported(1 << 4), 0);
            assert_eq!(acpica_interpreter_fixed_power_supported(1 << 20), 0);
            assert_eq!(
                acpica_interpreter_fixed_power_supported((1 << 4) | (1 << 20)),
                0
            );
        }
    }
}
