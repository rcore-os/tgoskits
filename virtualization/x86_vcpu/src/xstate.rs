use core::mem::offset_of;

use ax_cpu_local::CpuPin;
use raw_cpuid::{CpuId, CpuIdResult};
use x86::controlregs::{Xcr0, xcr0 as xcr0_read};
use x86_64::registers::control::{Cr4, Cr4Flags};

use crate::{X86VcpuError, X86VcpuResult, msr::Msr};

const XSAVE_AREA_SIZE: usize = 1024;
const XSAVE_HEADER_OFFSET: usize = 512;
const XSAVE_LEGACY_FCW_OFFSET: usize = 0;
const XSAVE_LEGACY_MXCSR_OFFSET: usize = 24;
const XSAVE_LEGACY_REGION_WITH_HEADER_SIZE: u32 = 576;
const BACKEND_MANAGED_XCR0_MASK: u64 = 0x7;
const XCR0_AVX_STATE: u64 = 1 << 2;
const XCR0_RESET_STATE: u64 = 1;

/// One standard-format XSAVE area for the kernel-managed x87/SSE/AVX state.
#[derive(Debug)]
#[repr(C, align(64))]
struct XStateArea {
    bytes: [u8; XSAVE_AREA_SIZE],
}

impl XStateArea {
    const fn initial() -> Self {
        let mut bytes = [0; XSAVE_AREA_SIZE];
        let fcw = 0x037fu16.to_ne_bytes();
        bytes[XSAVE_LEGACY_FCW_OFFSET] = fcw[0];
        bytes[XSAVE_LEGACY_FCW_OFFSET + 1] = fcw[1];
        let mxcsr = 0x1f80u32.to_ne_bytes();
        bytes[XSAVE_LEGACY_MXCSR_OFFSET] = mxcsr[0];
        bytes[XSAVE_LEGACY_MXCSR_OFFSET + 1] = mxcsr[1];
        bytes[XSAVE_LEGACY_MXCSR_OFFSET + 2] = mxcsr[2];
        bytes[XSAVE_LEGACY_MXCSR_OFFSET + 3] = mxcsr[3];
        Self { bytes }
    }

    fn clear_components(&mut self, components: u64) {
        let xstate_bv = unsafe {
            // SAFETY: the XSAVE header starts at byte 512 and the area is
            // 64-byte aligned, so this u64 lies fully inside aligned storage.
            &mut *self
                .bytes
                .as_mut_ptr()
                .add(XSAVE_HEADER_OFFSET)
                .cast::<u64>()
        };
        *xstate_bv &= !components;
    }
}

/// Immutable extended-state capability contract shared by every CPU on which
/// one vCPU may run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
struct XStateContract {
    xsave_enabled: u8,
    xss_msr_available: u8,
    _reserved: [u8; 6],
    managed_xcr0: u64,
    standard_size: u32,
    _reserved2: u32,
}

impl XStateContract {
    fn current() -> X86VcpuResult<Self> {
        let xsave_enabled = xsave_available() && Cr4::read().contains(Cr4Flags::OSXSAVE);
        let xss_msr_available = xsave_enabled && xsaves_available();
        let managed_xcr0 = if xsave_enabled {
            unsafe { xcr0_read().bits() }
        } else {
            0
        };
        let host_xss = if xss_msr_available {
            Msr::IA32_XSS.read()
        } else {
            0
        };
        let standard_size = if xsave_enabled {
            core::arch::x86_64::__cpuid_count(CPUID_EXTENDED_STATE, 0).ebx
        } else {
            512
        };

        // Supervisor xstate needs an XSAVES compacted-format owner. Until the
        // backend provides one, accepting a non-zero host XSS would let guest
        // execution overwrite state that the standard XSAVE areas omit.
        if host_xss != 0 {
            return Err(X86VcpuError::Unsupported);
        }

        // This backend deliberately manages only x87/SSE/AVX in a 1024-byte
        // standard-format area. Reject a wider live host XCR0 instead of
        // silently exposing state that cannot be transferred atomically.
        if xsave_enabled
            && (managed_xcr0 & !BACKEND_MANAGED_XCR0_MASK != 0
                || standard_size as usize > XSAVE_AREA_SIZE)
        {
            return Err(X86VcpuError::Unsupported);
        }

        Ok(Self {
            xsave_enabled: u8::from(xsave_enabled),
            xss_msr_available: u8::from(xss_msr_available),
            _reserved: [0; 6],
            managed_xcr0,
            standard_size,
            _reserved2: 0,
        })
    }

