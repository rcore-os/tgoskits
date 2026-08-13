//! Runtime allocation inside an IVC MMIO aperture reserved by the device graph.

use alloc::{sync::Arc, vec, vec::Vec};
use core::ops::Range;

use ax_memory_addr::is_aligned_4k;
use ax_sync::{RawSpinLockGuard, SpinLock as Mutex};
use axdevice_base::IrqLine;
use axvm_types::GuestPhysAddr;

use crate::*;

/// Allocates guest-physical bindings inside one graph-owned IVC aperture.
pub trait IvcApertureAllocator: Send + Sync {
    /// Reserves one page-aligned range.
    fn allocate(&self, size: usize) -> DeviceManagerResult<GuestPhysAddr>;

    /// Releases a previously reserved range.
    fn release(&self, addr: GuestPhysAddr, size: usize) -> DeviceManagerResult;
}

/// Type key for a VM's IVC aperture allocator service.
pub struct IvcApertureAllocatorKey;

impl ServiceKey for IvcApertureAllocatorKey {
    type Service = dyn IvcApertureAllocator;

    const NAME: &'static str = "ivc-aperture-allocator";
    const CARDINALITY: ServiceCardinality = ServiceCardinality::Single;
}

/// VM-local endpoint used by IVC peer notification.
pub trait IvcNotifyEndpoint: Send + Sync {
    /// Delivers one notification event to the peer VM.
    fn notify(&self) -> DeviceManagerResult;

    /// Returns the planned controller-local input for diagnostics.
    fn input(&self) -> usize;
}

/// IVC notify endpoint backed by one graph-owned wired IRQ line.
pub struct WiredIvcNotifyEndpoint {
    line: IrqLine,
}

impl WiredIvcNotifyEndpoint {
    /// Wraps a planned wired IRQ line as an IVC notify endpoint.
    pub const fn new(line: IrqLine) -> Self {
        Self { line }
    }
}

impl IvcNotifyEndpoint for WiredIvcNotifyEndpoint {
    fn notify(&self) -> DeviceManagerResult {
        self.line.pulse().map_err(DeviceManagerError::from)
    }

    fn input(&self) -> usize {
        self.line.input().value()
    }
}

/// Type key for the optional VM-local IRQ endpoint used by IVC peer notification.
pub struct IvcNotifyIrqKey;

impl ServiceKey for IvcNotifyIrqKey {
    type Service = dyn IvcNotifyEndpoint;

    const NAME: &'static str = "ivc-notify-irq";
    const CARDINALITY: ServiceCardinality = ServiceCardinality::Single;
}

/// Default allocator for an IVC MMIO aperture claimed by a [`DeviceModel`](crate::DeviceModel).
///
/// This type does not reserve guest address space. The IVC model first declares
/// and consumes a normal MMIO resource slot, then publishes this allocator in
/// the same device bundle so the resource lease owns its lifetime.
pub struct IvcAperturePool {
    ranges: Mutex<RangeAllocator>,
}

impl IvcAperturePool {
    fn ranges(&self) -> RawSpinLockGuard<'_, RangeAllocator> {
        // SAFETY: VM device-resource planning serializes same-vCPU entry; the
        // raw lock excludes concurrent resource operations on other CPUs.
        unsafe { self.ranges.lock_raw() }
    }

    /// Creates an allocator over one non-empty, page-aligned range.
    pub fn new(base: usize, length: usize) -> DeviceManagerResult<Self> {
        let end = base
            .checked_add(length)
            .ok_or_else(|| DeviceManagerError::InvalidConfig {
                operation: "create IVC aperture allocator",
                detail: "IVC aperture overflows the address space".into(),
            })?;
        if length == 0 || !is_aligned_4k(base) || !is_aligned_4k(length) {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "create IVC aperture allocator",
                detail: alloc::format!(
                    "base {base:#x} and length {length:#x} must be non-zero and 4 KiB aligned"
                ),
            });
        }
        Ok(Self {
            ranges: Mutex::new(RangeAllocator::new(base..end)),
        })
    }

    /// Converts this pool into the typed runtime service capability.
    pub fn into_service(self) -> Arc<dyn IvcApertureAllocator> {
        Arc::new(self)
    }
}

impl IvcApertureAllocator for IvcAperturePool {
    fn allocate(&self, size: usize) -> DeviceManagerResult<GuestPhysAddr> {
        validate_size(size, "allocate IVC aperture range")?;
        self.ranges()
            .allocate(size)
            .map(|range| GuestPhysAddr::from_usize(range.start))
            .ok_or(DeviceManagerError::OutOfMemory {
                operation: "allocate IVC aperture range",
            })
    }

    fn release(&self, addr: GuestPhysAddr, size: usize) -> DeviceManagerResult {
        validate_size(size, "release IVC aperture range")?;
        let end =
            addr.as_usize()
                .checked_add(size)
                .ok_or_else(|| DeviceManagerError::InvalidInput {
                    operation: "release IVC aperture range",
                    detail: "IVC aperture range end overflows the address space".into(),
                })?;
        if self.ranges().release(addr.as_usize()..end) {
            Ok(())
        } else {
            Err(DeviceManagerError::InvalidInput {
                operation: "release IVC aperture range",
                detail: alloc::format!(
                    "range {:#x}..{end:#x} is outside the pool or is not allocated",
                    addr.as_usize()
                ),
            })
        }
    }
}

fn validate_size(size: usize, operation: &'static str) -> DeviceManagerResult {
    if size == 0 || !is_aligned_4k(size) {
        Err(DeviceManagerError::InvalidInput {
            operation,
            detail: alloc::format!("size {size:#x} must be non-zero and 4 KiB aligned"),
        })
    } else {
        Ok(())
    }
}

struct RangeAllocator {
    initial: Range<usize>,
    free: Vec<Range<usize>>,
}

impl RangeAllocator {
    fn new(range: Range<usize>) -> Self {
        Self {
            initial: range.clone(),
            free: vec![range],
        }
    }

    fn allocate(&mut self, size: usize) -> Option<Range<usize>> {
        let index = self
            .free
            .iter()
            .enumerate()
            .filter(|(_, range)| range.end - range.start >= size)
            .min_by_key(|(_, range)| range.end - range.start)
            .map(|(index, _)| index)?;
        let start = self.free[index].start;
        let end = start + size;
        if self.free[index].end == end {
            self.free.remove(index);
        } else {
            self.free[index].start = end;
        }
        Some(start..end)
    }

    fn release(&mut self, range: Range<usize>) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ivc_aperture_reuses_range_after_release() {
        let pool = IvcAperturePool::new(0x1000_0000, 0x2000).unwrap();

        let first = pool.allocate(0x1000).unwrap();
        let second = pool.allocate(0x1000).unwrap();
        assert_ne!(first, second);

        pool.release(first, 0x1000).unwrap();
        let reused = pool.allocate(0x1000).unwrap();

        assert_eq!(reused, first);
    }
}
