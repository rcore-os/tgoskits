//! Saved GICv3 virtual CPU-interface state.

use crate::{IntId, InterruptState, PhysicalIrqId, Priority, TriggerMode, VgicError};

pub(crate) const MAX_LIST_REGISTERS: usize = 16;

/// Inline capacity of the GICv2 active priority stack.
///
/// Preemption requires a strict drop in group priority at every nesting level,
/// and a group priority is derived from the 8-bit priority field, so no legal
/// guest can hold more than 256 active priority layers. Keeping the stack
/// inline makes `CpuInterfaceState::clone`, and therefore the CPU-interface
/// snapshot on the load/save path, allocation-free.
const MAX_V2_ACTIVE_DEPTH: usize = 256;

/// Allocation-free failure of one list-register state check.
///
/// The CPU-pinned load/save merge runs while the canonical raw lock is held,
/// so it reports the offending index, INTID, and operation instead of
/// formatting a diagnostic there. Callers format this value after they release
/// the guard.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ListRegisterFailure {
    /// The list-register index is outside the configured CPU interface.
    IndexOutOfRange {
        /// Out-of-range list-register index.
        index: usize,
        /// Interrupt being synchronized.
        intid: IntId,
        /// Operation that found the index.
        operation: &'static str,
    },
    /// The list register no longer holds the expected INTID.
    IntIdMismatch {
        /// List-register index.
        index: usize,
        /// Interrupt being synchronized.
        intid: IntId,
        /// Operation that found the mismatch.
        operation: &'static str,
    },
    /// The list register's backing changed while it was loaded.
    BackingChanged {
        /// Interrupt whose backing changed.
        intid: IntId,
        /// Backing observed in canonical state.
        from: ListRegisterBacking,
        /// Backing observed from the hardware save.
        to: ListRegisterBacking,
        /// Operation that found the change.
        operation: &'static str,
    },
}

impl ListRegisterFailure {
    /// Converts this failure into the public typed error without allocating.
    ///
    /// The conversion runs on the CPU-pinned merge path, so both the failure
    /// and the resulting [`VgicError`] carry only `Copy` facts.
    pub(crate) fn into_vgic_error(self) -> VgicError {
        match self {
            Self::IndexOutOfRange {
                index,
                intid,
                operation,
            } => VgicError::NativeState {
                operation,
                vcpu: None,
                intid: Some(intid),
                reason: "the list-register index is out of range",
                kind: crate::StateErrorKind::InvalidInput,
                detail: crate::NativeStateDetail::ListRegister(index),
            },
            Self::IntIdMismatch {
                index,
                intid,
                operation,
            } => VgicError::NativeState {
                operation,
                vcpu: None,
                intid: Some(intid),
                reason: "the list register no longer holds the expected INTID",
                kind: crate::StateErrorKind::InvalidState,
                detail: crate::NativeStateDetail::ListRegister(index),
            },
            Self::BackingChanged {
                intid,
                from,
                to,
                operation,
            } => VgicError::NativeState {
                operation,
                vcpu: None,
                intid: Some(intid),
                reason: "the list-register backing changed while it was loaded",
                kind: crate::StateErrorKind::InvalidState,
                detail: crate::NativeStateDetail::BackingMismatch {
                    owned: from,
                    observed: to,
                },
            },
        }
    }
}

const ICH_HCR_ENABLE: u64 = 1;
const ICH_HCR_UIE: u64 = 1 << 1;
const ICH_HCR_LRENPIE: u64 = 1 << 2;
const ICH_HCR_NPIE: u64 = 1 << 3;
const ICH_HCR_TDIR: u64 = 1 << 14;
const ICH_HCR_EOI_COUNT_SHIFT: u32 = 27;
const ICH_HCR_EOI_COUNT_MASK: u64 = 0x1f << ICH_HCR_EOI_COUNT_SHIFT;
const ICH_VMCR_VENG1: u64 = 1 << 1;
const ICH_VMCR_VEOIM: u64 = 1 << 9;
const ICH_VMCR_VPMR_SHIFT: u32 = 24;
const ICH_VMCR_VPMR_MASK: u64 = 0xff << ICH_VMCR_VPMR_SHIFT;

