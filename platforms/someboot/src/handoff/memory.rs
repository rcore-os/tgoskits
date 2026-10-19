//! Allocation-free, transactional interval reconciliation for boot ownership.
use super::{
    HandoffError,
    protocol::{MAX_REGIONS, MemoryRegion},
};
use crate::boot_resources::PhysicalRange;

#[cfg(any(test, all(efi, target_arch = "loongarch64", not(target_os = "uefi"))))]
pub(crate) const FREE: u32 = 0;
pub(crate) const RESERVED: u32 = 3;
pub(crate) const BOOT_DATA: u32 = 6;
pub(crate) const RECLAIMABLE: u32 = 7;

#[derive(Clone)]
pub(crate) struct BootMap<const N: usize = MAX_REGIONS> {
    pub regions: [MemoryRegion; N],
    pub len: usize,
}
impl<const N: usize> BootMap<N> {
    pub const fn new() -> Self {
        Self {
            regions: [MemoryRegion {
                range: PhysicalRange::new(0, 0),
                kind: 0,
                reserved: 0,
            }; N],
            len: 0,
        }
    }
    pub fn from_regions(regions: &[MemoryRegion]) -> Result<Self, HandoffError> {
        let mut map = Self::new();
        for region in regions {
            map.push(*region)?;
        }
        map.regions[..map.len].sort_unstable_by_key(|r| r.range.start);
        Ok(map)
    }
    fn push(&mut self, region: MemoryRegion) -> Result<(), HandoffError> {
        if region.range.len == 0 {
            return Ok(());
        }
        if self.len == N {
            return Err(HandoffError::InvalidProtocol);
        }
        self.regions[self.len] = region;
        self.len += 1;
        Ok(())
    }
    /// Reclassifies covered RAM only. Firmware and MMIO are never overridable.
    /// Only the first/last overlapping regions can split: a contiguous overlay
    /// adds at most two boundaries, even when it spans many existing regions.
    /// Build a candidate first: overflow or a protected overlap leaves self intact.
    pub fn assign(&mut self, range: PhysicalRange, kind: u32) -> Result<(), HandoffError> {
        if !range.valid() || !range.start.is_multiple_of(4096) || !range.len.is_multiple_of(4096) {
            return Err(HandoffError::InvalidProtocol);
        }
        let end = range.end().ok_or(HandoffError::InvalidProtocol)?;
        let mut covered = 0;
        let mut next = Self::new();
        for region in &self.regions[..self.len] {
            let a = region.range.start;
            let b = region.range.end().ok_or(HandoffError::InvalidProtocol)?;
            if a >= end || b <= range.start {
                next.push(*region)?;
                continue;
            }
            if matches!(region.kind, RESERVED | 4 | 1) {
                return Err(HandoffError::InvalidProtocol);
            }
            let lo = a.max(range.start);
            let hi = b.min(end);
            covered += hi - lo;
            next.push(MemoryRegion {
                range: PhysicalRange::new(a, lo - a),
                ..*region
            })?;
            next.push(MemoryRegion {
                range: PhysicalRange::new(lo, hi - lo),
                kind,
                reserved: 0,
            })?;
            next.push(MemoryRegion {
                range: PhysicalRange::new(hi, b - hi),
                ..*region
            })?;
        }
        if covered != range.len {
            return Err(HandoffError::InvalidProtocol);
        }
        next.coalesce();
        *self = next;
        Ok(())
    }
    pub fn coalesce(&mut self) {
        let mut out = 0;
        for i in 0..self.len {
            let region = self.regions[i];
            if out != 0
                && self.regions[out - 1].kind == region.kind
                && self.regions[out - 1].range.end() == Some(region.range.start)
            {
                self.regions[out - 1].range.len += region.range.len;
            } else {
                self.regions[out] = region;
                out += 1;
            }
        }
        self.len = out;
    }
    #[cfg(any(test, all(efi, target_arch = "loongarch64", not(target_os = "uefi"))))]
    pub fn complete_takeover(&mut self) -> u64 {
        let mut bytes = 0;
        for region in &mut self.regions[..self.len] {
            if region.kind == RECLAIMABLE {
                bytes += region.range.len;
                region.kind = FREE;
            }
        }
        self.coalesce();
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn takeover_excludes_survivors_and_firmware_and_is_transactional() {
        let mut map = BootMap::<MAX_REGIONS>::from_regions(&[
            MemoryRegion {
                range: PhysicalRange::new(4096, 8 * 4096),
                kind: RECLAIMABLE,
                reserved: 0,
            },
            MemoryRegion {
                range: PhysicalRange::new(9 * 4096, 4096),
                kind: RESERVED,
                reserved: 0,
            },
        ])
        .unwrap();
        map.assign(PhysicalRange::new(3 * 4096, 2 * 4096), BOOT_DATA)
            .unwrap();
        assert!(!map.regions[..map.len].iter().any(|r| r.kind == FREE));
        let before = map.len;
        assert!(
            map.assign(PhysicalRange::new(8 * 4096, 2 * 4096), FREE)
                .is_err()
        );
        assert_eq!(map.len, before);
        assert_eq!(map.complete_takeover(), 6 * 4096);
        assert_eq!(map.complete_takeover(), 0);
        assert_eq!(map.regions[1].kind, BOOT_DATA);
        assert_eq!(map.regions[3].kind, RESERVED);
        // Too many splits fail without publishing a partially modified map.
        let regions: alloc::vec::Vec<_> = (0..MAX_REGIONS)
            .map(|i| MemoryRegion {
                range: PhysicalRange::new((i as u64 * 2 + 1) * 4096, 8192),
                kind: FREE,
                reserved: 0,
            })
            .collect();
        let mut full = BootMap::<MAX_REGIONS>::from_regions(&regions).unwrap();
        assert!(
            full.assign(PhysicalRange::new(8192, 4096), BOOT_DATA)
                .is_err()
        );
        assert_eq!(full.len, MAX_REGIONS);
        assert!(full.regions.iter().all(|r| r.kind == FREE));
    }
}
