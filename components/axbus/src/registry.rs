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

//! Slot-map based device registry with interval-tree address routing.
//!
//! Replaces the old `AxEmuDevices<R>` (a plain `Vec`) with an indexed,
//! conflict-detecting container.
//!
//! # Design
//!
//! - **`slotmap::SlotMap`** for O(1) device lookup by `DeviceId`.
//! - **`RangeMap`** (interval tree backed by `BTreeMap`) for O(log n) address
//!   routing — same approach as crosvm and Firecracker.
//! - **Conflict detection** at registration time: no two devices may claim
//!   overlapping MMIO or PIO ranges.
//!
//! # References
//!
//! - crosvm: `devices/src/bus.rs` uses `BTreeMap<(u64, u64), BusEntry>` where
//!   the key is `(base, len)`. On lookup, `first_before()` does a `range(..=addr)`
//!   and then checks overlap.
//! - Firecracker: `vmm/src/vstate/bus.rs` follows the same pattern.
//!

use alloc::collections::BTreeMap;
use alloc::sync::Arc;

use slotmap::SlotMap;

use crate::r#trait::*;

// ── Key type for slotmap ───────────────────────────────────────────────────

slotmap::new_key_type! {
    /// Internal slotmap key, convertible to/from the public `DeviceId`.
    pub struct DeviceKey;
}

// Mapping is done via `IdMap` — no explicit key conversion needed outside
// the registry.

// Store a mapping from DeviceId to DeviceKey for fast O(1) reverse lookup.
// This avoids unsafe key reconstruction when the caller passes a DeviceId.
struct IdMap {
    keys: BTreeMap<u64, DeviceKey>,
    next_id: u64,
}

impl IdMap {
    /// Create an empty ID map.
    fn new() -> Self {
        Self {
            keys: BTreeMap::new(),
            next_id: 1,
        }
    }

    /// Allocate a new DeviceId for the given slotmap key.
    fn alloc(&mut self, key: DeviceKey) -> DeviceId {
        let id = self.next_id;
        self.next_id += 1;
        self.keys.insert(id, key);
        DeviceId::from_u64(id)
    }

    /// Look up the slotmap key for a DeviceId.
    fn lookup(&self, id: DeviceId) -> Option<DeviceKey> {
        self.keys.get(&id.0).copied()
    }

    /// Remove a DeviceId mapping, returning the slotmap key.
    fn remove(&mut self, id: DeviceId) -> Option<DeviceKey> {
        self.keys.remove(&id.0)
    }

    /// Reverse lookup: find the DeviceId for a given DeviceKey (O(n), fine for <100 devices).
    fn scan_for_key(&self, target: DeviceKey) -> Option<DeviceId> {
        for (&id, &key) in &self.keys {
            if key == target {
                return Some(DeviceId::from_u64(id));
            }
        }
        None
    }
}

// ── Interval tree ──────────────────────────────────────────────────────────

/// A simple interval tree using a `BTreeMap<u64, (u64, DeviceKey)>`.
///
/// Each entry maps `start → (end, device)`. Lookup is O(log n):
/// find the entry with the largest start ≤ addr, then check if addr falls
/// within [start, end).
struct RangeMap {
    intervals: BTreeMap<u64, (u64, DeviceKey)>,
}

impl RangeMap {
    /// Create an empty range map.
    fn new() -> Self {
        Self {
            intervals: BTreeMap::new(),
        }
    }

    /// Insert `[start, end)` → `device`.
    #[allow(dead_code)]
    fn insert(&mut self, start: u64, end: u64, device: DeviceKey) {
        self.intervals.insert(start, (end, device));
    }

    /// Lookup the device that owns the address `addr`.
    fn lookup(&self, addr: u64) -> Option<DeviceKey> {
        let (_, &(end, dev)) = self.intervals.range(..=addr).next_back()?;
        if addr < end {
            Some(dev)
        } else {
            None
        }
    }

    /// Remove the interval starting at `start`.
    fn remove(&mut self, start: u64) {
        self.intervals.remove(&start);
    }
}

// ── DeviceRegistry ─────────────────────────────────────────────────────────

/// An indexed, conflict-detecting container for emulated devices.
///

pub struct DeviceRegistry {
    slotmap: SlotMap<DeviceKey, Arc<dyn VirtualDevice>>,
    mmio_tree: RangeMap,
    pio_tree: RangeMap,
    id_map: IdMap,
}