/// Source backing used for one virtual list-register delivery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListRegisterBacking {
    /// The hypervisor owns the complete virtual interrupt lifecycle.
    Software,
    /// The physical GIC owns pending/active state and the LR names its source.
    ///
    /// Guest deactivation can consequently retire the physical activation in
    /// hardware. A trapped DIR still uses this identity to complete the exact
    /// ownership-checked host source.
    Physical(PhysicalIrqId),
}

/// One virtual interrupt represented in an ICH list register.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ListRegisterState {
    intid: IntId,
    priority: Priority,
    state: InterruptState,
    backing: ListRegisterBacking,
    maintenance_on_eoi: bool,
}

impl ListRegisterState {
    /// Creates a list-register entry.
    pub const fn new(intid: IntId, priority: Priority, state: InterruptState) -> Self {
        Self {
            intid,
            priority,
            state,
            backing: ListRegisterBacking::Software,
            maintenance_on_eoi: false,
        }
    }

    /// Creates a software-backed entry with trigger-aware EOI maintenance.
    pub const fn new_software(
        intid: IntId,
        priority: Priority,
        state: InterruptState,
        trigger: TriggerMode,
    ) -> Self {
        Self::new_software_with_maintenance(
            intid,
            priority,
            state,
            matches!(trigger, TriggerMode::Level),
        )
    }

    pub(crate) const fn new_software_with_maintenance(
        intid: IntId,
        priority: Priority,
        state: InterruptState,
        maintenance_on_eoi: bool,
    ) -> Self {
        Self {
            intid,
            priority,
            state,
            backing: ListRegisterBacking::Software,
            maintenance_on_eoi,
        }
    }

    /// Creates a hardware-backed entry for one ownership-checked physical interrupt.
    pub const fn new_physical(
        intid: IntId,
        priority: Priority,
        state: InterruptState,
        physical: PhysicalIrqId,
    ) -> Self {
        Self {
            intid,
            priority,
            state,
            backing: ListRegisterBacking::Physical(physical),
            maintenance_on_eoi: false,
        }
    }

    /// Returns the represented INTID.
    pub const fn intid(self) -> IntId {
        self.intid
    }

    /// Returns the virtual priority.
    pub const fn priority(self) -> Priority {
        self.priority
    }

    /// Returns the saved delivery state.
    pub const fn state(self) -> InterruptState {
        self.state
    }

    /// Returns whether delivery state is software-owned or physical-GIC-backed.
    pub const fn backing(self) -> ListRegisterBacking {
        self.backing
    }

    /// Returns whether guest deactivation must raise a maintenance interrupt.
    pub const fn maintenance_on_eoi(self) -> bool {
        self.maintenance_on_eoi
    }

    /// Updates the saved delivery state.
    pub fn set_state(&mut self, state: InterruptState) {
        self.state = state;
    }
}

/// Complete ICH state saved for one vCPU.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CpuInterfaceState {
    hcr: u64,
    vmcr: u64,
    apr: [u64; 4],
    list_registers: [Option<ListRegisterState>; MAX_LIST_REGISTERS],
    withdrawn_pending: [Option<IntId>; MAX_LIST_REGISTERS],
    list_register_count: usize,
    v2_enabled: bool,
    v2_priority_mask: Priority,
    v2_binary_point: u8,
    v2_eoi_mode: bool,
    /// Active GICv2 deliveries, innermost (highest group priority) last.
    v2_active_stack: [Option<(IntId, Priority)>; MAX_V2_ACTIVE_DEPTH],
    /// Occupied entries at the front of [`Self::v2_active_stack`].
    v2_active_depth: usize,
}

impl CpuInterfaceState {
    pub(crate) fn new(list_register_count: usize) -> Self {
        assert!((1..=MAX_LIST_REGISTERS).contains(&list_register_count));
        Self {
            hcr: 1,
            vmcr: ICH_VMCR_VENG1 | ICH_VMCR_VPMR_MASK,
            apr: [0; 4],
            list_registers: [None; MAX_LIST_REGISTERS],
            withdrawn_pending: [None; MAX_LIST_REGISTERS],
            list_register_count,
            v2_enabled: false,
            v2_priority_mask: Priority::new(0),
            v2_binary_point: 0,
            v2_eoi_mode: false,
            v2_active_stack: [None; MAX_V2_ACTIVE_DEPTH],
            v2_active_depth: 0,
        }
    }

    /// Returns ICH_HCR_EL2 state.
    pub const fn hcr(&self) -> u64 {
        self.hcr
    }

