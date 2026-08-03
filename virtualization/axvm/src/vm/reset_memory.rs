//! Pristine monitor-owned guest RAM used by explicit VM reset flows.

use alloc::{boxed::Box, sync::Arc, vec::Vec};

use axvm_types::GuestPhysAddr;

use super::{AxVM, VMMemoryRegion};
#[cfg(target_arch = "aarch64")]
use crate::architecture::ArchOps;
use crate::{AxVmError, AxVmResult, ax_err, ax_err_type};

#[derive(Debug)]
struct ResetMemoryRegion {
    gpa: GuestPhysAddr,
    bytes: Box<[u8]>,
}

#[derive(Debug)]
pub(super) struct GuestMemorySnapshot {
    regions: Box<[ResetMemoryRegion]>,
    byte_len: usize,
}

impl GuestMemorySnapshot {
    fn capture(memory_regions: &[VMMemoryRegion]) -> AxVmResult<Self> {
        let mut snapshots = Vec::new();
        let mut byte_len = 0usize;

        for region in memory_regions.iter().filter(|region| region.needs_dealloc) {
            let region_size = region.size();
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(region_size)
                .map_err(|_| AxVmError::OutOfMemory {
                    operation: "capture guest reset memory",
                })?;
            // SAFETY: A monitor-owned region remains allocated for the lifetime
            // of its VM. The VM is Ready while this snapshot is captured, so no
            // vCPU can mutate the region during the copy.
            let source = unsafe {
                core::slice::from_raw_parts(region.hva.as_usize() as *const u8, region_size)
            };
            bytes.extend_from_slice(source);
            byte_len = byte_len
                .checked_add(region_size)
                .ok_or_else(|| ax_err_type!(InvalidData, "guest reset memory size overflow"))?;
            snapshots.push(ResetMemoryRegion {
                gpa: region.gpa,
                bytes: bytes.into_boxed_slice(),
            });
        }

        if snapshots.is_empty() {
            return ax_err!(
                Unsupported,
                "VM reset snapshots require monitor-owned guest memory"
            );
        }
        Ok(Self {
            regions: snapshots.into_boxed_slice(),
            byte_len,
        })
    }

    const fn byte_len(&self) -> usize {
        self.byte_len
    }

    fn restore(&self, memory_regions: &[VMMemoryRegion]) -> AxVmResult {
        let owned_regions = memory_regions
            .iter()
            .filter(|region| region.needs_dealloc)
            .collect::<Vec<_>>();
        if owned_regions.len() != self.regions.len() {
            return ax_err!(
                BadState,
                "guest memory layout changed after the reset snapshot was captured"
            );
        }

        for (region, snapshot) in owned_regions.into_iter().zip(self.regions.iter()) {
            if region.gpa != snapshot.gpa || region.size() != snapshot.bytes.len() {
                return ax_err!(
                    BadState,
                    "guest memory layout changed after the reset snapshot was captured"
                );
            }
            // SAFETY: The destination is the same live monitor-owned allocation
            // captured above, both slices have the checked identical length, and
            // every vCPU task has been joined before reset restoration begins.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    snapshot.bytes.as_ptr(),
                    region.hva.as_usize() as *mut u8,
                    snapshot.bytes.len(),
                );
            }
            crate::clean_dcache_range(region.hva, snapshot.bytes.len());
        }
        Ok(())
    }
}

impl AxVM {
    /// Captures monitor-owned guest RAM before the VM's first start.
    ///
    /// A monitor can opt into cold-reset semantics by calling this after boot
    /// images are loaded and while the VM is still [`crate::VmStatus::Ready`].
    /// Volatile device backends are intentionally not copied, so persistent
    /// guest disk state survives a reset.
    ///
    /// # Errors
    ///
    /// Returns an error if the VM is not ready, has no monitor-owned memory, a
    /// snapshot already exists, any vCPU can migrate between physical CPUs, or
    /// the snapshot allocation fails.
    pub fn capture_reset_memory(&self) -> AxVmResult<usize> {
        validate_reset_cache_affinities(&self.get_vcpu_affinities_pcpu_ids())?;
        let memory_regions = {
            let machine = self.machine.lock();
            if machine.status() != crate::VmStatus::Ready {
                return ax_err!(BadState, "guest reset memory must be captured before start");
            }
            machine
                .resources()
                .ok_or_else(|| ax_err_type!(BadState, "VM resources are not available"))?
                .memory_regions
                .clone()
        };
        let snapshot = Arc::new(GuestMemorySnapshot::capture(&memory_regions)?);
        let byte_len = snapshot.byte_len();
        let mut slot = self.reset_memory_snapshot.lock();
        if slot.is_some() {
            return ax_err!(AlreadyExists, "guest reset memory is already captured");
        }
        *slot = Some(snapshot);
        Ok(byte_len)
    }

