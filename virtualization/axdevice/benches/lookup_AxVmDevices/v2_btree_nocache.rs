// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// ...

use alloc::{collections::BTreeMap, vec::Vec};

use crate::common::{self, Registry, mmio_addr, port_addr, sysreg_addr};

/// V2: Three `BTreeMap<Addr, slot>` indexes without cached size.
///
/// On lookup we need a second step to retrieve the size — modelling the
/// original `dev.resources()` → `Vec<Resource>` allocation + iteration.
/// We simulate this with a parallel `Vec` of per-slot range metadata plus
/// an explicit heap allocation per lookup.
pub struct V2Registry {
    mmio_index: BTreeMap<u64, usize>,
    mmio_meta: Vec<(u64, u64)>,

    port_index: BTreeMap<u16, usize>,
    port_meta: Vec<(u16, u16)>,

    sysreg_index: BTreeMap<u32, usize>,
    sysreg_meta: Vec<(u32, u32)>,
}

impl Registry for V2Registry {
    fn new_with_devices(n: usize) -> Self {
        let mut reg = Self {
            mmio_index: BTreeMap::new(),
            mmio_meta: Vec::with_capacity(n),
            port_index: BTreeMap::new(),
            port_meta: Vec::with_capacity(n),
            sysreg_index: BTreeMap::new(),
            sysreg_meta: Vec::with_capacity(n),
        };
        for i in 0..n {
            let mbase = mmio_addr(i);
            reg.mmio_index.insert(mbase, i);
            reg.mmio_meta.push((mbase, common::MMIO_SIZE));

            let pbase = port_addr(i);
            reg.port_index.insert(pbase, i);
            reg.port_meta.push((pbase, common::PORT_SIZE));

            let sbase = sysreg_addr(i);
            reg.sysreg_index.insert(sbase, i);
            reg.sysreg_meta.push((sbase, common::SYSREG_COUNT));
        }
        reg
    }

    fn lookup_mmio(&self, addr: u64) -> Option<usize> {
        let (&base, &slot) = self.mmio_index.range(..=addr).next_back()?;
        // Simulate dev.resources() → Vec<Resource> allocation.
        let resources: Vec<(u64, u64)> = self
            .mmio_meta
            .iter()
            .filter(|&&(..)| true) // touch all entries (like real iteration)
            .copied()
            .collect();
        let found = resources
            .iter()
            .any(|&(b, s)| b == base && s > 0 && addr < b.wrapping_add(s));
        found.then_some(slot)
    }

    fn lookup_port(&self, addr: u16) -> Option<usize> {
        let (&base, &slot) = self.port_index.range(..=addr).next_back()?;
        let resources: Vec<(u16, u16)> = self
            .port_meta
            .iter()
            .filter(|&&(..)| true)
            .copied()
            .collect();
        let found = resources
            .iter()
            .any(|&(b, s)| b == base && s > 0 && (addr as u32) < (b as u32).wrapping_add(s as u32));
        found.then_some(slot)
    }

    fn lookup_sysreg(&self, addr: u32) -> Option<usize> {
        let (&start, &slot) = self.sysreg_index.range(..=addr).next_back()?;
        let resources: Vec<(u32, u32)> = self
            .sysreg_meta
            .iter()
            .filter(|&&(..)| true)
            .copied()
            .collect();
        let found = resources.iter().any(|&(b, count)| {
            b == start && count > 0 && addr <= b.saturating_add(count.saturating_sub(1))
        });
        found.then_some(slot)
    }
}