impl DeviceRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            slotmap: SlotMap::with_key(),
            mmio_tree: RangeMap::new(),
            pio_tree: RangeMap::new(),
            id_map: IdMap::new(),
        }
    }

    /// Register a device, detecting address conflicts automatically.
    ///
    /// Returns the assigned `DeviceId` on success.
    pub fn register(&mut self, dev: Arc<dyn VirtualDevice>) -> Result<DeviceId> {
        let resources = dev.resources().to_vec();

        // Check all address resources for conflicts before inserting.
        for res in &resources {
            match res {
                Resource::Mmio(range) | Resource::SysReg(range) => {
                    let start = range.start;
                    let end = range.end;
                    if let Some((&prev_start, &(prev_end, _))) = self.mmio_tree.intervals.range(..start).next_back() {
                        if prev_end > start {
                            return Err(DeviceError::AddressConflict(Resource::Mmio(prev_start..prev_end)));
                        }
                    }
                    if let Some((&next_start, &(_, _))) = self.mmio_tree.intervals.range(start..).next() {
                        if end > next_start {
                            return Err(DeviceError::AddressConflict(Resource::Mmio(next_start..end)));
                        }
                    }
                }
                Resource::Pio(range) => {
                    let start = range.start as u64;
                    let end = range.end as u64;
                    if let Some((&prev_start, &(prev_end, _))) = self.pio_tree.intervals.range(..start).next_back() {
                        if prev_end > start {
                            return Err(DeviceError::AddressConflict(Resource::Pio(prev_start as u16..prev_end as u16)));
                        }
                    }
                    if let Some((&next_start, &(_, _))) = self.pio_tree.intervals.range(start..).next() {
                        if end > next_start {
                            return Err(DeviceError::AddressConflict(Resource::Pio(next_start as u16..end as u16)));
                        }
                    }
                }
                Resource::Irq(_) => {}
            }
        }

        // Insert the device into the slotmap first to get its key.
        let key = self.slotmap.insert(dev);

        // Allocate a DeviceId and store the mapping.
        let id = self.id_map.alloc(key);

        // Register all address resources in the interval trees.
        for res in &resources {
            match res {
                Resource::Mmio(range) | Resource::SysReg(range) => {
                    self.mmio_tree.intervals.insert(range.start, (range.end, key));
                }
                Resource::Pio(range) => {
                    self.pio_tree.intervals.insert(range.start as u64, (range.end as u64, key));
                }
                Resource::Irq(_) => {}
            }
        }

        Ok(id)
    }

    /// Unregister a device by its ID.
    pub fn unregister(&mut self, id: DeviceId) -> Option<Arc<dyn VirtualDevice>> {
        let key = self.id_map.remove(id)?;
        let dev = self.slotmap.remove(key)?;

        for res in dev.resources() {
            match res {
                Resource::Mmio(range) | Resource::SysReg(range) => self.mmio_tree.remove(range.start),
                Resource::Pio(range) => self.pio_tree.remove(range.start as u64),
                Resource::Irq(_) => {}
            }
        }

        Some(dev)
    }

    /// Look up a device by its ID.
    pub fn get(&self, id: DeviceId) -> Option<Arc<dyn VirtualDevice>> {
        let key = self.id_map.lookup(id)?;
        self.slotmap.get(key).cloned()
    }

    /// Look up a device by address on a specific bus.
    pub fn lookup(&self, bus: BusKind, addr: u64) -> Option<Arc<dyn VirtualDevice>> {
        let key = match bus {
            BusKind::Mmio => self.mmio_tree.lookup(addr)?,
            BusKind::Pio => self.pio_tree.lookup(addr)?,
            BusKind::SysReg => {
                return None;
            }
        };
        self.slotmap.get(key).cloned()
    }

    /// Iterate over all registered devices.
    pub fn iter(&self) -> impl Iterator<Item = (DeviceId, &Arc<dyn VirtualDevice>)> {
        // We can only iterate in slotmap order. To get DeviceId, look up the id_map in reverse.
        // For efficiency, we collect and map; this is O(n) which is fine for VM setup.
        let id_map = &self.id_map;
        self.slotmap
            .iter()
            .filter_map(move |(slot_key, v)| {
                // Find the DeviceId for this slot key by scanning id_map.
                // In practice, this is fine for VM device counts (< 100).
                id_map.scan_for_key(slot_key).map(|id| (id, v))
            })
    }

    /// Number of registered devices.
    pub fn len(&self) -> usize {
        self.slotmap.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slotmap.is_empty()
    }

    /// Route a bus access to the appropriate device and return the response.
    pub fn handle_access(&self, bus: BusKind, access: &BusAccess) -> BusResponse {
        let dev = match bus {
            BusKind::SysReg => {
                // For SysReg, iterate all devices and try each one
                // (SysReg ranges are typically sparse)
                for (_, dev) in self.slotmap.iter() {
                    let resp = dev.handle_access(bus, access);
                    if !matches!(resp, BusResponse::NoDevice) {
                        return resp;
                    }
                }
                return BusResponse::NoDevice;
            }
            _ => match self.lookup(bus, access.addr()) {
                Some(d) => d,
                None => return BusResponse::NoDevice,
            },
        };
        dev.handle_access(bus, access)
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(missing_docs)]
mod tests {
    use super::*;
    use core::any::Any;

