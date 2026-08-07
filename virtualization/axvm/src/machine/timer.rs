//! Guest-visible AArch64 architectural timer resources.

use std::{string::String, vec::Vec};

/// Guest-visible AArch64 architectural timer resources.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuestTimerProfile {
    /// Absolute path used for the standard timer node.
    pub node_path: String,
    /// Timer-node phandle retained from host firmware, when present.
    pub node_phandle: Option<u32>,
    /// Effective virtual-GIC phandle used by every interrupt specifier.
    pub interrupt_parent: Option<u32>,
    /// Raw interrupt specifiers in the binding-defined firmware order.
    pub interrupt_specifiers: Vec<Vec<u32>>,
    /// Secure physical timer PPI INTID.
    pub secure_physical_intid: u32,
    /// Non-secure physical timer PPI INTID.
    pub nonsecure_physical_intid: u32,
    /// Virtual timer PPI INTID.
    pub virtual_intid: u32,
    /// Hypervisor physical timer PPI INTID.
    pub hypervisor_intid: u32,
    /// Firmware-corrected counter frequency, if explicitly supplied.
    pub clock_frequency_hz: Option<u32>,
}

/// Invalid machine-owned AArch64 architectural timer resources.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum GuestTimerProfileError {
    /// The architectural binding requires its four mandatory interrupts and
    /// permits one optional hypervisor virtual timer interrupt.
    #[error("architectural timer requires four or five interrupts, got {count}")]
    InterruptCount { count: usize },
    /// Every interrupt must use the three-cell GIC encoding.
    #[error("architectural timer interrupt {index} has {cells} cells instead of three")]
    InterruptCells { index: usize, cells: usize },
    /// The interrupt is not a private peripheral interrupt.
    #[error(
        "architectural timer interrupt {index} is not a GIC PPI: type={interrupt_type}, \
         source={ppi_source}"
    )]
    InterruptClass {
        index: usize,
        interrupt_type: u32,
        ppi_source: u32,
    },
    /// Architectural timer outputs are level signals.
    #[error("architectural timer interrupt {index} is not level-triggered: flags={flags:#x}")]
    InterruptTrigger { index: usize, flags: u32 },
    /// The decoded mandatory interrupt identities must agree with the named profile fields.
    #[error("architectural timer INTIDs do not match their interrupt specifiers")]
    InterruptIdentity,
    /// A firmware correction must still describe a usable counter.
    #[error("architectural timer clock frequency must be nonzero")]
    ZeroFrequency,
}

impl GuestTimerProfile {
    /// Validates the complete machine profile and returns decoded INTIDs in
    /// binding-defined firmware order.
    pub(crate) fn validated_intids(&self) -> Result<Vec<u32>, GuestTimerProfileError> {
        if !(4..=5).contains(&self.interrupt_specifiers.len()) {
            return Err(GuestTimerProfileError::InterruptCount {
                count: self.interrupt_specifiers.len(),
            });
        }
        let intids = self
            .interrupt_specifiers
            .iter()
            .enumerate()
            .map(|(index, specifier)| decode_timer_ppi(index, specifier))
            .collect::<Result<Vec<_>, _>>()?;
        if intids[..4]
            != [
                self.secure_physical_intid,
                self.nonsecure_physical_intid,
                self.virtual_intid,
                self.hypervisor_intid,
            ]
        {
            return Err(GuestTimerProfileError::InterruptIdentity);
        }
        if self.clock_frequency_hz == Some(0) {
            return Err(GuestTimerProfileError::ZeroFrequency);
        }
        Ok(intids)
    }
}

pub(crate) fn decode_timer_ppi(
    index: usize,
    specifier: &[u32],
) -> Result<u32, GuestTimerProfileError> {
    let [interrupt_type, source, flags] = specifier else {
        return Err(GuestTimerProfileError::InterruptCells {
            index,
            cells: specifier.len(),
        });
    };
    if *interrupt_type != 1 || *source >= 16 {
        return Err(GuestTimerProfileError::InterruptClass {
            index,
            interrupt_type: *interrupt_type,
            ppi_source: *source,
        });
    }
    if !matches!(flags & 0xf, 4 | 8) {
        return Err(GuestTimerProfileError::InterruptTrigger {
            index,
            flags: *flags,
        });
    }
    Ok(16 + source)
}