    const fn xsave_enabled(self) -> bool {
        self.xsave_enabled != 0
    }
}

/// Extended processor state switched between host and guest.
#[derive(Debug)]
#[repr(C)]
pub struct XState {
    pub guest_xcr0: u64,

    host_xcr0: u64,
    host_xss: u64,
    guest_xss: u64,
    contract: XStateContract,
    /// Scratch copy of the host task's extended register contents.
    host_area: XStateArea,
    /// Persistent extended register contents owned by this vCPU.
    guest_area: XStateArea,
}

pub(crate) const XSTATE_GUEST_XCR0_OFFSET: usize = offset_of!(XState, guest_xcr0);
pub(crate) const XSTATE_HOST_XCR0_OFFSET: usize = offset_of!(XState, host_xcr0);
pub(crate) const XSTATE_HOST_XSS_OFFSET: usize = offset_of!(XState, host_xss);
pub(crate) const XSTATE_GUEST_XSS_OFFSET: usize = offset_of!(XState, guest_xss);
pub(crate) const XSTATE_XSAVE_AVAILABLE_OFFSET: usize =
    offset_of!(XState, contract) + offset_of!(XStateContract, xsave_enabled);
pub(crate) const XSTATE_XSAVES_AVAILABLE_OFFSET: usize =
    offset_of!(XState, contract) + offset_of!(XStateContract, xss_msr_available);
pub(crate) const XSTATE_HOST_AREA_OFFSET: usize = offset_of!(XState, host_area);
pub(crate) const XSTATE_GUEST_AREA_OFFSET: usize = offset_of!(XState, guest_area);
pub(crate) const IA32_XSS_MSR: u32 = 0xda0;

const CPUID_STRUCTURED_EXTENDED_FEATURES: u32 = 0x07;
const CPUID_FEATURE_INFO: u32 = 0x01;
const CPUID_EXTENDED_STATE: u32 = 0x0d;
const CPUID_AMX_TILE: u32 = 0x1d;
const CPUID_AMX_TMUX: u32 = 0x1e;
const CPUID_AVX10: u32 = 0x24;

const XCR0_MPX_MASK: u64 = (1 << 3) | (1 << 4);
const XCR0_AVX512_MASK: u64 = (1 << 5) | (1 << 6) | (1 << 7);
const XCR0_PKRU_MASK: u64 = 1 << 9;
const CPUID_FEATURE_XSAVE: u32 = 1 << 26;
const CPUID_FEATURE_OSXSAVE: u32 = 1 << 27;
const CPUID_FEATURE_AVX: u32 = 1 << 28;
const CPUID_FEATURE_FMA: u32 = 1 << 12;
const CPUID_FEATURE_F16C: u32 = 1 << 29;