    #[derive(Debug)]
    struct DummyDevice {
        id: DeviceId,
        resources: Vec<Resource>,
    }

    impl VirtualDevice for DummyDevice {
        fn id(&self) -> DeviceId {
            self.id
        }
        fn name(&self) -> &str {
            "dummy"
        }
        fn resources(&self) -> &[Resource] {
            &self.resources
        }
        fn handle_access(&self, _bus: BusKind, access: &BusAccess) -> BusResponse {
            match access {
                BusAccess::Read { .. } => BusResponse::Success(Some(42)),
                BusAccess::Write { .. } => BusResponse::Success(None),
            }
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[test]
    fn test_register_and_lookup() {
        let mut reg = DeviceRegistry::new();
        let dev = Arc::new(DummyDevice {
            id: DeviceId::from_u64(0),
            resources: alloc::vec![Resource::Mmio(0x1000..0x2000)],
        });
        let id = reg.register(dev.clone()).unwrap();
        assert_ne!(id, DeviceId::from_u64(0));

        // lookup by addr
        let found = reg.lookup(BusKind::Mmio, 0x1500).unwrap();
        assert_eq!(found.name(), "dummy");

        // lookup out of range
        assert!(reg.lookup(BusKind::Mmio, 0x3000).is_none());
    }

    #[test]
    fn test_address_conflict_detected() {
        let mut reg = DeviceRegistry::new();
        let dev1 = Arc::new(DummyDevice {
            id: DeviceId::from_u64(0),
            resources: alloc::vec![Resource::Mmio(0x1000..0x2000)],
        });
        let dev2 = Arc::new(DummyDevice {
            id: DeviceId::from_u64(0),
            resources: alloc::vec![Resource::Mmio(0x1800..0x2800)],
        });

        reg.register(dev1).unwrap();

        let err = reg.register(dev2).unwrap_err();
        assert!(matches!(err, DeviceError::AddressConflict(_)));
    }

    #[test]
    fn test_unregister() {
        let mut reg = DeviceRegistry::new();
        let dev = Arc::new(DummyDevice {
            id: DeviceId::from_u64(0),
            resources: alloc::vec![Resource::Mmio(0x1000..0x2000)],
        });
        let id = reg.register(dev.clone()).unwrap();

        assert!(reg.lookup(BusKind::Mmio, 0x1500).is_some());

        reg.unregister(id);

        assert!(reg.lookup(BusKind::Mmio, 0x1500).is_none());
        assert!(reg.get(id).is_none());
    }

    #[test]
    fn test_pio_and_mmio_independence() {
        let mut reg = DeviceRegistry::new();
        let dev = Arc::new(DummyDevice {
            id: DeviceId::from_u64(0),
            resources: alloc::vec![
                Resource::Mmio(0x1000..0x2000),
                Resource::Pio(0x60..0x64),
            ],
        });
        reg.register(dev).unwrap();

        assert!(reg.lookup(BusKind::Mmio, 0x1500).is_some());
        assert!(reg.lookup(BusKind::Pio, 0x62).is_some());
        assert!(reg.lookup(BusKind::Pio, 0x1500).is_none());
        assert!(reg.lookup(BusKind::Mmio, 0x62).is_none());
    }

    #[test]
    fn test_empty_registry() {
        let reg = DeviceRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        assert!(reg.lookup(BusKind::Mmio, 0x1000).is_none());
    }

    #[test]
    fn test_sysreg_lookup_returns_none() {
        // SysReg devices do NOT participate in address-based lookup.
        let reg = DeviceRegistry::new();
        assert!(reg.lookup(BusKind::SysReg, 0x1234).is_none());
    }

    #[test]
    fn test_port_range_out_of_bounds() {
        let mut reg = DeviceRegistry::new();
        let dev = Arc::new(DummyDevice {
            id: DeviceId::from_u64(0),
            resources: vec![Resource::Pio(0x60..0x64)],
        });
        reg.register(dev).unwrap();
        // inside
        assert!(reg.lookup(BusKind::Pio, 0x62).is_some());
        // before range
        assert!(reg.lookup(BusKind::Pio, 0x10).is_none());
        // after range
        assert!(reg.lookup(BusKind::Pio, 0x70).is_none());
    }

    #[test]
    fn test_pio_address_conflict() {
        let mut reg = DeviceRegistry::new();
        let dev1 = Arc::new(DummyDevice {
            id: DeviceId::from_u64(0),
            resources: vec![Resource::Pio(0x60..0x70)],
        });
        let dev2 = Arc::new(DummyDevice {
            id: DeviceId::from_u64(0),
            resources: vec![Resource::Pio(0x68..0x78)],
        });
        reg.register(dev1).unwrap();
        let err = reg.register(dev2).unwrap_err();
        assert!(matches!(err, DeviceError::AddressConflict(_)));
    }

    #[test]
    fn test_zero_length_resource() {
        let mut reg = DeviceRegistry::new();
        let dev = Arc::new(DummyDevice {
            id: DeviceId::from_u64(0),
            resources: vec![Resource::Mmio(0x1000..0x1000)], // empty range
        });
        // Should still register, but can't be looked up.
        let _id = reg.register(dev).unwrap();
        assert!(reg.lookup(BusKind::Mmio, 0x1000).is_none());
    }

    #[test]
    fn test_register_then_iter() {
        let mut reg = DeviceRegistry::new();
        let dev = Arc::new(DummyDevice {
            id: DeviceId::from_u64(0),
            resources: vec![Resource::Mmio(0x1000..0x2000)],
        });
        reg.register(dev).unwrap();
        let count = reg.iter().count();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_handle_access_sysreg_fallback() {
        // SysReg handle_access iterates all devices and returns NoDevice
        // when none claims the access.
        let mut reg = DeviceRegistry::new();
        #[derive(Debug)]
        struct SysRegOnly;
        impl VirtualDevice for SysRegOnly {
            fn id(&self) -> DeviceId { DeviceId(0) }
            fn name(&self) -> &str { "sysreg-only" }
            fn resources(&self) -> &[Resource] { &[] }
            fn handle_access(&self, bus: BusKind, access: &BusAccess) -> BusResponse {
                match (bus, access) {
                    (BusKind::SysReg, BusAccess::Read { addr, .. }) if *addr == 0x1000 => {
                        BusResponse::Success(Some(1))
                    }
                    _ => BusResponse::NoDevice,
                }
            }
            fn as_any(&self) -> &dyn Any { self }
        }
        reg.register(Arc::new(SysRegOnly)).unwrap();
        // mismatch address → NoDevice
        let resp = reg.handle_access(
            BusKind::SysReg,
            &BusAccess::Read { addr: 0x9999, width: AccessWidth::U32 },
        );
        assert!(matches!(resp, BusResponse::NoDevice));
        // matching address → Success
        let resp = reg.handle_access(
            BusKind::SysReg,
            &BusAccess::Read { addr: 0x1000, width: AccessWidth::U32 },
        );
        assert!(matches!(resp, BusResponse::Success(Some(1))));
    }

    #[test]
    fn test_exact_boundary_match() {
        let mut reg = DeviceRegistry::new();
        let dev = Arc::new(DummyDevice {
            id: DeviceId::from_u64(0),
            resources: vec![Resource::Mmio(0x1000..0x2000)],
        });
        reg.register(dev).unwrap();
        // start address
        assert!(reg.lookup(BusKind::Mmio, 0x1000).is_some());
        // just before end (exclusive end)
        assert!(reg.lookup(BusKind::Mmio, 0x1fff).is_some());
        // at end (exclusive, should not match)
        assert!(reg.lookup(BusKind::Mmio, 0x2000).is_none());
    }
}
