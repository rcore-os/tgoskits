use alloc::{sync::Arc, vec, vec::Vec};
use core::ops::Range;

use ax_kspin::SpinRaw as Mutex;
use ax_memory_addr::is_aligned_4k;
use axvm_types::GuestPhysAddr;

use crate::{DeviceManagerError, DeviceManagerResult, ServiceCardinality, ServiceKey};

/// Allocates guest-physical ranges from one statically reserved window.
pub trait GuestRangeAllocator: Send + Sync {
    /// Reserves one page-aligned guest range.
    fn allocate(&self, size: usize) -> DeviceManagerResult<GuestPhysAddr>;

    /// Releases a previously reserved guest range.
    fn release(&self, addr: GuestPhysAddr, size: usize) -> DeviceManagerResult;
}

/// Type key for the VM's IVC guest-range allocator service.
pub struct GuestRangeAllocatorKey;

impl ServiceKey for GuestRangeAllocatorKey {
    type Service = dyn GuestRangeAllocator;

    const NAME: &'static str = "ivc-guest-range-allocator";
    const CARDINALITY: ServiceCardinality = ServiceCardinality::Single;
}

/// IVC's allocator over the guest range reserved during VM preparation.
pub(crate) struct IvcGuestRangeAllocator {
    ranges: Mutex<RangeAllocator>,
}

impl IvcGuestRangeAllocator {
    pub(crate) fn new(base: usize, length: usize) -> DeviceManagerResult<Self> {
        let end = base
            .checked_add(length)
            .ok_or_else(|| DeviceManagerError::InvalidConfig {
                operation: "create IVC guest range allocator",
                detail: "reserved guest range overflows the address space".into(),
            })?;
        if length == 0 || !is_aligned_4k(base) || !is_aligned_4k(length) {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "create IVC guest range allocator",
                detail: alloc::format!(
                    "base {base:#x} and length {length:#x} must be non-zero and 4 KiB aligned"
                ),
            });
        }
        Ok(Self {
            ranges: Mutex::new(RangeAllocator::new(base..end)),
        })
    }

    pub(crate) fn into_service(self) -> Arc<dyn GuestRangeAllocator> {
        Arc::new(self)
    }
}

impl GuestRangeAllocator for IvcGuestRangeAllocator {
    fn allocate(&self, size: usize) -> DeviceManagerResult<GuestPhysAddr> {
        if size == 0 || !is_aligned_4k(size) {
            return Err(DeviceManagerError::InvalidInput {
                operation: "allocate IVC guest range",
                detail: alloc::format!("size {size:#x} must be non-zero and 4 KiB aligned"),
            });
        }
        self.ranges
            .lock()
            .allocate_range(size)
            .map(|range| GuestPhysAddr::from_usize(range.start))
            .ok_or(DeviceManagerError::OutOfMemory {
                operation: "allocate IVC guest range",
            })
    }

    fn release(&self, addr: GuestPhysAddr, size: usize) -> DeviceManagerResult {
        if size == 0 || !is_aligned_4k(size) {
            return Err(DeviceManagerError::InvalidInput {
                operation: "release IVC guest range",
                detail: alloc::format!("size {size:#x} must be non-zero and 4 KiB aligned"),
            });
        }
        let end =
            addr.as_usize()
                .checked_add(size)
                .ok_or_else(|| DeviceManagerError::InvalidInput {
                    operation: "release IVC guest range",
                    detail: "guest range end overflows the address space".into(),
                })?;
        if self.ranges.lock().free_range(addr.as_usize()..end) {
            Ok(())
        } else {
            Err(DeviceManagerError::InvalidInput {
                operation: "release IVC guest range",
                detail: alloc::format!(
                    "range {:#x}..{end:#x} is outside the reserved window or is not allocated",
                    addr.as_usize()
                ),
            })
        }
    }
}

/// A minimal best-fit range allocator for IVC GPA ranges.
#[derive(Debug)]
pub(crate) struct RangeAllocator {
    initial: Range<usize>,
    free: Vec<Range<usize>>,
}

impl RangeAllocator {
    pub(crate) fn new(range: Range<usize>) -> Self {
        Self {
            initial: range.clone(),
            free: vec![range],
        }
    }

    pub(crate) fn allocate_range(&mut self, size: usize) -> Option<Range<usize>> {
        debug_assert!(size > 0);

        let mut best_fit = None;
        for (index, range) in self.free.iter().enumerate() {
            let len = range.end - range.start;
            if len < size {
                continue;
            }
            if len == size {
                best_fit = Some(index);
                break;
            }
            match best_fit {
                Some(best_index)
                    if len >= self.free[best_index].end - self.free[best_index].start => {}
                _ => best_fit = Some(index),
            }
        }

        let index = best_fit?;
        let start = self.free[index].start;
        let end = start + size;
        if self.free[index].end == end {
            self.free.remove(index);
        } else {
            self.free[index].start = end;
        }
        Some(start..end)
    }

    pub(crate) fn free_range(&mut self, range: Range<usize>) -> bool {
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

        if index > 0 && self.free[index - 1].end > range.start {
            return false;
        }
        if index < self.free.len() && range.end > self.free[index].start {
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
    use super::RangeAllocator;

    #[test]
    fn allocates_and_reuses_ranges() {
        let mut allocator = RangeAllocator::new(0..0x4000);

        assert_eq!(allocator.allocate_range(0x1000), Some(0..0x1000));
        assert_eq!(allocator.allocate_range(0x1000), Some(0x1000..0x2000));

        assert!(allocator.free_range(0..0x1000));
        assert_eq!(allocator.allocate_range(0x1000), Some(0..0x1000));
    }

    #[test]
    fn picks_best_fit_range() {
        let mut allocator = RangeAllocator::new(0..0x9000);

        assert_eq!(allocator.allocate_range(0x3000), Some(0..0x3000));
        assert_eq!(allocator.allocate_range(0x3000), Some(0x3000..0x6000));
        assert_eq!(allocator.allocate_range(0x3000), Some(0x6000..0x9000));

        assert!(allocator.free_range(0..0x3000));
        assert!(allocator.free_range(0x6000..0x9000));

        assert_eq!(allocator.allocate_range(0x3000), Some(0..0x3000));
        assert_eq!(allocator.allocate_range(0x3000), Some(0x6000..0x9000));
    }

    #[test]
    fn merges_neighboring_freed_ranges() {
        let mut allocator = RangeAllocator::new(0..0x3000);

        assert_eq!(allocator.allocate_range(0x1000), Some(0..0x1000));
        assert_eq!(allocator.allocate_range(0x1000), Some(0x1000..0x2000));
        assert_eq!(allocator.allocate_range(0x1000), Some(0x2000..0x3000));

        assert!(allocator.free_range(0..0x1000));
        assert!(allocator.free_range(0x2000..0x3000));
        assert!(allocator.free_range(0x1000..0x2000));

        assert_eq!(allocator.allocate_range(0x3000), Some(0..0x3000));
    }

    #[test]
    fn rejects_invalid_or_duplicate_frees() {
        let mut allocator = RangeAllocator::new(0x1000..0x3000);

        assert!(!allocator.free_range(0x1000..0x1000));
        assert!(!allocator.free_range(0..0x1000));
        assert!(!allocator.free_range(0x2000..0x4000));
        assert!(!allocator.free_range(0x1000..0x2000));

        assert_eq!(allocator.allocate_range(0x1000), Some(0x1000..0x2000));
        assert!(allocator.free_range(0x1000..0x2000));
        assert!(!allocator.free_range(0x1000..0x2000));
    }
}
