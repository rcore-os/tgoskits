//! Runtime allocation inside a guest range reserved by the device graph.

use alloc::{sync::Arc, vec, vec::Vec};
use core::ops::Range;

use ax_memory_addr::is_aligned_4k;
use ax_sync::{RawSpinLockGuard, SpinLock as Mutex};
use axvm_types::GuestPhysAddr;

use crate::*;

/// Allocates guest-physical ranges from one graph-owned window.
pub trait GuestRangeAllocator: Send + Sync {
    /// Reserves one page-aligned guest range.
    fn allocate(&self, size: usize) -> DeviceManagerResult<GuestPhysAddr>;

    /// Releases a previously reserved guest range.
    fn release(&self, addr: GuestPhysAddr, size: usize) -> DeviceManagerResult;
}

/// Type key for a VM's guest-range allocator service.
pub struct GuestRangeAllocatorKey;

impl ServiceKey for GuestRangeAllocatorKey {
    type Service = dyn GuestRangeAllocator;

    const NAME: &'static str = "guest-range-allocator";
    const CARDINALITY: ServiceCardinality = ServiceCardinality::Single;
}

/// Default allocator for a range already claimed by a [`DeviceModel`](crate::DeviceModel).
///
/// This type does not reserve guest address space. A model must first declare
/// and consume the corresponding resource slot, then publish this allocator in
/// the same device bundle so the resource lease owns its lifetime.
pub struct GuestRangePool {
    ranges: Mutex<RangeAllocator>,
}

impl GuestRangePool {
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
                operation: "create guest range allocator",
                detail: "reserved guest range overflows the address space".into(),
            })?;
        if length == 0 || !is_aligned_4k(base) || !is_aligned_4k(length) {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "create guest range allocator",
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
    pub fn into_service(self) -> Arc<dyn GuestRangeAllocator> {
        Arc::new(self)
    }
}

impl GuestRangeAllocator for GuestRangePool {
    fn allocate(&self, size: usize) -> DeviceManagerResult<GuestPhysAddr> {
        validate_size(size, "allocate guest range")?;
        self.ranges()
            .allocate(size)
            .map(|range| GuestPhysAddr::from_usize(range.start))
            .ok_or(DeviceManagerError::OutOfMemory {
                operation: "allocate guest range",
            })
    }

    fn release(&self, addr: GuestPhysAddr, size: usize) -> DeviceManagerResult {
        validate_size(size, "release guest range")?;
        let end =
            addr.as_usize()
                .checked_add(size)
                .ok_or_else(|| DeviceManagerError::InvalidInput {
                    operation: "release guest range",
                    detail: "guest range end overflows the address space".into(),
                })?;
        if self.ranges().release(addr.as_usize()..end) {
            Ok(())
        } else {
            Err(DeviceManagerError::InvalidInput {
                operation: "release guest range",
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
