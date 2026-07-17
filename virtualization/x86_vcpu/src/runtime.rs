//! Runtime interface over the selected Intel VMX or AMD SVM implementation.

use core::sync::atomic::{AtomicU8, Ordering};

use raw_cpuid::CpuId;

use crate::{
    X86GuestPhysAddr, X86HostOps, X86HostPhysAddr, X86NestedPagingConfig, X86VcpuCreateConfig,
    X86VcpuError, X86VcpuResult, X86VcpuSetupConfig, X86VmExit,
    svm::{SvmPerCpuState, SvmVcpu},
    vmx::{VmxPerCpuState, VmxVcpu},
};

const UNSELECTED: u8 = 0;
const VMX: u8 = 1;
const SVM: u8 = 2;

// The bootstrap CPU publishes its immutable backend choice with Release. Secondary CPUs use
// Acquire before allocating backend-specific state, so they cannot observe an uninitialized or
// conflicting selection.
static SELECTED_BACKEND: AtomicU8 = AtomicU8::new(UNSELECTED);

/// The hardware virtualization extension selected for this x86 host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum X86VirtualizationBackend {
    /// Intel Virtual Machine Extensions.
    Vmx,
    /// AMD Secure Virtual Machine extensions.
    Svm,
}

impl X86VirtualizationBackend {
    const fn as_raw(self) -> u8 {
        match self {
            Self::Vmx => VMX,
            Self::Svm => SVM,
        }
    }

    const fn from_raw(raw: u8) -> Option<Self> {
        match raw {
            VMX => Some(Self::Vmx),
            SVM => Some(Self::Svm),
            _ => None,
        }
    }
}

/// CPU virtualization capability bits used to select an x86 backend.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct X86VirtualizationCapabilities {
    /// CPUID.01H:ECX.VMX.
    vmx: bool,
    /// CPUID.80000001H:ECX.SVM.
    svm: bool,
}

/// Runtime-dispatched x86 per-CPU virtualization state.
pub struct X86PerCpuState<H: X86HostOps> {
    inner: X86PerCpuStateInner<H>,
}

enum X86PerCpuStateInner<H: X86HostOps> {
    Vmx(VmxPerCpuState<H>),
    Svm(SvmPerCpuState<H>),
}

impl<H: X86HostOps> X86PerCpuState<H> {
    /// Create state for one host CPU using the backend selected by initialization.
    ///
    /// # Errors
    ///
    /// Returns [`X86VcpuError::BadState`] when initialization has not selected
    /// a compatible backend for this CPU. Propagates backend state-allocation errors.
    pub fn new(cpu_id: usize) -> X86VcpuResult<Self> {
        match validate_current_cpu_backend()? {
            X86VirtualizationBackend::Vmx => VmxPerCpuState::new(cpu_id).map(|inner| Self {
                inner: X86PerCpuStateInner::Vmx(inner),
            }),
            X86VirtualizationBackend::Svm => SvmPerCpuState::new(cpu_id).map(|inner| Self {
                inner: X86PerCpuStateInner::Svm(inner),
            }),
        }
    }

    /// Return whether hardware virtualization is enabled on this CPU.
    pub fn is_enabled(&self) -> bool {
        match &self.inner {
            X86PerCpuStateInner::Vmx(state) => state.is_enabled(),
            X86PerCpuStateInner::Svm(state) => state.is_enabled(),
        }
    }

    /// Enable the selected virtualization extension on this CPU.
    ///
    /// # Errors
    ///
    /// Returns a capability-selection error when the current CPU does not
    /// expose the backend selected by the bootstrap CPU. Propagates the
    /// selected backend's hardware-enable error.
    pub fn hardware_enable(&mut self) -> X86VcpuResult {
        validate_current_cpu_backend()?;
        match &mut self.inner {
            X86PerCpuStateInner::Vmx(state) => state.hardware_enable(),
            X86PerCpuStateInner::Svm(state) => state.hardware_enable(),
        }
    }

