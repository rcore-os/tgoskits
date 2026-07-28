// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// ...

use crate::common::{self, Registry, mmio_addr, port_addr, sysreg_addr};

/// V1: Three `Vec`s — one per bus type — with O(n) linear scan on lookup.
///
/// Each entry stores `(base, end_or_count, slot)` where `end_or_count` is
/// - for MMIO:   `base + size` (exclusive end),
/// - for Port:   `base + size` (exclusive end),
/// - for SysReg: `count` (number of registers).
pub struct V1Registry {
    mmio: Vec<(u64, u64, usize)>,
    port: Vec<(u16, u16, usize)>,
    sysreg: Vec<(u32, u32, usize)>,
}

impl Registry for V1Registry {
    fn new_with_devices(n: usize) -> Self {
        let mut reg = Self {
            mmio: Vec::with_capacity(n),
            port: Vec::with_capacity(n),
            sysreg: Vec::with_capacity(n),
        };
        for i in 0..n {
            reg.mmio
                .push((mmio_addr(i), mmio_addr(i) + common::MMIO_SIZE, i));
            reg.port.push((
                port_addr(i),
                port_addr(i).wrapping_add(common::PORT_SIZE),
                i,
            ));
            reg.sysreg.push((sysreg_addr(i), common::SYSREG_COUNT, i));
        }
        reg
    }

    fn lookup_mmio(&self, addr: u64) -> Option<usize> {
        self.mmio
            .iter()
            .find(|&&(base, end, _)| addr >= base && addr < end)
            .map(|&(_, _, slot)| slot)
    }

    fn lookup_port(&self, addr: u16) -> Option<usize> {
        self.port
            .iter()
            .find(|&&(base, end, _)| addr >= base && addr < end)
            .map(|&(_, _, slot)| slot)
    }

    fn lookup_sysreg(&self, addr: u32) -> Option<usize> {
        self.sysreg
            .iter()
            .find(|&&(start, count, _)| {
                let end = start.saturating_add(count.saturating_sub(1));
                addr >= start && addr <= end
            })
            .map(|&(_, _, slot)| slot)
    }
}