    /// Updates ICH_HCR_EL2 state.
    pub fn set_hcr(&mut self, value: u64) {
        self.hcr = value;
    }

    pub(crate) fn take_eoi_count(&mut self) -> usize {
        let count = ((self.hcr & ICH_HCR_EOI_COUNT_MASK) >> ICH_HCR_EOI_COUNT_SHIFT) as usize;
        self.hcr &= !ICH_HCR_EOI_COUNT_MASK;
        count
    }

    pub(crate) fn configure_delivery_traps(
        &mut self,
        pending_outside_lrs: bool,
        active_outside_lrs: bool,
        trap_deactivation: bool,
    ) {
        let managed =
            ICH_HCR_UIE | ICH_HCR_LRENPIE | ICH_HCR_NPIE | ICH_HCR_TDIR | ICH_HCR_EOI_COUNT_MASK;
        let mut hcr = (self.hcr & !managed) | ICH_HCR_ENABLE;
        if pending_outside_lrs || active_outside_lrs {
            hcr |= ICH_HCR_UIE;
        }
        if active_outside_lrs {
            hcr |= ICH_HCR_LRENPIE;
        }
        if pending_outside_lrs {
            hcr |= ICH_HCR_NPIE;
        }
        if trap_deactivation {
            hcr |= ICH_HCR_TDIR;
        }
        self.hcr = hcr;
    }

    /// Returns ICH_VMCR_EL2 state.
    pub const fn vmcr(&self) -> u64 {
        self.vmcr
    }

    /// Updates ICH_VMCR_EL2 state.
    pub fn set_vmcr(&mut self, value: u64) {
        self.vmcr = value;
    }

    /// Returns the guest-visible common ICC control bits.
    pub const fn icc_control(&self) -> u64 {
        ((self.vmcr & ICH_VMCR_VEOIM != 0) as u64) << 1
    }

    /// Updates writable common ICC control bits.
    pub fn set_icc_control(&mut self, value: u64) {
        if value & (1 << 1) != 0 {
            self.vmcr |= ICH_VMCR_VEOIM;
        } else {
            self.vmcr &= !ICH_VMCR_VEOIM;
        }
    }

    /// Returns the guest-visible virtual priority mask.
    pub const fn icc_priority_mask(&self) -> u8 {
        ((self.vmcr & ICH_VMCR_VPMR_MASK) >> ICH_VMCR_VPMR_SHIFT) as u8
    }

    /// Updates the guest-visible virtual priority mask.
    pub fn set_icc_priority_mask(&mut self, value: u8) {
        self.vmcr = (self.vmcr & !ICH_VMCR_VPMR_MASK) | (u64::from(value) << ICH_VMCR_VPMR_SHIFT);
    }

    /// Returns the priority of the highest-priority active LR.
    pub fn icc_running_priority(&self) -> Priority {
        self.list_registers
            .iter()
            .flatten()
            .filter(|entry| {
                matches!(
                    entry.state(),
                    InterruptState::Active | InterruptState::ActivePending
                )
            })
            .map(|entry| entry.priority())
            .min()
            .unwrap_or_else(|| Priority::new(0xff))
    }

    /// Returns saved active-priority registers.
    pub const fn apr(&self) -> &[u64; 4] {
        &self.apr
    }

    /// Updates one active-priority register.
    pub fn set_apr(&mut self, index: usize, value: u64) -> bool {
        if let Some(register) = self.apr.get_mut(index) {
            *register = value;
            true
        } else {
            false
        }
    }

    /// Returns all list-register slots.
    pub fn list_registers(&self) -> &[Option<ListRegisterState>] {
        &self.list_registers[..self.list_register_count]
    }

    /// Returns mutable list-register slots for a checked backend save.
    pub fn list_registers_mut(&mut self) -> &mut [Option<ListRegisterState>] {
        &mut self.list_registers[..self.list_register_count]
    }

    /// Returns the span containing all occupied LRs, including any holes.
    ///
    /// Backends can use this bound when transferring hardware LR state. A
    /// guest can complete an earlier LR while a higher-numbered one remains
    /// live, so counting occupied entries would lose that higher slot.
    pub fn used_list_registers(&self) -> usize {
        self.list_registers()
            .iter()
            .rposition(Option::is_some)
            .map_or(0, |index| index + 1)
    }