    /// Disable the selected virtualization extension on this CPU.
    ///
    /// # Errors
    ///
    /// Propagates the selected backend's hardware-disable error.
    pub fn hardware_disable(&mut self) -> X86VcpuResult {
        match &mut self.inner {
            X86PerCpuStateInner::Vmx(state) => state.hardware_disable(),
            X86PerCpuStateInner::Svm(state) => state.hardware_disable(),
        }
    }
}

/// Runtime-dispatched x86 virtual CPU.
pub struct X86Vcpu<H: X86HostOps> {
    inner: X86VcpuInner<H>,
}

enum X86VcpuInner<H: X86HostOps> {
    Vmx(VmxVcpu<H>),
    Svm(SvmVcpu<H>),
}

macro_rules! dispatch_vcpu {
    ($self:expr, $method:ident $(, $arg:expr)*) => {
        match &mut $self.inner {
            X86VcpuInner::Vmx(vcpu) => vcpu.$method($($arg),*),
            X86VcpuInner::Svm(vcpu) => vcpu.$method($($arg),*),
        }
    };
}

impl<H: X86HostOps> X86Vcpu<H> {
    /// Create a virtual CPU using the backend selected during initialization.
    ///
    /// # Errors
    ///
    /// Returns [`X86VcpuError::BadState`] when initialization has not selected
    /// a backend. Propagates vCPU-creation errors from that backend.
    pub fn new_with_config(
        vm_id: usize,
        vcpu_id: usize,
        config: X86VcpuCreateConfig,
    ) -> X86VcpuResult<Self> {
        match selected_backend().ok_or(X86VcpuError::BadState)? {
            X86VirtualizationBackend::Vmx => {
                VmxVcpu::new_with_config(vm_id, vcpu_id, config).map(|inner| Self {
                    inner: X86VcpuInner::Vmx(inner),
                })
            }
            X86VirtualizationBackend::Svm => {
                SvmVcpu::new_with_config(vm_id, vcpu_id, config).map(|inner| Self {
                    inner: X86VcpuInner::Svm(inner),
                })
            }
        }
    }

    /// Set the guest entry address.
    ///
    /// # Errors
    ///
    /// Propagates validation errors from the selected backend.
    pub fn set_entry(&mut self, entry: X86GuestPhysAddr) -> X86VcpuResult {
        dispatch_vcpu!(self, set_entry, entry)
    }

    /// Install the guest nested page-table configuration.
    ///
    /// # Errors
    ///
    /// Propagates configuration errors from the selected backend.
    pub fn set_nested_page_table(&mut self, config: X86NestedPagingConfig) -> X86VcpuResult {
        dispatch_vcpu!(self, set_nested_page_table, config)
    }

    /// Configure emulated devices and host I/O interception.
    ///
    /// # Errors
    ///
    /// Propagates setup errors from the selected backend.
    pub fn setup(&mut self, config: X86VcpuSetupConfig) -> X86VcpuResult {
        dispatch_vcpu!(self, setup, config)
    }

    /// Enter the guest until the selected backend reports a VM exit.
    ///
    /// # Errors
    ///
    /// Propagates guest-entry and VM-exit decoding errors from the selected backend.
    pub fn run(&mut self) -> X86VcpuResult<X86VmExit> {
        dispatch_vcpu!(self, run)
    }

    /// Bind this vCPU to the current host CPU.
    ///
    /// # Errors
    ///
    /// Propagates binding errors from the selected backend.
    pub fn bind(&mut self) -> X86VcpuResult {
        dispatch_vcpu!(self, bind)
    }

    /// Release this vCPU from the current host CPU.
    ///
    /// # Errors
    ///
    /// Propagates unbinding errors from the selected backend.
    pub fn unbind(&mut self) -> X86VcpuResult {
        dispatch_vcpu!(self, unbind)
    }

