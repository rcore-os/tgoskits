// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// ...

use alloc::collections::BTreeMap;

use crate::common::{self, Registry, mmio_addr, port_addr, sysreg_addr};

/// A minimal `RangeEntry` — the same shape as the production code.
struct RangeEntry {
    slot: usize,
    size: u64,
}

/// V3: Three `BTreeMap<Addr, RangeEntry>` with size cached in the map value.
///
/// This mirrors the current production `AxVmDevices` implementation. Lookup
/// finds the predecessor entry via `range(..=addr).next_back()` then does a
/// pure arithmetic bounds check — zero allocation, zero extra indirection.
pub struct V3Registry {
    mmio_index: BTreeMap<u64, RangeEntry>,
    port_index: BTreeMap<u16, RangeEntry>,
    sysreg_index: BTreeMap<u32, RangeEntry>,
}

impl Registry for V3Registry {
    fn new_with_devices(n: usize) -> Self {
        let mut reg = Self {
            mmio_index: BTreeMap::new(),
            port_index: BTreeMap::new(),
            sysreg_index: BTreeMap::new(),
        };
        for i in 0..n {
            reg.mmio_index.insert(
                mmio_addr(i),
                RangeEntry {
                    slot: i,
                    size: common::MMIO_SIZE,
                },
            );
            reg.port_index.insert(
                port_addr(i),
                RangeEntry {
                    slot: i,
                    size: common::PORT_SIZE as u64,
                },
            );
            reg.sysreg_index.insert(
                sysreg_addr(i),
                RangeEntry {
                    slot: i,
                    size: common::SYSREG_COUNT as u64,
                },
            );
        }
        reg
    }

    fn lookup_mmio(&self, addr: u64) -> Option<usize> {
        let (&base, entry) = self.mmio_index.range(..=addr).next_back()?;
        (addr < base.wrapping_add(entry.size)).then_some(entry.slot)
    }

    fn lookup_port(&self, addr: u16) -> Option<usize> {
        let (&base, entry) = self.port_index.range(..=addr).next_back()?;
        ((addr as u64) < (base as u64).wrapping_add(entry.size)).then_some(entry.slot)
    }

    fn lookup_sysreg(&self, addr: u32) -> Option<usize> {
        let (&start, entry) = self.sysreg_index.range(..=addr).next_back()?;
        let end = start.saturating_add((entry.size as u32).saturating_sub(1));
        (addr <= end).then_some(entry.slot)
    }
}
