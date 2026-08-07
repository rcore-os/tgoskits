// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// ...

use alloc::{collections::BTreeMap, vec::Vec};

use crate::common::{self, Registry, mmio_addr, port_addr, sysreg_addr};

/// A minimal `RangeEntry` — same shape as the production code.
#[derive(Clone, Copy)]
struct RangeEntry {
    slot: usize,
    size: u64,
}

/// V4: Build-time `BTreeMap` for conflict-free registration, then `finish()`
/// converts to a sorted `Vec<(Addr, RangeEntry)>` for `binary_search_by`
/// lookups with better cache locality.
pub struct V4Registry {
    mmio_vec: Vec<(u64, RangeEntry)>,
    port_vec: Vec<(u16, RangeEntry)>,
    sysreg_vec: Vec<(u32, RangeEntry)>,
}

impl Registry for V4Registry {
    fn new_with_devices(n: usize) -> Self {
        // Phase 1: register into temporary BTreeMaps.
        let mut mmio_tmp: BTreeMap<u64, RangeEntry> = BTreeMap::new();
        let mut port_tmp: BTreeMap<u16, RangeEntry> = BTreeMap::new();
        let mut sysreg_tmp: BTreeMap<u32, RangeEntry> = BTreeMap::new();

        for i in 0..n {
            mmio_tmp.insert(
                mmio_addr(i),
                RangeEntry {
                    slot: i,
                    size: common::MMIO_SIZE,
                },
            );
            port_tmp.insert(
                port_addr(i),
                RangeEntry {
                    slot: i,
                    size: common::PORT_SIZE as u64,
                },
            );
            sysreg_tmp.insert(
                sysreg_addr(i),
                RangeEntry {
                    slot: i,
                    size: common::SYSREG_COUNT as u64,
                },
            );
        }

        // Phase 2: finish — drain sorted BTreeMaps into sorted Vecs.
        let reg = Self {
            mmio_vec: mmio_tmp.into_iter().collect(),
            port_vec: port_tmp.into_iter().collect(),
            sysreg_vec: sysreg_tmp.into_iter().collect(),
        };

        // BTreeMap iteration yields keys in ascending order, so the Vecs
        // are already sorted. Assert for safety.
        debug_assert!(reg.mmio_vec.windows(2).all(|w| w[0].0 < w[1].0));
        debug_assert!(reg.port_vec.windows(2).all(|w| w[0].0 < w[1].0));
        debug_assert!(reg.sysreg_vec.windows(2).all(|w| w[0].0 < w[1].0));

        reg
    }

    fn lookup_mmio(&self, addr: u64) -> Option<usize> {
        let idx = match self.mmio_vec.binary_search_by_key(&addr, |&(k, _)| k) {
            Ok(i) => i,
            Err(0) => return None,
            Err(i) => i - 1,
        };
        let (base, entry) = &self.mmio_vec[idx];
        (addr < base.wrapping_add(entry.size)).then_some(entry.slot)
    }

    fn lookup_port(&self, addr: u16) -> Option<usize> {
        let idx = match self.port_vec.binary_search_by_key(&addr, |&(k, _)| k) {
            Ok(i) => i,
            Err(0) => return None,
            Err(i) => i - 1,
        };
        let (base, entry) = &self.port_vec[idx];
        ((addr as u64) < (*base as u64).wrapping_add(entry.size)).then_some(entry.slot)
    }

    fn lookup_sysreg(&self, addr: u32) -> Option<usize> {
        let idx = match self.sysreg_vec.binary_search_by_key(&addr, |&(k, _)| k) {
            Ok(i) => i,
            Err(0) => return None,
            Err(i) => i - 1,
        };
        let (start, entry) = &self.sysreg_vec[idx];
        let end = start.saturating_add((entry.size as u32).saturating_sub(1));
        (addr <= end).then_some(entry.slot)
    }
}