    /// Set one guest general-purpose register.
    pub fn set_gpr(&mut self, reg: usize, value: usize) {
        dispatch_vcpu!(self, set_gpr, reg, value)
    }

    /// Queue an edge-triggered interrupt for the guest.
    ///
    /// # Errors
    ///
    /// Propagates interrupt-injection errors from the selected backend.
    pub fn inject_interrupt(&mut self, vector: usize) -> X86VcpuResult {
        dispatch_vcpu!(self, inject_interrupt, vector)
    }

    /// Queue an interrupt with its trigger mode for the guest.
    ///
    /// # Errors
    ///
    /// Propagates interrupt-injection errors from the selected backend.
    pub fn inject_interrupt_with_trigger(
        &mut self,
        vector: usize,
        level_triggered: bool,
    ) -> X86VcpuResult {
        dispatch_vcpu!(self, inject_interrupt_with_trigger, vector, level_triggered)
    }

    /// Handle a guest local-APIC end-of-interrupt notification.
    pub fn handle_eoi(&mut self) -> Option<u8> {
        dispatch_vcpu!(self, handle_eoi)
    }

    /// Set the guest return register value.
    pub fn set_return_value(&mut self, value: usize) {
        dispatch_vcpu!(self, set_return_value, value)
    }
}

/// Return whether the current CPU exposes exactly one supported virtualization extension.
///
/// This is a pure CPUID query and does not select a process-wide backend.
pub fn has_hardware_support() -> bool {
    detect_current_backend().is_ok()
}

/// Detect and freeze the x86 backend used by this AxVM runtime.
///
/// # Errors
///
/// Returns [`X86VcpuError::Unsupported`] when this CPU exposes neither VMX
/// nor SVM, [`X86VcpuError::InvalidData`] when it exposes both, and
/// [`X86VcpuError::BadState`] when another CPU selected a different backend.
pub fn initialize_hardware_support() -> X86VcpuResult {
    select_current_backend().map(|_| ())
}

/// Guest-physical nested-paging formats supported by x86 hardware backends.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum X86NestedPagingFormat {
    /// Intel Extended Page Tables.
    Ept,
    /// AMD Nested Page Tables.
    Npt,
}

/// Return the nested-paging format selected during initialization.
///
/// # Errors
///
/// Returns [`X86VcpuError::BadState`] when initialization has not selected a backend.
pub fn selected_nested_paging_format() -> X86VcpuResult<X86NestedPagingFormat> {
    match selected_backend().ok_or(X86VcpuError::BadState)? {
        X86VirtualizationBackend::Vmx => Ok(X86NestedPagingFormat::Ept),
        X86VirtualizationBackend::Svm => Ok(X86NestedPagingFormat::Npt),
    }
}

/// Return whether the selected backend needs an APIC-access backing page.
///
/// # Errors
///
/// Returns [`X86VcpuError::BadState`] when initialization has not selected a backend.
pub fn requires_apic_access_page() -> X86VcpuResult<bool> {
    Ok(matches!(
        selected_backend().ok_or(X86VcpuError::BadState)?,
        X86VirtualizationBackend::Vmx
    ))
}

/// Return the guest physical address reserved for the APIC-access page.
///
/// # Errors
///
/// Returns [`X86VcpuError::BadState`] when no backend has been selected and
/// [`X86VcpuError::Unsupported`] when the selected backend is SVM.
pub fn apic_access_page_gpa() -> X86VcpuResult<X86GuestPhysAddr> {
    match selected_backend() {
        Some(X86VirtualizationBackend::Vmx) => {
            Ok(X86GuestPhysAddr::from(crate::vmx::X86_APIC_ACCESS_GPA))
        }
        Some(X86VirtualizationBackend::Svm) => Err(X86VcpuError::Unsupported),
        None => Err(X86VcpuError::BadState),
    }
}

