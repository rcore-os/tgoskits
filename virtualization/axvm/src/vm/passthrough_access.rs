//! Guest passthrough address-space ownership after a VM stops.

use alloc::format;
#[cfg(any(feature = "fs", feature = "host-fs"))]
use alloc::vec::Vec;
#[cfg(any(feature = "fs", feature = "host-fs"))]
use core::sync::atomic::AtomicUsize;
use core::sync::atomic::{AtomicU8, Ordering};

use super::AxVM;
use crate::{AxVmResult, ax_err};
#[cfg(any(feature = "fs", feature = "host-fs"))]
use crate::{
    VmStatus,
    config::VMInterruptMode,
    layout::{VmRegionKind, VmStage2Mapping},
};

/// Final host-physical interval reachable through one VM's stage-2 mappings.
#[cfg(any(feature = "fs", feature = "host-fs"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PassthroughHostRange {
    base: usize,
    length: usize,
}

#[cfg(any(feature = "fs", feature = "host-fs"))]
impl PassthroughHostRange {
    pub(crate) const fn base(self) -> usize {
        self.base
    }

    pub(crate) const fn length(self) -> usize {
        self.length
    }
}

/// Monotonic ownership state for mappings that let a guest reach host devices.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(super) enum PassthroughAccessState {
    Active   = 0,
    Revoking = 1,
    Revoked  = 2,
}

impl PassthroughAccessState {
    fn from_raw(raw: u8) -> Self {
        match raw {
            0 => Self::Active,
            1 => Self::Revoking,
            2 => Self::Revoked,
            _ => panic!("invalid VM passthrough access state {raw}"),
        }
    }
}

/// Restart-stable control word for one VM's passthrough mappings.
pub(super) struct PassthroughAccessControl {
    state: AtomicU8,
    #[cfg(any(feature = "fs", feature = "host-fs"))]
    next_mapping: AtomicUsize,
}

impl PassthroughAccessControl {
    pub(super) const fn new() -> Self {
        Self {
            state: AtomicU8::new(PassthroughAccessState::Active as u8),
            #[cfg(any(feature = "fs", feature = "host-fs"))]
            next_mapping: AtomicUsize::new(0),
        }
    }

    fn state(&self) -> PassthroughAccessState {
        PassthroughAccessState::from_raw(self.state.load(Ordering::Acquire))
    }

    #[cfg(any(feature = "fs", feature = "host-fs"))]
    fn begin_revocation(&self) -> PassthroughAccessState {
        loop {
            match self.state() {
                PassthroughAccessState::Active => {
                    if self
                        .state
                        .compare_exchange(
                            PassthroughAccessState::Active as u8,
                            PassthroughAccessState::Revoking as u8,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        self.next_mapping.store(0, Ordering::Release);
                        return PassthroughAccessState::Revoking;
                    }
                }
                state => return state,
            }
        }
    }

    #[cfg(any(feature = "fs", feature = "host-fs"))]
    fn finish_revocation(&self) {
        self.state
            .store(PassthroughAccessState::Revoked as u8, Ordering::Release);
    }
}

impl AxVM {
    pub(crate) fn ensure_passthrough_access_active(&self) -> AxVmResult {
        if self.passthrough_access.state() == PassthroughAccessState::Active {
            return Ok(());
        }
        ax_err!(
            BadState,
            format!(
                "VM[{}] passthrough access was revoked and requires a new typed ownership handoff",
                self.id()
            )
        )
    }

    #[cfg(any(feature = "fs", feature = "host-fs"))]
    pub(crate) fn uses_passthrough_access(&self) -> AxVmResult<bool> {
        self.with_resources(|resources| {
            let config = resources.config();
            Ok(!config.pass_through_devices().is_empty()
                || !config.pass_through_addresses().is_empty()
                || !config.pass_through_ports().is_empty())
        })
    }

    /// Returns whether this VM owns any host resource that participates in the
    /// retained passthrough-route lifecycle.
    ///
    /// This is deliberately broader than stage-2 memory access: x86 port
    /// forwarding and AArch64 SPI routing also need an explicit teardown owner
    /// even when no host block-controller range was selected.
    #[cfg(any(feature = "fs", feature = "host-fs"))]
    pub(crate) fn uses_passthrough_resources(&self) -> AxVmResult<bool> {
        self.with_resources(|resources| {
            let config = resources.config();
            Ok(!config.pass_through_devices().is_empty()
                || !config.pass_through_addresses().is_empty()
                || !config.pass_through_ports().is_empty()
                || !config.pass_through_spis().is_empty())
        })
    }

    #[cfg(any(feature = "fs", feature = "host-fs"))]
    pub(crate) fn passthrough_interrupt_mode(&self) -> AxVmResult<VMInterruptMode> {
        self.with_resources(|resources| Ok(resources.config().interrupt_mode()))
    }

