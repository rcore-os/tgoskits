// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Inter-VM Communication (IVC) channel manager.
//!
//! Extracted from the old `AxVmDevices` into an independent service.
//! Manages a global GPA range for shared memory channels between VMs.
//!
//! Uses a simple first-fit free list (`BTreeMap<usize, usize>` mapping `start → size`)
//! to track free regions.

use alloc::collections::BTreeMap;

use ax_errno::{AxResult, ax_err, ax_err_type};
use ax_memory_addr::is_aligned_4k;
use axaddrspace::GuestPhysAddr;

/// Manages IVC channel allocation within a pre-configured GPA range.
pub struct IVCManager {
    /// Free regions: start → size
    free_regions: BTreeMap<usize, usize>,
    /// Base address of the managed range
    base: usize,
    /// Total size of the managed range
    size: usize,
}

impl IVCManager {
    /// Create a new IVC manager managing the range `[base, base + size)`.
    pub fn new(base: usize, size: usize) -> Self {
        let mut free_regions = BTreeMap::new();
        free_regions.insert(base, size);
        Self {
            free_regions,
            base,
            size,
        }
    }

    /// Mutable version of alloc_channel (intended to be called through a Mutex).
    pub fn alloc_channel_mut(&mut self, size: usize) -> AxResult<GuestPhysAddr> {
        if size == 0 {
            return ax_err!(InvalidInput, "size must be > 0");
        }
        if !is_aligned_4k(size) {
            return ax_err!(InvalidInput, "size must be 4K-aligned");
        }

        // First-fit: find the first region large enough
        let addr = self
            .free_regions
            .iter()
            .find_map(|(&start, &region_size)| {
                if region_size >= size {
                    Some(start)
                } else {
                    None
                }
            })
            .ok_or_else(|| ax_err_type!(NoMemory, "IVC channel allocation failed"))?;

        let region_size = self.free_regions.remove(&addr).unwrap();
        let remaining = region_size - size;
        if remaining > 0 {
            self.free_regions.insert(addr + size, remaining);
        }

        Ok(GuestPhysAddr::from_usize(addr))
    }

    /// Release a previously allocated IVC channel.
    pub fn release_channel_mut(&mut self, addr: GuestPhysAddr, size: usize) -> AxResult {
        if size == 0 {
            return ax_err!(InvalidInput, "size must be > 0");
        }
        if !is_aligned_4k(size) {
            return ax_err!(InvalidInput, "size must be 4K-aligned");
        }

        let start = addr.as_usize();
        let end = start + size;

        // Merge with adjacent free regions
        let mut merged_start = start;
        let mut merged_end = end;

        if let Some((&next_start, &next_size)) = self.free_regions.range(start..).next()
            && next_start == end
        {
            merged_end = end + next_size;
            self.free_regions.remove(&next_start);
        }

        if let Some((&prev_start, &prev_size)) = self.free_regions.range(..start).next_back()
            && prev_start + prev_size == start
        {
            merged_start = prev_start;
            self.free_regions.remove(&prev_start);
        }

        self.free_regions
            .insert(merged_start, merged_end - merged_start);
        Ok(())
    }

    /// Return remaining free capacity.
    pub fn capacity(&self) -> usize {
        self.free_regions.values().sum()
    }

    /// Return the base address of the managed range.
    pub fn base(&self) -> usize {
        self.base
    }

    /// Return the total size of the managed range.
    pub fn total_size(&self) -> usize {
        self.size
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(missing_docs)]
mod tests {
    use super::*;

    #[test]
    fn test_alloc_and_release() {
        let mut mgr = IVCManager::new(0x1000_0000, 0x1_0000);
        let addr = mgr.alloc_channel_mut(0x4000).unwrap();
        assert_eq!(addr.as_usize(), 0x1000_0000);

        let addr2 = mgr.alloc_channel_mut(0x4000).unwrap();
        assert_eq!(addr2.as_usize(), 0x1000_4000);

        mgr.release_channel_mut(addr, 0x4000).unwrap();

        let addr3 = mgr.alloc_channel_mut(0x4000).unwrap();
        assert_eq!(addr3.as_usize(), 0x1000_0000);
    }

    #[test]
    fn test_alloc_exhaustion() {
        let mut mgr = IVCManager::new(0x1000_0000, 0x4000);
        mgr.alloc_channel_mut(0x4000).unwrap();
        assert!(mgr.alloc_channel_mut(0x1000).is_err());
    }

    #[test]
    fn test_merge_on_release() {
        let mut mgr = IVCManager::new(0x1000_0000, 0x10000);
        let a1 = mgr.alloc_channel_mut(0x4000).unwrap();
        let a2 = mgr.alloc_channel_mut(0x4000).unwrap();
        let _a3 = mgr.alloc_channel_mut(0x4000).unwrap();
        assert_eq!(mgr.capacity(), 0x4000);

        mgr.release_channel_mut(a2, 0x4000).unwrap();
        mgr.release_channel_mut(a1, 0x4000).unwrap();

        // 【修复】：释放两个 16KB 块后，加上剩余未分配的 16KB，总空闲容量应为 48KB (0xC000)
        assert_eq!(mgr.capacity(), 0xC000);

        let big = mgr.alloc_channel_mut(0x8000).unwrap();
        assert_eq!(big.as_usize(), 0x1000_0000);
    }
}