/// Return the host page that backs the APIC-access page.
///
/// # Errors
///
/// Returns [`X86VcpuError::BadState`] when no backend has been selected and
/// [`X86VcpuError::Unsupported`] when the selected backend is SVM.
pub fn apic_access_page_addr<H: X86HostOps>() -> X86VcpuResult<X86HostPhysAddr> {
    match selected_backend() {
        Some(X86VirtualizationBackend::Vmx) => Ok(crate::vmx::x86_apic_access_page_addr::<H>()),
        Some(X86VirtualizationBackend::Svm) => Err(X86VcpuError::Unsupported),
        None => Err(X86VcpuError::BadState),
    }
}

/// Select the only usable x86 virtualization backend from CPUID capabilities.
///
/// A CPU advertising both extensions is rejected because AxVM must use one
/// backend consistently on every host CPU.
fn select_backend_from_capabilities(
    capabilities: X86VirtualizationCapabilities,
) -> X86VcpuResult<X86VirtualizationBackend> {
    match (capabilities.vmx, capabilities.svm) {
        (true, false) => Ok(X86VirtualizationBackend::Vmx),
        (false, true) => Ok(X86VirtualizationBackend::Svm),
        (false, false) => Err(X86VcpuError::Unsupported),
        (true, true) => Err(X86VcpuError::InvalidData),
    }
}

/// Detect the virtualization backend exposed to the current CPU.
fn detect_current_backend() -> X86VcpuResult<X86VirtualizationBackend> {
    let cpuid = CpuId::new();
    select_backend_from_capabilities(X86VirtualizationCapabilities {
        vmx: cpuid
            .get_feature_info()
            .is_some_and(|features| features.has_vmx()),
        svm: cpuid
            .get_extended_processor_and_feature_identifiers()
            .is_some_and(|features| features.has_svm()),
    })
}

/// Detect and freeze the backend used by the current AxVM process.
///
/// Calling this on another CPU succeeds only when it exposes the backend
/// already selected by the bootstrap CPU.
fn select_current_backend() -> X86VcpuResult<X86VirtualizationBackend> {
    let detected = detect_current_backend()?;
    match SELECTED_BACKEND.compare_exchange(
        UNSELECTED,
        detected.as_raw(),
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => Ok(detected),
        Err(raw) if X86VirtualizationBackend::from_raw(raw) == Some(detected) => Ok(detected),
        Err(_) => Err(X86VcpuError::BadState),
    }
}

/// Return the backend selected during AxVM initialization.
fn selected_backend() -> Option<X86VirtualizationBackend> {
    X86VirtualizationBackend::from_raw(SELECTED_BACKEND.load(Ordering::Acquire))
}

/// Verify that the current CPU supports the backend selected by the bootstrap CPU.
fn validate_current_cpu_backend() -> X86VcpuResult<X86VirtualizationBackend> {
    let selected = selected_backend().ok_or(X86VcpuError::BadState)?;
    if detect_current_backend()? == selected {
        Ok(selected)
    } else {
        Err(X86VcpuError::BadState)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_vmx_when_only_vmx_is_exposed() {
        assert_eq!(
            select_backend_from_capabilities(X86VirtualizationCapabilities {
                vmx: true,
                svm: false,
            }),
            Ok(X86VirtualizationBackend::Vmx)
        );
    }

    #[test]
    fn selects_svm_when_only_svm_is_exposed() {
        assert_eq!(
            select_backend_from_capabilities(X86VirtualizationCapabilities {
                vmx: false,
                svm: true,
            }),
            Ok(X86VirtualizationBackend::Svm)
        );
    }

    #[test]
    fn rejects_missing_or_ambiguous_virtualization_extensions() {
        assert_eq!(
            select_backend_from_capabilities(X86VirtualizationCapabilities::default()),
            Err(X86VcpuError::Unsupported)
        );
        assert_eq!(
            select_backend_from_capabilities(X86VirtualizationCapabilities {
                vmx: true,
                svm: true,
            }),
            Err(X86VcpuError::InvalidData)
        );
    }
}