const fn empty_cpuid_result() -> CpuIdResult {
    CpuIdResult {
        eax: 0,
        ebx: 0,
        ecx: 0,
        edx: 0,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum XsetbvFault {
    InvalidOpcode,
    GeneralProtection,
}

impl XState {
    pub fn new() -> X86VcpuResult<Self> {
        let contract = XStateContract::current()?;

        Ok(Self {
            host_xcr0: contract.managed_xcr0,
            guest_xcr0: if contract.xsave_enabled() {
                XCR0_RESET_STATE
            } else {
                0
            },
            host_xss: 0,
            guest_xss: 0,
            contract,
            host_area: XStateArea::initial(),
            guest_area: XStateArea::initial(),
        })
    }

    /// Returns whether the kernel task context can preserve every component
    /// enabled by this guest XCR0 value.
    pub fn supports_guest_xcr0(&self, value: u64) -> bool {
        self.contract.xsave_enabled() && value & !self.contract.managed_xcr0 == 0
    }

    /// Validates XSETBV against the same capability closure used for CPUID.
    ///
    /// The backend translates the returned fault into a guest exception; a
    /// malformed guest value never escapes as a host-side error.
    pub(crate) fn validate_guest_xsetbv(
        &self,
        index: u64,
        value: u64,
        guest_osxsave: bool,
    ) -> Result<u64, XsetbvFault> {
        if !self.contract.xsave_enabled() || !guest_osxsave {
            return Err(XsetbvFault::InvalidOpcode);
        }
        if index != 0 {
            return Err(XsetbvFault::GeneralProtection);
        }

        let xcr0 = Xcr0::from_bits(value).ok_or(XsetbvFault::GeneralProtection)?;
        let avx512_state = xcr0.contains(Xcr0::XCR0_OPMASK_STATE)
            || xcr0.contains(Xcr0::XCR0_ZMM_HI256_STATE)
            || xcr0.contains(Xcr0::XCR0_HI16_ZMM_STATE);
        let avx512_state_complete = xcr0.contains(Xcr0::XCR0_OPMASK_STATE)
            && xcr0.contains(Xcr0::XCR0_ZMM_HI256_STATE)
            && xcr0.contains(Xcr0::XCR0_HI16_ZMM_STATE);
        let valid = self.supports_guest_xcr0(xcr0.bits())
            && xcr0.contains(Xcr0::XCR0_FPU_MMX_STATE)
            && (!xcr0.contains(Xcr0::XCR0_AVX_STATE) || xcr0.contains(Xcr0::XCR0_SSE_STATE))
            && !(xcr0.contains(Xcr0::XCR0_BNDCSR_STATE) ^ xcr0.contains(Xcr0::XCR0_BNDREG_STATE))
            && (!avx512_state
                || (avx512_state_complete
                    && xcr0.contains(Xcr0::XCR0_AVX_STATE)
                    && xcr0.contains(Xcr0::XCR0_SSE_STATE)));

        valid
            .then_some(xcr0.bits())
            .ok_or(XsetbvFault::GeneralProtection)
    }

    /// Verifies that the pinned current CPU implements the immutable contract
    /// captured when this vCPU was created.
    pub fn validate_current_cpu(&self, _cpu_pin: &CpuPin) -> X86VcpuResult {
        if XStateContract::current()? == self.contract {
            Ok(())
        } else {
            Err(X86VcpuError::Unsupported)
        }
    }

    /// Captures the host-owned control state before the assembly world switch.
    ///
    /// The caller must have disabled host interrupts. Installing guest values
    /// and restoring these values are deliberately performed by the final
    /// naked world-switch assembly, so Rust never executes with guest XCR0/XSS.
    pub fn capture_host(&mut self) {
        self.host_xcr0 = self.contract.managed_xcr0;
        self.host_xss = 0;
    }

    /// Commits a validated guest XCR0 value and initializes components that
    /// the guest disabled. XSETBV is intercepted, so only the vCPU owner calls
    /// this after the architectural dependency checks have passed.
    pub fn set_guest_xcr0(&mut self, value: u64) {
        let disabled = self.guest_xcr0 & !value;
        self.guest_area.clear_components(disabled);
        self.guest_xcr0 = value;
    }

    fn guest_xstate_size(&self) -> u32 {
        if !self.contract.xsave_enabled() {
            return 0;
        }

        if self.guest_xcr0 & XCR0_AVX_STATE != 0 {
            self.contract.standard_size
        } else {
            XSAVE_LEGACY_REGION_WITH_HEADER_SIZE.min(self.contract.standard_size)
        }
    }

    /// Applies the xstate-dependent CPUID policy shared by VMX and SVM.
    ///
    /// The guest may only discover instruction families whose architectural
    /// state is included in `managed_xcr0`. Supervisor xstate is intentionally
    /// hidden because this backend exposes a fixed zero IA32_XSS view.
    pub(crate) fn filter_guest_cpuid(
        &self,
        leaf: u32,
        subleaf: u32,
        result: CpuIdResult,
        guest_osxsave: bool,
    ) -> CpuIdResult {
        match leaf {
            CPUID_FEATURE_INFO => self.filter_feature_info(result, guest_osxsave),
            CPUID_STRUCTURED_EXTENDED_FEATURES => {
                self.filter_structured_extended_features(subleaf, result)
            }
            CPUID_EXTENDED_STATE => self.filter_extended_state_cpuid(subleaf, result),
            // AMX and AVX10 require state and capability virtualization that
            // the fixed kernel-managed xstate contract does not provide.
            CPUID_AMX_TILE | CPUID_AMX_TMUX | CPUID_AVX10 => empty_cpuid_result(),
            _ => result,
        }
    }

    fn filter_feature_info(&self, mut result: CpuIdResult, guest_osxsave: bool) -> CpuIdResult {
        if !self.contract.xsave_enabled() {
            result.ecx &= !(CPUID_FEATURE_XSAVE
                | CPUID_FEATURE_OSXSAVE
                | CPUID_FEATURE_AVX
                | CPUID_FEATURE_FMA
                | CPUID_FEATURE_F16C);
            return result;
        }

        if guest_osxsave {
            result.ecx |= CPUID_FEATURE_OSXSAVE;
        } else {
            result.ecx &= !CPUID_FEATURE_OSXSAVE;
        }

        if self.contract.managed_xcr0 & XCR0_AVX_STATE == 0 {
            result.ecx &= !(CPUID_FEATURE_AVX | CPUID_FEATURE_FMA | CPUID_FEATURE_F16C);
        }
        result
    }

    fn filter_structured_extended_features(
        &self,
        subleaf: u32,
        mut result: CpuIdResult,
    ) -> CpuIdResult {
        match subleaf {
            0 => {
                const MPX: u32 = 1 << 14;
                const AVX512_EBX: u32 = (1 << 16)
                    | (1 << 17)
                    | (1 << 21)
                    | (1 << 26)
                    | (1 << 27)
                    | (1 << 28)
                    | (1 << 30)
                    | (1 << 31);
                const INTEL_PT: u32 = 1 << 25;
                const PKU_OSPKE: u32 = (1 << 3) | (1 << 4);
                const AVX512_ECX: u32 = (1 << 1) | (1 << 6) | (1 << 11) | (1 << 12) | (1 << 14);
                const SHSTK: u32 = 1 << 7;
                const ENQCMD: u32 = 1 << 29;
                const PKS: u32 = 1 << 31;
                const AVX512_EDX: u32 = (1 << 2) | (1 << 3) | (1 << 8) | (1 << 23);
                const UINTR: u32 = 1 << 5;
                const ARCH_LBR: u32 = 1 << 19;
                const IBT: u32 = 1 << 20;
                const AMX: u32 = (1 << 22) | (1 << 24) | (1 << 25);

                if self.contract.managed_xcr0 & XCR0_MPX_MASK != XCR0_MPX_MASK {
                    result.ebx &= !MPX;
                }
                if self.contract.managed_xcr0 & XCR0_AVX512_MASK != XCR0_AVX512_MASK {
                    result.ebx &= !AVX512_EBX;
                    result.ecx &= !AVX512_ECX;
                    result.edx &= !AVX512_EDX;
                }
                if self.contract.managed_xcr0 & XCR0_PKRU_MASK == 0 {
                    result.ecx &= !PKU_OSPKE;
                }

                // These features use IA32_XSS-managed or otherwise
                // unvirtualized supervisor state, so they remain hidden even
                // when the host supports them.
                result.ebx &= !INTEL_PT;
                result.ecx &= !(SHSTK | ENQCMD | PKS);
                result.edx &= !(UINTR | ARCH_LBR | IBT | AMX);
                result
            }
            1 => {
                const AVX512_BF16: u32 = 1 << 5;
                const AMX_FP16: u32 = 1 << 21;
                const AMX_COMPLEX: u32 = 1 << 8;
                const CET_SSS: u32 = 1 << 18;
                const AVX10: u32 = 1 << 19;
                const APX: u32 = 1 << 21;

                if self.contract.managed_xcr0 & XCR0_AVX512_MASK != XCR0_AVX512_MASK {
                    result.eax &= !AVX512_BF16;
                }
                result.eax &= !AMX_FP16;
                result.edx &= !(AMX_COMPLEX | CET_SSS | AVX10 | APX);
                result
            }
            _ => result,
        }
    }

    fn filter_extended_state_cpuid(&self, subleaf: u32, mut result: CpuIdResult) -> CpuIdResult {
        if !self.contract.xsave_enabled() {
            return empty_cpuid_result();
        }

        match subleaf {
            0 => {
                result.eax = self.contract.managed_xcr0 as u32;
                result.edx = (self.contract.managed_xcr0 >> 32) as u32;
                result.ebx = self.guest_xstate_size();
                result.ecx = self.contract.standard_size;
                result
            }
            1 => {
                const XSAVEC: u32 = 1 << 1;
                const XGETBV_ECX_1: u32 = 1 << 2;
                const XSAVES_XRSTORS: u32 = 1 << 3;
                const XFD: u32 = 1 << 4;

                // Only XSAVEOPT retains the same standard-format size contract
                // as CPUID(D).0. Compact/supervisor formats need a separate
                // capability model and remain hidden.
                result.eax &= !(XSAVEC | XGETBV_ECX_1 | XSAVES_XRSTORS | XFD);
                result.ebx = 0;
                result.ecx = 0;
                result.edx = 0;
                result
            }
            component
                if component < 64 && self.contract.managed_xcr0 & (1u64 << component) != 0 =>
            {
                result
            }
            _ => empty_cpuid_result(),
        }
    }

    #[cfg(test)]
    fn new_for_test(managed_xcr0: u64, managed_xstate_size: u32) -> Self {
        Self {
            guest_xcr0: managed_xcr0,
            host_xcr0: managed_xcr0,
            host_xss: 0,
            guest_xss: 0,
            contract: XStateContract {
                xsave_enabled: 1,
                xss_msr_available: 0,
                _reserved: [0; 6],
                managed_xcr0,
                standard_size: managed_xstate_size,
                _reserved2: 0,
            },
            host_area: XStateArea::initial(),
            guest_area: XStateArea::initial(),
        }
    }
}

/// Saves host extended registers and installs guest xstate after host GPRs are safe.
///
/// The containing naked assembly must provide all named offset operands. RDI
/// must point to the object that embeds [`XState`]. No Rust call or interrupt
/// may occur until [`restore_host_xstate_from_rdi`] has run.
macro_rules! install_guest_xstate_from_rdi {
    () => {
        "
        cmp byte ptr [rdi + {xsave_available}], 0
        je 19f
        lea r8, [rdi + {host_area}]
        mov rax, [rdi + {host_xcr0}]
        mov rdx, rax
        shr rdx, 32
        xsave64 [r8]
        mov rax, [rdi + {guest_xcr0}]
        mov rdx, rax
        shr rdx, 32
        xor ecx, ecx
        xsetbv
        lea r8, [rdi + {guest_area}]
        mov rax, [rdi + {guest_xcr0}]
        mov rdx, rax
        shr rdx, 32
        xrstor64 [r8]
        cmp byte ptr [rdi + {xsaves_available}], 0
        je 20f
        mov rax, [rdi + {guest_xss}]
        mov rdx, rax
        shr rdx, 32
        mov ecx, {ia32_xss}
        wrmsr
        jmp 20f
19:
        lea r8, [rdi + {host_area}]
        fxsave64 [r8]
        lea r8, [rdi + {guest_area}]
        fxrstor64 [r8]
20:"
    };
}

/// Saves guest extended registers and restores all host xstate before Rust.
///
/// Guest GPRs must already be saved, because this sequence clobbers RAX, RCX,
/// RDX, and R8. RDI must point to the object that embeds [`XState`].
macro_rules! restore_host_xstate_from_rdi {
    () => {
        "
        cmp byte ptr [rdi + {xsave_available}], 0
        je 29f
        lea r8, [rdi + {guest_area}]
        mov rax, [rdi + {guest_xcr0}]
        mov rdx, rax
        shr rdx, 32
        xsave64 [r8]
        cmp byte ptr [rdi + {xsaves_available}], 0
        je 31f
        mov ecx, {ia32_xss}
        rdmsr
        shl rdx, 32
        or rax, rdx
        mov [rdi + {guest_xss}], rax
31:
        xor ecx, ecx
        xgetbv
        shl rdx, 32
        or rax, rdx
        mov [rdi + {guest_xcr0}], rax
        mov rax, [rdi + {host_xcr0}]
        mov rdx, rax
        shr rdx, 32
        xor ecx, ecx
        xsetbv
        lea r8, [rdi + {host_area}]
        mov rax, [rdi + {host_xcr0}]
        mov rdx, rax
        shr rdx, 32
        xrstor64 [r8]
        cmp byte ptr [rdi + {xsaves_available}], 0
        je 30f
        mov rax, [rdi + {host_xss}]
        mov rdx, rax
        shr rdx, 32
        mov ecx, {ia32_xss}
        wrmsr
        jmp 30f
29:
        lea r8, [rdi + {guest_area}]
        fxsave64 [r8]
        lea r8, [rdi + {host_area}]
        fxrstor64 [r8]
30:"
    };
}

pub fn xsave_available() -> bool {
    CpuId::new()
        .get_feature_info()
        .map(|features| features.has_xsave())
        .unwrap_or(false)
}

pub fn xsaves_available() -> bool {
    CpuId::new()
        .get_extended_state_info()
        .map(|features| features.has_xsaves_xrstors())
        .unwrap_or(false)
}

pub fn enable_xsave() {
    if xsave_available() {
        unsafe {
            Cr4::write(Cr4::read() | Cr4Flags::OSXSAVE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_xcr0_is_limited_to_kernel_managed_components() {
        let state = XState::new_for_test(0x7, 832);

        assert!(state.supports_guest_xcr0(0x3));
        assert!(state.supports_guest_xcr0(0x7));
        assert!(!state.supports_guest_xcr0(0x27));
        assert!(!state.supports_guest_xcr0(0x2ff));
    }

    #[test]
    fn leaf_d_hides_unmanaged_components_and_sizes() {
        let mut state = XState::new_for_test(0x7, 832);
        let raw = CpuIdResult {
            eax: 0x2ff,
            ebx: 2_688,
            ecx: 2_688,
            edx: 0,
        };

        let root = state.filter_extended_state_cpuid(0, raw);
        assert_eq!(root.eax, 0x7);
        assert_eq!(root.ebx, 832);
        assert_eq!(root.ecx, 832);
        assert_eq!(root.edx, 0);

        state.set_guest_xcr0(0x3);
        let legacy_only = state.filter_extended_state_cpuid(0, raw);
        assert_eq!(legacy_only.ebx, XSAVE_LEGACY_REGION_WITH_HEADER_SIZE);

        let unmanaged = state.filter_extended_state_cpuid(5, raw);
        assert_eq!(unmanaged.eax, 0);
        assert_eq!(unmanaged.ebx, 0);
        assert_eq!(unmanaged.ecx, 0);
        assert_eq!(unmanaged.edx, 0);
    }

    #[test]
    fn disabling_guest_component_clears_its_saved_presence_bit() {
        let mut state = XState::new_for_test(0x7, 832);
        state.guest_area.bytes[XSAVE_HEADER_OFFSET] = 0x7;

        state.set_guest_xcr0(0x3);

        assert_eq!(state.guest_area.bytes[XSAVE_HEADER_OFFSET], 0x3);
    }

    #[test]
    fn leaf_d_hides_supervisor_xstate_controls() {
        const XSAVES_XRSTORS: u32 = 1 << 3;
        const XFD: u32 = 1 << 4;

        let state = XState::new_for_test(0x7, 832);
        let filtered = state.filter_extended_state_cpuid(
            1,
            CpuIdResult {
                eax: XSAVES_XRSTORS | XFD,
                ebx: 2_688,
                ecx: u32::MAX,
                edx: u32::MAX,
            },
        );

        assert_eq!(filtered.eax & (XSAVES_XRSTORS | XFD), 0);
        assert_eq!(filtered.ecx, 0);
        assert_eq!(filtered.edx, 0);
    }

    #[test]
    fn structured_features_match_the_managed_xstate_mask() {
        let state = XState::new_for_test(0x7, 832);
        let filtered = state.filter_guest_cpuid(
            CPUID_STRUCTURED_EXTENDED_FEATURES,
            0,
            CpuIdResult {
                eax: 1,
                ebx: u32::MAX,
                ecx: u32::MAX,
                edx: u32::MAX,
            },
            false,
        );

        const AVX2: u32 = 1 << 5;
        const MPX_AND_AVX512: u32 = (1 << 14)
            | (1 << 16)
            | (1 << 17)
            | (1 << 21)
            | (1 << 26)
            | (1 << 27)
            | (1 << 28)
            | (1 << 30)
            | (1 << 31);
        const PKU_CET_AVX512: u32 = (1 << 1)
            | (1 << 3)
            | (1 << 4)
            | (1 << 6)
            | (1 << 7)
            | (1 << 11)
            | (1 << 12)
            | (1 << 14);
        const AMX_CET_AVX512: u32 = (1 << 2)
            | (1 << 3)
            | (1 << 8)
            | (1 << 20)
            | (1 << 22)
            | (1 << 23)
            | (1 << 24)
            | (1 << 25);

        assert_ne!(filtered.ebx & AVX2, 0, "AVX2 only needs managed AVX state");
        assert_eq!(filtered.ebx & MPX_AND_AVX512, 0);
        assert_eq!(filtered.ecx & PKU_CET_AVX512, 0);
        assert_eq!(filtered.edx & AMX_CET_AVX512, 0);
    }

    #[test]
    fn structured_subleaf_one_hides_new_xstate_families() {
        let state = XState::new_for_test(0x7, 832);
        let filtered = state.filter_guest_cpuid(
            CPUID_STRUCTURED_EXTENDED_FEATURES,
            1,
            CpuIdResult {
                eax: u32::MAX,
                ebx: u32::MAX,
                ecx: u32::MAX,
                edx: u32::MAX,
            },
            false,
        );

        const AVX512_BF16_AND_AMX_FP16: u32 = (1 << 5) | (1 << 21);
        const AMX_CET_AVX10_APX: u32 = (1 << 8) | (1 << 18) | (1 << 19) | (1 << 21);
        assert_eq!(filtered.eax & AVX512_BF16_AND_AMX_FP16, 0);
        assert_eq!(filtered.edx & AMX_CET_AVX10_APX, 0);
    }

    #[test]
    fn amx_and_avx10_information_leaves_are_hidden() {
        let state = XState::new_for_test(0x7, 832);
        let raw = CpuIdResult {
            eax: u32::MAX,
            ebx: u32::MAX,
            ecx: u32::MAX,
            edx: u32::MAX,
        };

        for leaf in [CPUID_AMX_TILE, CPUID_AMX_TMUX, CPUID_AVX10] {
            assert_eq!(
                state.filter_guest_cpuid(leaf, 0, raw, false),
                empty_cpuid_result()
            );
        }
    }

    #[test]
    fn feature_info_osxsave_follows_guest_cr4() {
        let state = XState::new_for_test(0x7, 832);
        let raw = CpuIdResult {
            eax: 0,
            ebx: 0,
            ecx: CPUID_FEATURE_XSAVE | CPUID_FEATURE_OSXSAVE | CPUID_FEATURE_AVX,
            edx: 0,
        };

        let disabled = state.filter_guest_cpuid(CPUID_FEATURE_INFO, 0, raw, false);
        let enabled = state.filter_guest_cpuid(CPUID_FEATURE_INFO, 0, raw, true);

        assert_eq!(disabled.ecx & CPUID_FEATURE_OSXSAVE, 0);
        assert_ne!(enabled.ecx & CPUID_FEATURE_OSXSAVE, 0);
        assert_ne!(enabled.ecx & CPUID_FEATURE_XSAVE, 0);
    }

    #[test]
    fn xsetbv_faults_are_derived_from_the_shared_capability_model() {
        let state = XState::new_for_test(0x7, 832);

        assert_eq!(
            state.validate_guest_xsetbv(0, 0x3, false),
            Err(XsetbvFault::InvalidOpcode)
        );
        assert_eq!(
            state.validate_guest_xsetbv(1, 0x3, true),
            Err(XsetbvFault::GeneralProtection)
        );
        assert_eq!(
            state.validate_guest_xsetbv(0, 0x5, true),
            Err(XsetbvFault::GeneralProtection)
        );
        assert_eq!(state.validate_guest_xsetbv(0, 0x7, true), Ok(0x7));
    }
}
