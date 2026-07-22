//! VM-local state for a virtual platform-level interrupt controller.

use alloc::{vec, vec::Vec};

use ax_kspin::SpinNoIrq as Mutex;
use axvm_types::GuestPhysAddr;
use bitmaps::Bitmap;

use crate::{VplicError, VplicResult, consts::*};

pub(super) struct ContextState {
    pub(super) enabled: Bitmap<{ PLIC_NUM_SOURCES }>,
    pub(super) threshold: u32,
}

pub(super) struct VplicState {
    pub(super) assignment: SourceAssignment,
    pub(super) pending: Bitmap<{ PLIC_NUM_SOURCES }>,
    pub(super) active: Bitmap<{ PLIC_NUM_SOURCES }>,
    pub(super) source_levels: Bitmap<{ PLIC_NUM_SOURCES }>,
    pub(super) priorities: Vec<u32>,
    pub(super) contexts: Vec<ContextState>,
}

pub(super) enum SourceAssignment {
    Unrestricted,
    Restricted(Bitmap<{ PLIC_NUM_SOURCES }>),
}

/// One VM's PLIC register and delivery state.
///
/// The controller never dereferences a host PLIC aperture. Physical IRQ
/// ownership and host claim/complete remain responsibilities of the host IRQ
/// adapter, which signals this controller through its typed input capability.
pub struct VPlicGlobal {
    addr: GuestPhysAddr,
    size: usize,
    contexts_num: usize,
    pub(super) state: Mutex<VplicState>,
}

impl VPlicGlobal {
    /// Creates a VM-local virtual PLIC.
    ///
    /// # Errors
    ///
    /// Returns an error when the MMIO range overflows or cannot contain every
    /// configured context.
    pub fn new(addr: GuestPhysAddr, size: Option<usize>, contexts_num: usize) -> VplicResult<Self> {
        let base = addr.as_usize();
        let required_end = contexts_num
            .checked_mul(PLIC_CONTEXT_STRIDE)
            .and_then(|offset| offset.checked_add(PLIC_CONTEXT_CTRL_OFFSET))
            .and_then(|offset| offset.checked_add(PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET))
            .and_then(|offset| base.checked_add(offset))
            .ok_or(VplicError::AddressOverflow)?;
        let size = size.ok_or(VplicError::MissingRegionSize)?;
        let region_end = base.checked_add(size).ok_or(VplicError::AddressOverflow)?;
        if region_end <= required_end {
            return Err(VplicError::InsufficientRegion {
                base,
                region_end,
                required_end,
            });
        }
        let contexts = (0..contexts_num)
            .map(|_| ContextState {
                enabled: Bitmap::new(),
                threshold: 0,
            })
            .collect();
        Ok(Self {
            addr,
            size,
            contexts_num,
            state: Mutex::new(VplicState {
                assignment: SourceAssignment::Unrestricted,
                pending: Bitmap::new(),
                active: Bitmap::new(),
                source_levels: Bitmap::new(),
                priorities: vec![0; PLIC_NUM_SOURCES],
                contexts,
            }),
        })
    }

    /// Returns the guest MMIO base.
    pub const fn address(&self) -> GuestPhysAddr {
        self.addr
    }

    /// Returns the guest MMIO aperture size.
    pub const fn size(&self) -> usize {
        self.size
    }

    /// Returns the number of PLIC contexts.
    pub const fn context_count(&self) -> usize {
        self.contexts_num
    }

    /// Restricts the controller to an explicitly assigned source.
    ///
    /// Calling this method switches legacy unrestricted controllers into an
    /// explicit assignment policy. Other sources are RAZ/WI and cannot be
    /// signaled through the controller input API.
    pub fn assign_source(&self, source_id: usize) -> VplicResult {
        Self::validate_source_id(source_id)?;
        let mut state = self.state.lock();
        match &mut state.assignment {
            SourceAssignment::Unrestricted => {
                let mut assigned = Bitmap::new();
                assigned.set(source_id, true);
                state.assignment = SourceAssignment::Restricted(assigned);
            }
            SourceAssignment::Restricted(assigned) => {
                assigned.set(source_id, true);
            }
        }
        Ok(())
    }

    /// Enforces an explicit source ownership set, even when no source has yet
    /// been assigned.
    pub fn restrict_to_assigned_sources(&self) {
        let mut state = self.state.lock();
        if matches!(state.assignment, SourceAssignment::Unrestricted) {
            state.assignment = SourceAssignment::Restricted(Bitmap::new());
        }
    }

    /// Returns whether no explicit source assignment has been installed.
    pub fn has_unrestricted_sources(&self) -> bool {
        matches!(self.state.lock().assignment, SourceAssignment::Unrestricted)
    }

    /// Returns whether a source is currently active in a guest context.
    pub fn is_active(&self, source_id: usize) -> VplicResult<bool> {
        Self::validate_source_id(source_id)?;
        let state = self.state.lock();
        Self::validate_assigned_source(&state, source_id)?;
        Ok(state.active.get(source_id))
    }

    pub(super) fn validate_source_id(source_id: usize) -> VplicResult {
        if source_id == 0 || source_id >= PLIC_NUM_SOURCES {
            return Err(VplicError::InvalidSource {
                source_id,
                max: PLIC_NUM_SOURCES,
            });
        }
        Ok(())
    }

    pub(super) fn source_is_assigned(state: &VplicState, source_id: usize) -> bool {
        match &state.assignment {
            SourceAssignment::Unrestricted => true,
            SourceAssignment::Restricted(assigned) => assigned.get(source_id),
        }
    }

    pub(super) fn validate_assigned_source(state: &VplicState, source_id: usize) -> VplicResult {
        Self::validate_source_id(source_id)?;
        if !Self::source_is_assigned(state, source_id) {
            return Err(VplicError::SourceNotAssigned { source_id });
        }
        Ok(())
    }
}