    /// Defers withdrawal of a loaded LR until the backend has read hardware.
    ///
    /// The slot retains its identity so a guest activation racing with input
    /// deassertion can still be harvested. Its pending state is hidden from
    /// software queries until the hardware observation is reconciled.
    pub(crate) fn withdraw_pending_delivery(&mut self, intid: IntId, loaded: bool) -> bool {
        let mut canceled = false;
        for (index, slot) in self.list_registers[..self.list_register_count]
            .iter_mut()
            .enumerate()
        {
            let Some(entry) = slot.as_mut().filter(|entry| entry.intid() == intid) else {
                continue;
            };
            match entry.state() {
                InterruptState::Pending if loaded => {
                    entry.set_state(InterruptState::Inactive);
                    self.withdrawn_pending[index] = Some(intid);
                }
                InterruptState::Pending => {
                    *slot = None;
                    canceled = true;
                }
                InterruptState::ActivePending if loaded => {
                    entry.set_state(InterruptState::Active);
                    self.withdrawn_pending[index] = Some(intid);
                }
                InterruptState::ActivePending => entry.set_state(InterruptState::Active),
                InterruptState::Inactive | InterruptState::Active => {}
            }
        }
        canceled
    }

    pub(crate) fn clear_pending_withdrawal(&mut self, index: usize) {
        self.withdrawn_pending[index] = None;
    }

    /// Applies the latest canonical withdrawals to a hardware observation.
    ///
    /// The input can change after the caller takes its save snapshot, so the
    /// owner must call this while holding the controller state lock.
    pub(crate) fn reconcile_withdrawn_pending(
        &self,
        observed: &mut CpuInterfaceState,
    ) -> Result<(), ListRegisterFailure> {
        for (index, withdrawal) in self.withdrawn_pending.iter().enumerate() {
            let Some(intid) = *withdrawal else {
                continue;
            };
            let slot = &mut observed.list_registers[index];
            if let Some(entry) = slot {
                if entry.intid() != intid {
                    return Err(ListRegisterFailure::IntIdMismatch {
                        index,
                        intid,
                        operation: "reconcile withdrawn CPU-interface delivery",
                    });
                }
                if entry.backing() != ListRegisterBacking::Software {
                    return Err(ListRegisterFailure::BackingChanged {
                        intid,
                        operation: "reconcile withdrawn CPU-interface delivery",
                        from: ListRegisterBacking::Software,
                        to: entry.backing(),
                    });
                }
                match entry.state() {
                    InterruptState::Pending => *slot = None,
                    InterruptState::ActivePending => entry.set_state(InterruptState::Active),
                    InterruptState::Inactive | InterruptState::Active => {}
                }
            }
        }
        observed.withdrawn_pending = [None; MAX_LIST_REGISTERS];
        Ok(())
    }

    /// Returns the guest-visible GICC_CTLR state.
    pub fn v2_control(&self) -> u32 {
        self.v2_enabled as u32 | ((self.v2_eoi_mode as u32) << 9)
    }

    pub(crate) fn set_v2_control(&mut self, value: u32) {
        self.v2_enabled = value & 1 != 0;
        self.v2_eoi_mode = value & (1 << 9) != 0;
    }

    /// Returns whether the GICv2 virtual CPU interface is enabled.
    pub const fn v2_enabled(&self) -> bool {
        self.v2_enabled
    }

    /// Returns the GICv2 virtual priority mask.
    pub const fn v2_priority_mask(&self) -> Priority {
        self.v2_priority_mask
    }

    pub(crate) fn set_v2_priority_mask(&mut self, value: u8) {
        self.v2_priority_mask = Priority::new(value);
    }

    /// Returns the GICv2 virtual binary point.
    pub const fn v2_binary_point(&self) -> u8 {
        self.v2_binary_point
    }

    pub(crate) fn set_v2_binary_point(&mut self, value: u8) {
        self.v2_binary_point = value & 0x7;
    }

    /// Returns whether split EOI/deactivation mode is enabled.
    pub const fn v2_eoi_mode(&self) -> bool {
        self.v2_eoi_mode
    }

    /// Whether the inline active priority stack cannot accept another layer.
    pub(crate) const fn v2_active_is_full(&self) -> bool {
        self.v2_active_depth >= MAX_V2_ACTIVE_DEPTH
    }

