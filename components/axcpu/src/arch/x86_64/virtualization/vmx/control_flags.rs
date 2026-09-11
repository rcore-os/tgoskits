//! VMX control-word capabilities and reserved-bit validation.

use super::{VmcsControl32, VmcsField, VmcsReadWrite, VmxControls};
use crate::{
    registers::Msr,
    virtualization::{Backend, ControlMemory, VirtualizationError},
};

/// A hardware control word and its matching capability MSR family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VmxControl {
    /// Pin-based execution controls.
    PinBased,
    /// Primary processor-based execution controls.
    Primary,
    /// Secondary processor-based execution controls.
    Secondary,
    /// VM-entry controls.
    Entry,
    /// VM-exit controls.
    Exit,
}

/// Capability evidence for one VMX control word on the current CPU.
#[derive(Clone, Copy, Debug)]
pub struct VmxControlCapabilities {
    mandatory: u32,
    permitted: u32,
    legacy_default: u32,
}

/// A requested VMX control change cannot be represented on this CPU.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum VmxControlError {
    /// The request sets and clears the same bit.
    #[error("VMX control masks overlap")]
    ConflictingMasks,
    /// Requested one-bits are forbidden by the hardware capability.
    #[error("unsupported VMX control set mask {0:#x}")]
    UnsupportedSet(u32),
    /// Requested zero-bits are mandatory one-bits.
    #[error("unsupported VMX control clear mask {0:#x}")]
    UnsupportedClear(u32),
    /// Hardware capability information or VMCS access is unavailable.
    #[error(transparent)]
    Cpu(#[from] VirtualizationError),
}

impl VmxControlCapabilities {
    /// Reports whether every requested one-bit is permitted.
    pub const fn allows(self, bits: u32) -> bool {
        self.permitted & bits == bits
    }

    fn adjust(self, previous: u32, set: u32, clear: u32) -> Result<u32, VmxControlError> {
        if set & clear != 0 {
            return Err(VmxControlError::ConflictingMasks);
        }
        if set & !self.permitted != 0 {
            return Err(VmxControlError::UnsupportedSet(set & !self.permitted));
        }
        if clear & self.mandatory != 0 {
            return Err(VmxControlError::UnsupportedClear(clear & self.mandatory));
        }
        Ok(self.mandatory | (previous & self.permitted & !(set | clear)) | set)
    }
}

impl VmxControl {
    fn field(self) -> VmcsField<u32, VmcsReadWrite> {
        match self {
            Self::PinBased => VmcsControl32::PINBASED_EXEC_CONTROLS,
            Self::Primary => VmcsControl32::PRIMARY_PROCBASED_EXEC_CONTROLS,
            Self::Secondary => VmcsControl32::SECONDARY_PROCBASED_EXEC_CONTROLS,
            Self::Entry => VmcsControl32::VMENTRY_CONTROLS,
            Self::Exit => VmcsControl32::VMEXIT_CONTROLS,
        }
    }

    /// Probes this CPU's true-control capability, or its legacy equivalent.
    ///
    /// # Safety
    /// Execute at ring 0 with readable VMX capability MSRs and retain this CPU
    /// until consuming the returned capability information.
    pub unsafe fn capabilities(self) -> Result<VmxControlCapabilities, VirtualizationError> {
        if Backend::detect() != Some(Backend::Vmx) {
            return Err(VirtualizationError::Unavailable);
        }
        let (legacy, true_control) = match self {
            Self::PinBased => (Msr::Ia32VmxPinbasedCtls, Msr::Ia32VmxTruePinbasedCtls),
            Self::Primary => (Msr::Ia32VmxProcbasedCtls, Msr::Ia32VmxTrueProcbasedCtls),
            Self::Secondary => (Msr::Ia32VmxProcbasedCtls2, Msr::Ia32VmxProcbasedCtls2),
            Self::Entry => (Msr::Ia32VmxEntryCtls, Msr::Ia32VmxTrueEntryCtls),
            Self::Exit => (Msr::Ia32VmxExitCtls, Msr::Ia32VmxTrueExitCtls),
        };
        // SAFETY: VMX was detected; true-control registers are read only when
        // VMX_BASIC advertises them. The legacy MSR supplies the initial defaults.
        let (legacy_value, active) = unsafe {
            let value = legacy.read();
            let active = if legacy != true_control && Msr::Ia32VmxBasic.read() & (1 << 55) != 0 {
                true_control.read()
            } else {
                value
            };
            (value, active)
        };
        Ok(VmxControlCapabilities {
            mandatory: active as u32,
            permitted: (active >> 32) as u32,
            legacy_default: legacy_value as u32,
        })
    }
}

impl<M: ControlMemory> VmxControls<M> {
    fn apply_control(
        &mut self,
        control: VmxControl,
        previous: Option<u32>,
        set: u32,
        clear: u32,
    ) -> Result<(), VmxControlError> {
        if !self.vmcs().is_bound() {
            return Err(VirtualizationError::NotEnabled.into());
        }
        // SAFETY: the VMCS bind contract retains this privileged VMX CPU.
        let capability = unsafe { control.capabilities()? };
        let value = capability.adjust(previous.unwrap_or(capability.legacy_default), set, clear)?;
        self.write(control.field(), value)?;
        Ok(())
    }

    /// Initializes a control word from hardware legacy defaults and explicit policy.
    pub fn initialize_control(
        &mut self,
        control: VmxControl,
        set: u32,
        clear: u32,
    ) -> Result<(), VmxControlError> {
        self.apply_control(control, None, set, clear)
    }

    /// Changes selected bits while preserving untouched flexible controls.
    pub fn update_control(
        &mut self,
        control: VmxControl,
        set: u32,
        clear: u32,
    ) -> Result<(), VmxControlError> {
        let previous = self.read(control.field())?;
        self.apply_control(control, Some(previous), set, clear)
    }
}

impl<M: ControlMemory> VmxControls<M> {
    /// Synchronizes EFER.LMA and VM-entry mode after a guest CR0 transition.
    /// `guest_cr0` is the guest-visible value, before VMX fixed-bit adjustment.
    pub fn synchronize_long_mode(&mut self, guest_cr0: u64) -> Result<(), VmxControlError> {
        use super::VmcsGuest64;
        const LME: u64 = 1 << 8;
        const LMA: u64 = 1 << 10;
        const PG: u64 = 1 << 31;
        const IA32E: u32 = 1 << 9;
        let previous = self.read(VmcsGuest64::IA32_EFER)?;
        let previous_entry = self.read(VmcsControl32::VMENTRY_CONTROLS)?;
        let active = previous & LME != 0 && guest_cr0 & PG != 0;
        self.update_control(
            VmxControl::Entry,
            if active { IA32E } else { 0 },
            if active { 0 } else { IA32E },
        )?;
        let next = (previous & !LMA) | if active { LMA } else { 0 };
        if let Err(error) = self.write(VmcsGuest64::IA32_EFER, next) {
            // Restore the already-validated control word before reporting failure.
            self.write(VmcsControl32::VMENTRY_CONTROLS, previous_entry)?;
            return Err(error.into());
        }
        Ok(())
    }
}
