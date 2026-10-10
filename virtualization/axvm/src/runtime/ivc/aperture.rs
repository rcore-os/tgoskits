//! Guest-aperture resources used by the manager-owned IVC table.

use std::{
    format,
    sync::{Arc, Mutex, MutexGuard},
    vec,
    vec::Vec,
};

use ax_memory_addr::is_aligned_4k;
use axdevice::{DeviceManagerError, DeviceManagerResult, ServiceCardinality, ServiceKey};
use axdevice_base::IrqLine;
use axvm_types::{GuestPhysAddr, MappingFlags};

/// Allocates guest-physical bindings inside one graph-owned IVC MMIO aperture.
pub(crate) trait IvcApertureAllocator: Send + Sync {
    fn allocate(&self, size: usize) -> DeviceManagerResult<GuestPhysAddr>;

    fn release(&self, addr: GuestPhysAddr, size: usize) -> DeviceManagerResult;
}

/// Type key for a VM's IVC aperture allocator service.
pub(crate) struct IvcApertureAllocatorKey;

impl ServiceKey for IvcApertureAllocatorKey {
    type Service = dyn IvcApertureAllocator;

    const NAME: &'static str = "ivc-aperture-allocator";
    const CARDINALITY: ServiceCardinality = ServiceCardinality::Single;
}

/// A VM-local endpoint used by IVC peer notification.
pub(crate) trait IvcNotifyEndpoint: Send + Sync {
    fn notify(&self) -> DeviceManagerResult;
}

/// An IVC notify endpoint backed by one graph-owned wired IRQ line.
pub(crate) struct WiredIvcNotifyEndpoint {
    line: IrqLine,
}

impl WiredIvcNotifyEndpoint {
    /// Wraps a planned wired IRQ line as an IVC notify endpoint.
    pub(crate) const fn new(line: IrqLine) -> Self {
        Self { line }
    }
}

impl IvcNotifyEndpoint for WiredIvcNotifyEndpoint {
    fn notify(&self) -> DeviceManagerResult {
        self.line.pulse().map_err(DeviceManagerError::from)
    }
}

/// Type key for the optional VM-local IRQ endpoint used by IVC notification.
pub(crate) struct IvcNotifyEndpointKey;

impl ServiceKey for IvcNotifyEndpointKey {
    type Service = dyn IvcNotifyEndpoint;

    const NAME: &'static str = "ivc-notify-endpoint";
    const CARDINALITY: ServiceCardinality = ServiceCardinality::Single;
}

/// The default allocator for an IVC MMIO aperture claimed by an IVC device model.
pub(crate) struct IvcAperturePool {
    ranges: Mutex<RangeAllocator>,
}

impl IvcAperturePool {
    fn ranges(&self) -> MutexGuard<'_, RangeAllocator> {
        crate::sync::MutexExt::lock_unpoisoned(&self.ranges)
    }

    /// Creates an allocator over one non-empty, page-aligned range.
    pub(crate) fn new(base: usize, length: usize) -> DeviceManagerResult<Self> {
        let end = base
            .checked_add(length)
            .ok_or_else(|| DeviceManagerError::InvalidInput {
                operation: "create IVC aperture pool",
                detail: format!("range {base:#x}+{length:#x} overflows"),
            })?;
        if base == 0 || length == 0 || !is_aligned_4k(base) || !is_aligned_4k(length) {
            return Err(DeviceManagerError::InvalidInput {
                operation: "create IVC aperture pool",
                detail: format!("range {base:#x}..{end:#x} is empty or not 4 KiB aligned"),
            });
        }

        Ok(Self {
            ranges: Mutex::new(RangeAllocator::new(base..end)),
        })
    }

    /// Publishes the pool as the VM-local allocator service.
    pub(crate) fn into_service(self) -> Arc<dyn IvcApertureAllocator> {
        Arc::new(self)
    }
}

impl IvcApertureAllocator for IvcAperturePool {
    fn allocate(&self, size: usize) -> DeviceManagerResult<GuestPhysAddr> {
        validate_aperture_size(size, "allocate IVC aperture range")?;
        let range = self
            .ranges()
            .allocate(size)
            .ok_or(DeviceManagerError::OutOfMemory {
                operation: "allocate IVC aperture range",
            })?;
        Ok(GuestPhysAddr::from_usize(range.start))
    }

    fn release(&self, addr: GuestPhysAddr, size: usize) -> DeviceManagerResult {
        validate_aperture_size(size, "release IVC aperture range")?;
        let end =
            addr.as_usize()
                .checked_add(size)
                .ok_or_else(|| DeviceManagerError::InvalidInput {
                    operation: "release IVC aperture range",
                    detail: format!("range {:#x}+{size:#x} overflows", addr.as_usize()),
                })?;
        if self.ranges().release(addr.as_usize()..end) {
            Ok(())
        } else {
            Err(DeviceManagerError::InvalidInput {
                operation: "release IVC aperture range",
                detail: format!(
                    "range {:#x}..{end:#x} is outside the pool or is not allocated",
                    addr.as_usize()
                ),
            })
        }
    }
}

pub(super) fn validate_aperture_size(size: usize, operation: &'static str) -> DeviceManagerResult {
    if size == 0 || !is_aligned_4k(size) {
        Err(DeviceManagerError::InvalidInput {
            operation,
            detail: format!("size {size:#x} must be non-zero and 4 KiB aligned"),
        })
    } else {
        Ok(())
    }
}

#[derive(Clone)]
struct RangeAllocator {
    initial: core::ops::Range<usize>,
    free: Vec<core::ops::Range<usize>>,
}

impl RangeAllocator {
    fn new(range: core::ops::Range<usize>) -> Self {
        Self {
            initial: range.clone(),
            free: vec![range],
        }
    }

    fn allocate(&mut self, size: usize) -> Option<core::ops::Range<usize>> {
        let index = self
            .free
            .iter()
            .enumerate()
            .filter(|(_, range)| range.end - range.start >= size)
            .min_by_key(|(_, range)| range.end - range.start)
            .map(|(index, _)| index)?;
        let start = self.free[index].start;
        let end = start.checked_add(size)?;
        if self.free[index].end == end {
            self.free.remove(index);
        } else {
            self.free[index].start = end;
        }
        Some(start..end)
    }

    fn release(&mut self, range: core::ops::Range<usize>) -> bool {
        if range.start >= range.end
            || range.start < self.initial.start
            || range.end > self.initial.end
        {
            return false;
        }

        let index = self
            .free
            .iter()
            .position(|free| free.start > range.start)
            .unwrap_or(self.free.len());
        if index > 0 && self.free[index - 1].end > range.start
            || index < self.free.len() && range.end > self.free[index].start
        {
            return false;
        }

        if index > 0 && self.free[index - 1].end == range.start {
            self.free[index - 1].end = range.end;
            if index < self.free.len() && self.free[index - 1].end == self.free[index].start {
                let next = self.free.remove(index);
                self.free[index - 1].end = next.end;
            }
        } else if index < self.free.len() && range.end == self.free[index].start {
            self.free[index].start = range.start;
        } else {
            self.free.insert(index, range);
        }
        true
    }
}

/// Stage-2 attributes for AXIVC shared pages.
///
/// The shared region is CPU-owned Normal Write-Back memory. Guests mapping the
/// same physical pages must use compatible WB attributes; DEVICE and uncached
/// aliases are not part of the AXIVC contract.
pub(crate) fn shared_memory_mapping_flags() -> MappingFlags {
    MappingFlags::READ | MappingFlags::WRITE
}