    /// Returns final HPA ranges instead of reparsing configured device names.
    #[cfg(any(feature = "fs", feature = "host-fs"))]
    pub(crate) fn passthrough_host_ranges(&self) -> AxVmResult<Vec<PassthroughHostRange>> {
        self.with_resources(|resources| {
            let layout = resources.address_layout.as_ref().ok_or_else(|| {
                crate::ax_err_type!(
                    BadState,
                    format!("VM[{}] has no prepared passthrough layout", self.id())
                )
            })?;
            Ok(passthrough_host_ranges_from_mappings(layout.mappings()))
        })
    }

    /// Joins every stopped vCPU task before route state can be revoked.
    #[cfg(any(feature = "fs", feature = "host-fs"))]
    pub(crate) fn quiesce_for_passthrough_revocation(&self) -> AxVmResult {
        match self.status() {
            VmStatus::Ready => Ok(()),
            VmStatus::Stopped => {
                if let Some(runtime) = self.take_stopped_runtime() {
                    runtime.join_all_vcpu_tasks(self.id());
                }
                Ok(())
            }
            status => ax_err!(
                BadState,
                format!(
                    "VM[{}] cannot revoke passthrough access while {status:?}",
                    self.id()
                )
            ),
        }
    }

    /// Removes every stage-2 passthrough mapping without rebuilding the VM.
    ///
    /// Progress is retained after an address-space error. Retrying continues
    /// after the last successfully processed mapping instead of unmapping an
    /// already removed range a second time.
    #[cfg(any(feature = "fs", feature = "host-fs"))]
    pub(crate) fn revoke_passthrough_access(&self) -> AxVmResult {
        if !self.uses_passthrough_access()? {
            return Ok(());
        }
        self.quiesce_for_passthrough_revocation()?;
        if self.passthrough_access.begin_revocation() == PassthroughAccessState::Revoked {
            return Ok(());
        }

        self.with_resources_mut(|resources| {
            let mapping_count = resources
                .address_layout
                .as_ref()
                .ok_or_else(|| {
                    crate::ax_err_type!(
                        BadState,
                        format!("VM[{}] has no prepared passthrough layout", self.id())
                    )
                })?
                .mappings()
                .len();
            let mut mapping_index = self.passthrough_access.next_mapping.load(Ordering::Acquire);
            while mapping_index < mapping_count {
                let mapping = resources
                    .address_layout
                    .as_ref()
                    .expect("the VM layout is retained throughout revocation")
                    .mappings()[mapping_index];
                if mapping.kind == VmRegionKind::Passthrough {
                    resources
                        .address_space
                        .unmap(mapping.gpa, mapping.size)
                        .map_err(|error| {
                            crate::AxVmError::from_addrspace(
                                "revoke guest passthrough mapping",
                                error,
                            )
                        })?;
                }
                mapping_index += 1;
                self.passthrough_access
                    .next_mapping
                    .store(mapping_index, Ordering::Release);
            }
            Ok(())
        })?;
        self.passthrough_access.finish_revocation();
        Ok(())
    }
}

#[cfg(any(feature = "fs", feature = "host-fs"))]
fn passthrough_host_ranges_from_mappings(
    mappings: &[VmStage2Mapping],
) -> Vec<PassthroughHostRange> {
    mappings
        .iter()
        .filter(|mapping| mapping.kind == VmRegionKind::Passthrough)
        .map(|mapping| PassthroughHostRange {
            base: mapping.hpa.as_usize(),
            length: mapping.size,
        })
        .collect()
}

#[cfg(all(test, any(feature = "fs", feature = "host-fs")))]
mod tests {
    use axvm_types::{GuestPhysAddr, HostPhysAddr, MappingFlags};

    use super::*;
    use crate::layout::VmStage2Mapping;

    #[test]
    fn storage_selection_uses_final_host_mappings_not_guest_addresses() {
        let mappings = [
            VmStage2Mapping {
                gpa: GuestPhysAddr::from(0x1000),
                hpa: HostPhysAddr::from(0x9000),
                size: 0x200,
                flags: MappingFlags::READ | MappingFlags::WRITE,
                kind: VmRegionKind::Passthrough,
            },
            VmStage2Mapping {
                gpa: GuestPhysAddr::from(0x2000),
                hpa: HostPhysAddr::from(0xa000),
                size: 0x100,
                flags: MappingFlags::READ | MappingFlags::WRITE,
                kind: VmRegionKind::Memory,
            },
        ];

        assert_eq!(
            passthrough_host_ranges_from_mappings(&mappings),
            [PassthroughHostRange {
                base: 0x9000,
                length: 0x200,
            }]
        );
    }
}