    /// Whether a candidate may preempt the running GICv2 delivery.
    ///
    /// An idle interface always admits the first delivery. Once a delivery is
    /// active, the candidate must have a strictly higher group priority than
    /// the running one: an equal or lower group priority stays pending, so a
    /// subpriority difference never preempts an active delivery. The binary
    /// point decides how many low priority bits are subpriority rather than
    /// part of the group priority.
    pub(crate) fn v2_preempts_running(&self, priority: Priority) -> bool {
        if self.v2_active_depth == 0 {
            return true;
        }
        let running = self.v2_active_stack[self.v2_active_depth - 1]
            .map_or(Priority::new(0xff), |(_, priority)| priority);
        v2_group_priority(priority, self.v2_binary_point) < running
    }

    /// Pushes one active GICv2 delivery onto the inline stack.
    ///
    /// The caller must have checked [`Self::v2_active_is_full`] before it
    /// changed canonical delivery state. Both calls run under the same raw
    /// guard, so no competing acknowledgement can exhaust that reserved slot.
    pub(crate) fn push_v2_active(&mut self, intid: IntId, priority: Priority) {
        self.v2_active_stack[self.v2_active_depth] =
            Some((intid, v2_group_priority(priority, self.v2_binary_point)));
        self.v2_active_depth += 1;
    }

    /// Pops the running priority when it belongs to `intid`.
    ///
    /// Only the innermost delivery retires, so a mismatched EOI/DIR leaves the
    /// stack untouched and preserves the split EOI/deactivation contract.
    pub(crate) fn drop_v2_priority(&mut self, intid: IntId) -> bool {
        if self.v2_active_depth == 0 {
            return false;
        }
        let top = self.v2_active_stack[self.v2_active_depth - 1];
        if top.is_none_or(|(active, _)| active != intid) {
            return false;
        }
        self.v2_active_depth -= 1;
        self.v2_active_stack[self.v2_active_depth] = None;
        true
    }

    /// Returns the group priority recorded when the top interrupt was activated.
    pub fn v2_running_priority(&self) -> Priority {
        if self.v2_active_depth == 0 {
            return Priority::new(0xff);
        }
        self.v2_active_stack[self.v2_active_depth - 1]
            .map_or(Priority::new(0xff), |(_, priority)| priority)
    }
}

/// Splits one 8-bit priority into the GICv2 group-priority field.
///
/// `GICC_BPR.BinaryPoint` is the highest subpriority bit. BPR=0 keeps
/// group-priority bits [7:1]; BPR=7 leaves no group-priority bits.
fn v2_group_priority(priority: Priority, binary_point: u8) -> Priority {
    let group_mask = 0xffu16 << ((binary_point & 0x7) + 1);
    Priority::new(priority.raw() & group_mask as u8)
}

#[cfg(test)]
mod tests {
    use super::{CpuInterfaceState, ListRegisterState};
    use crate::{IntId, InterruptState, PpiId, Priority, TriggerMode};

    #[test]
    fn only_software_level_delivery_requests_eoi_maintenance() {
        let intid = IntId::Ppi(PpiId::new(27).unwrap());
        let level = ListRegisterState::new_software(
            intid,
            Priority::DEFAULT,
            InterruptState::Pending,
            TriggerMode::Level,
        );
        let edge = ListRegisterState::new_software(
            intid,
            Priority::DEFAULT,
            InterruptState::Pending,
            TriggerMode::Edge,
        );

        assert!(level.maintenance_on_eoi());
        assert!(!edge.maintenance_on_eoi());
    }

    #[test]
    fn used_lr_span_includes_empty_slots_before_the_last_delivery() {
        let mut state = CpuInterfaceState::new(4);
        let intid = IntId::Ppi(PpiId::new(27).unwrap());
        assert_eq!(state.used_list_registers(), 0);

        state.list_registers_mut()[3] = Some(ListRegisterState::new(
            intid,
            Priority::DEFAULT,
            InterruptState::Active,
        ));
        assert_eq!(state.used_list_registers(), 4);

        state.list_registers_mut()[0] = Some(ListRegisterState::new(
            intid,
            Priority::DEFAULT,
            InterruptState::Pending,
        ));
        state.list_registers_mut()[0] = None;
        assert_eq!(state.used_list_registers(), 4);

        state.list_registers_mut()[3] = None;
        assert_eq!(state.used_list_registers(), 0);
        assert_eq!(state.list_registers().len(), 4);
    }
}
