//! Guest COM1 output boundaries used by x86 firmware diagnostics.

use alloc::{
    collections::BTreeMap,
    sync::{Arc, Weak},
};

use ax_kspin::SpinNoIrq as Mutex;
use x86_vlapic::X86VmId;

const OVMF_SEC_MARKER: &str = "SecCoreStartupWithStack(";

static OVMF_SEC_DIAGNOSTICS: Mutex<OvmfSecDiagnostics> = Mutex::new(OvmfSecDiagnostics::new());
static FIXED_OVMF_INSTANCES: Mutex<BTreeMap<X86VmId, Weak<crate::AxVM>>> =
    Mutex::new(BTreeMap::new());

pub(super) fn observe(vm_id: X86VmId, bytes: &[u8]) {
    let reached_sec = OVMF_SEC_DIAGNOSTICS.lock().observe(vm_id, bytes);
    if reached_sec {
        info!(
            "VM[{vm_id}] guest COM1 reached OVMF SEC: {}",
            OVMF_SEC_MARKER
        );
    }
}

pub(super) fn activate_ovmf_sec_diagnostic(vm: &crate::AxVMRef) {
    let fixed_ovmf_profile_loaded = FIXED_OVMF_INSTANCES
        .lock()
        .get(&vm.id())
        .and_then(Weak::upgrade)
        .is_some_and(|loaded| Arc::ptr_eq(&loaded, vm));
    let is_registered_instance = fixed_ovmf_profile_loaded
        && crate::manager::with_vm(vm.id(), |registered| {
            alloc::sync::Arc::ptr_eq(registered, vm)
        })
        .unwrap_or(false);

    OVMF_SEC_DIAGNOSTICS.lock().activate(
        vm.id(),
        fixed_ovmf_profile_loaded,
        is_registered_instance,
    );
}

pub(super) fn mark_fixed_ovmf_profile_loaded(vm: &crate::AxVMRef) {
    // The loader calls this only after the fixed CODE image has been
    // validated and copied. A weak, exact-instance reference keeps the
    // qualification across reset without extending the VM lifetime.
    FIXED_OVMF_INSTANCES
        .lock()
        .insert(vm.id(), Arc::downgrade(vm));
}

pub(super) fn forget_vm(vm_id: X86VmId) {
    OVMF_SEC_DIAGNOSTICS.lock().forget(vm_id);
}

struct OvmfSecDiagnostics {
    matchers: BTreeMap<X86VmId, MarkerMatcher>,
}

impl OvmfSecDiagnostics {
    const fn new() -> Self {
        Self {
            matchers: BTreeMap::new(),
        }
    }

    fn activate(
        &mut self,
        vm_id: X86VmId,
        fixed_ovmf_profile_loaded: bool,
        is_registered_instance: bool,
    ) {
        if fixed_ovmf_profile_loaded && is_registered_instance {
            self.matchers.entry(vm_id).or_default();
        }
    }

    fn observe(&mut self, vm_id: X86VmId, bytes: &[u8]) -> bool {
        self.matchers
            .get_mut(&vm_id)
            .is_some_and(|matcher| matcher.observe(bytes))
    }

    fn forget(&mut self, vm_id: X86VmId) {
        self.matchers.remove(&vm_id);
    }
}

#[derive(Default)]
struct MarkerMatcher {
    matched: usize,
    reported: bool,
}

impl MarkerMatcher {
    fn observe(&mut self, bytes: &[u8]) -> bool {
        if self.reported {
            return false;
        }

        for &byte in bytes {
            if byte == OVMF_SEC_MARKER.as_bytes()[self.matched] {
                self.matched += 1;
                if self.matched == OVMF_SEC_MARKER.len() {
                    self.reported = true;
                    return true;
                }
            } else {
                self.matched = usize::from(byte == OVMF_SEC_MARKER.as_bytes()[0]);
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_marker_split_across_guest_writes_once() {
        let mut matcher = MarkerMatcher::default();

        assert!(!matcher.observe(b"noise SecCoreStartup"));
        assert!(matcher.observe(b"WithStack("));
        assert!(!matcher.observe(OVMF_SEC_MARKER.as_bytes()));
    }

    #[test]
    fn rejects_incomplete_or_different_serial_output() {
        let mut matcher = MarkerMatcher::default();

        assert!(!matcher.observe(b"SecCoreStartupWithStack"));
        assert!(!matcher.observe(b"[BdsDxe]"));
    }

    #[test]
    fn serial_output_without_enabled_diagnostic_is_ignored() {
        let mut diagnostics = OvmfSecDiagnostics::new();

        assert!(!diagnostics.observe(91, OVMF_SEC_MARKER.as_bytes()));
        assert!(diagnostics.matchers.is_empty());
    }

    #[test]
    fn diagnostic_state_is_isolated_between_vms() {
        let mut diagnostics = OvmfSecDiagnostics::new();
        diagnostics.activate(1, true, true);
        diagnostics.activate(2, true, true);

        assert!(!diagnostics.observe(1, b"SecCoreStartup"));
        assert!(diagnostics.observe(2, OVMF_SEC_MARKER.as_bytes()));
        assert!(diagnostics.observe(1, b"WithStack("));
        assert!(!diagnostics.observe(2, OVMF_SEC_MARKER.as_bytes()));
    }

    #[test]
    fn enabling_an_active_vm_does_not_reset_a_partial_marker() {
        let mut diagnostics = OvmfSecDiagnostics::new();
        diagnostics.activate(5, true, true);
        assert!(!diagnostics.observe(5, b"SecCoreStartup"));

        diagnostics.activate(5, true, true);

        assert!(diagnostics.observe(5, b"WithStack("));
    }

    #[test]
    fn prepare_or_registration_failure_does_not_create_diagnostic_state() {
        let mut diagnostics = OvmfSecDiagnostics::new();

        diagnostics.activate(6, true, false);

        assert!(diagnostics.matchers.is_empty());
        assert!(!diagnostics.observe(6, OVMF_SEC_MARKER.as_bytes()));
    }

    #[test]
    fn duplicate_id_activation_does_not_reset_the_registered_vm_matcher() {
        let mut diagnostics = OvmfSecDiagnostics::new();
        diagnostics.activate(8, true, true);
        assert!(!diagnostics.observe(8, b"SecCoreStartup"));

        diagnostics.activate(8, true, false);

        assert!(diagnostics.observe(8, b"WithStack("));
    }

    #[test]
    fn forgetting_a_vm_removes_and_resets_its_diagnostic_state() {
        let mut diagnostics = OvmfSecDiagnostics::new();
        diagnostics.activate(7, true, true);
        assert!(!diagnostics.observe(7, b"SecCoreStartup"));

        diagnostics.forget(7);
        assert!(!diagnostics.observe(7, b"WithStack("));

        diagnostics.activate(7, true, true);
        assert!(diagnostics.observe(7, OVMF_SEC_MARKER.as_bytes()));
    }

    #[test]
    fn reset_reactivates_from_the_instance_owned_firmware_state() {
        let mut diagnostics = OvmfSecDiagnostics::new();
        diagnostics.activate(9, true, true);
        assert!(diagnostics.observe(9, OVMF_SEC_MARKER.as_bytes()));

        diagnostics.forget(9);
        diagnostics.activate(9, true, true);

        assert!(diagnostics.observe(9, OVMF_SEC_MARKER.as_bytes()));
    }
}