    /// Cleans and invalidates this VM's reset-memory ranges on the current pCPU.
    ///
    /// Reset snapshots require singleton vCPU affinities, so every stopping
    /// vCPU task executes this on the only pCPU where it could have cached guest
    /// RAM. The VM does not publish `Stopped` until all such calls complete.
    #[cfg(target_arch = "aarch64")]
    pub(crate) fn quiesce_local_reset_memory_cache(&self) -> usize {
        if self.reset_memory_snapshot.lock().is_none() {
            return 0;
        }
        let memory_regions = {
            let machine = self.machine.lock();
            machine
                .resources()
                .expect("a stopping reset-enabled VM must retain its memory resources")
                .memory_regions
                .clone()
        };
        let mut byte_len = 0usize;
        for region in memory_regions.iter().filter(|region| region.needs_dealloc) {
            crate::arch::CurrentArch::clean_and_invalidate_dcache_range(region.hva, region.size());
            byte_len = byte_len.saturating_add(region.size());
        }
        byte_len
    }

    pub(super) fn restore_reset_memory(&self) -> AxVmResult {
        let Some(snapshot) = self.reset_memory_snapshot.lock().clone() else {
            return Ok(());
        };
        let memory_regions = {
            let machine = self.machine.lock();
            machine
                .resources()
                .ok_or_else(|| ax_err_type!(BadState, "VM resources are not available"))?
                .memory_regions
                .clone()
        };
        snapshot.restore(&memory_regions)
    }
}

fn validate_reset_cache_affinities(mappings: &[(usize, Option<usize>, usize)]) -> AxVmResult {
    if mappings.is_empty() {
        return ax_err!(
            BadState,
            "reset memory requires prepared vCPU affinity metadata"
        );
    }
    for (vcpu_id, affinity, _) in mappings {
        let singleton = (*affinity).is_some_and(|mask| mask.count_ones() == 1);
        if !singleton {
            return ax_err!(
                Unsupported,
                format_args!(
                    "reset memory requires singleton affinity for VCpu[{vcpu_id}], got \
                     {affinity:?}"
                )
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use core::alloc::Layout;

    use axvm_types::HostVirtAddr;

    use super::*;

    fn test_region(gpa: usize, bytes: &mut [u8]) -> VMMemoryRegion {
        VMMemoryRegion {
            gpa: GuestPhysAddr::from(gpa),
            hva: HostVirtAddr::from_mut_ptr_of(bytes.as_mut_ptr()),
            layout: Layout::from_size_align(bytes.len(), 1).unwrap(),
            needs_dealloc: true,
        }
    }

    #[test]
    fn restore_replaces_guest_mutations_with_the_captured_memory() {
        let mut first = vec![0x11, 0x22, 0x33, 0x44];
        let mut second = vec![0x55, 0x66, 0x77];
        let regions = vec![
            test_region(0x8000_0000, &mut first),
            test_region(0x9000_0000, &mut second),
        ];
        let snapshot = GuestMemorySnapshot::capture(&regions).unwrap();

        first.fill(0xaa);
        second.fill(0xbb);
        snapshot.restore(&regions).unwrap();

        assert_eq!(first, [0x11, 0x22, 0x33, 0x44]);
        assert_eq!(second, [0x55, 0x66, 0x77]);
        assert_eq!(snapshot.byte_len(), 7);
    }

    #[test]
    fn restore_rejects_a_changed_guest_memory_layout() {
        let mut original = vec![0x11, 0x22];
        let original_regions = vec![test_region(0x8000_0000, &mut original)];
        let snapshot = GuestMemorySnapshot::capture(&original_regions).unwrap();
        let mut replacement = vec![0xaa, 0xbb];
        let changed_regions = vec![test_region(0x9000_0000, &mut replacement)];

        assert!(snapshot.restore(&changed_regions).is_err());
        assert_eq!(replacement, [0xaa, 0xbb]);
    }

    #[test]
    fn reset_cache_quiesce_requires_singleton_vcpu_affinities() {
        assert!(
            validate_reset_cache_affinities(&[(0, Some(0b0010), 0), (1, Some(0b0100), 1)]).is_ok()
        );
        assert!(validate_reset_cache_affinities(&[(0, None, 0)]).is_err());
        assert!(validate_reset_cache_affinities(&[(0, Some(0), 0)]).is_err());
        assert!(validate_reset_cache_affinities(&[(0, Some(0b0110), 0)]).is_err());
    }
}
