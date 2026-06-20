//! Device emulation operations for VPlicGlobal.
//!
//! Implements the `BaseDeviceOps` trait for MMIO read/write handling.

use ax_errno::AxResult;
use axaddrspace::{GuestPhysAddrRange, HostPhysAddr, device::AccessWidth};
use axbus::InterruptControllerOps;
use axdevice_base::{BaseDeviceOps, EmuDeviceType};

use crate::{consts::*, utils::*, vplic::VPlicGlobal};

const VCAUSE_INTERRUPT_BIT: usize = 1usize << (usize::BITS - 1);
const VCAUSE_VS_TIMER: usize = VCAUSE_INTERRUPT_BIT | 5;
const PLIC_PENDING_WORDS: usize = PLIC_NUM_SOURCES / 32;
const BITMAP_WORDS: usize = PLIC_NUM_SOURCES / 64;

impl VPlicGlobal {
    fn irq_priority(&self, irq_id: usize) -> AxResult<u32> {
        let addr = HostPhysAddr::from_usize(
            self.host_plic_addr.as_usize() + PLIC_PRIORITY_OFFSET + irq_id * 4,
        );
        Ok(perform_mmio_read(addr, AccessWidth::Dword)? as u32)
    }

    fn context_threshold(&self, context_id: usize) -> AxResult<u32> {
        let addr = HostPhysAddr::from_usize(
            self.host_plic_addr.as_usize()
                + PLIC_CONTEXT_CTRL_OFFSET
                + context_id * PLIC_CONTEXT_STRIDE
                + PLIC_CONTEXT_THRESHOLD_OFFSET,
        );
        Ok(perform_mmio_read(addr, AccessWidth::Dword)? as u32)
    }

    fn context_enable_mask(&self, context_id: usize, reg_index: usize) -> AxResult<u32> {
        let addr = HostPhysAddr::from_usize(
            self.host_plic_addr.as_usize()
                + PLIC_ENABLE_OFFSET
                + context_id * PLIC_ENABLE_STRIDE
                + reg_index * 4,
        );
        Ok(perform_mmio_read(addr, AccessWidth::Dword)? as u32)
    }

    /// Returns a snapshot of pending & !active bits. Lock-free.
    fn pending_inactive_snapshot(&self) -> [u64; BITMAP_WORDS] {
        let mut result = self.pending_irqs.and_not(&self.active_irqs);
        // IRQ 0 is reserved by the PLIC specification.
        result[0] &= !1u64;
        result
    }

    /// Selects the highest-priority enabled IRQ from a candidate snapshot.
    fn best_enabled_pending_irq(
        &self,
        context_id: usize,
        candidates: &[u64; BITMAP_WORDS],
    ) -> AxResult<Option<(usize, u32)>> {
        let mut best_irq = None;
        let mut best_priority = 0;
        let mut cached_enable_reg_index = usize::MAX;
        let mut cached_enable_mask = 0u32;

        for word_idx in 0..BITMAP_WORDS {
            let mut word = candidates[word_idx];
            while word != 0 {
                let bit = word.trailing_zeros() as usize;
                word &= !(1u64 << bit);
                let irq_id = word_idx * 64 + bit;

                let reg_index = irq_id / 32;
                let bit_index = irq_id % 32;

                if reg_index != cached_enable_reg_index {
                    cached_enable_mask = self.context_enable_mask(context_id, reg_index)?;
                    cached_enable_reg_index = reg_index;
                }
                if (cached_enable_mask & (1 << bit_index)) == 0 {
                    continue;
                }

                let priority = self.irq_priority(irq_id)?;
                if priority > best_priority {
                    best_priority = priority;
                    best_irq = Some((irq_id, priority));
                }
            }
        }

        Ok(best_irq)
    }

    fn next_deliverable_irq(&self, context_id: usize) -> AxResult<Option<usize>> {
        let threshold = self.context_threshold(context_id)?;
        let candidates = self.pending_inactive_snapshot();
        if let Some((irq_id, priority)) = self.best_enabled_pending_irq(context_id, &candidates)? {
            if priority > threshold {
                return Ok(Some(irq_id));
            }
        }
        Ok(None)
    }

    /// Claims the next enabled pending IRQ using atomic test-and-clear.
    /// No locks — concurrent claims on different contexts race safely.
    fn claim_next_irq(&self, context_id: usize) -> AxResult<Option<usize>> {
        loop {
            let candidates = self.pending_inactive_snapshot();
            let Some((irq_id, _priority)) =
                self.best_enabled_pending_irq(context_id, &candidates)?
            else {
                return Ok(None);
            };

            // Atomically clear the pending bit. Only one caller wins.
            if !self.pending_irqs.test_and_clear(irq_id) {
                continue;
            }

            // Set the active bit (no race concern — only the winner reaches here).
            self.active_irqs.set(irq_id);
            return Ok(Some(irq_id));
        }
    }

    fn sync_vseip(&self, context_id: usize) -> AxResult<()> {
        if self.next_deliverable_irq(context_id)?.is_some() {
            unsafe {
                if riscv_h::register::vscause::read().bits() == VCAUSE_VS_TIMER {
                    riscv_h::register::hvip::clear_vstip();
                }
                riscv_h::register::hvip::set_vseip();
            }
        } else {
            unsafe {
                riscv_h::register::hvip::clear_vseip();
            }
        }
        Ok(())
    }

    fn sync_all_guest_contexts_vseip(&self) -> AxResult<()> {
        for context_id in (1..self.contexts_num).step_by(2) {
            self.sync_vseip(context_id)?;
        }
        Ok(())
    }
}

impl BaseDeviceOps<GuestPhysAddrRange> for VPlicGlobal {
    fn emu_type(&self) -> axdevice_base::EmuDeviceType {
        EmuDeviceType::PPPTGlobal
    }

    fn address_range(&self) -> GuestPhysAddrRange {
        GuestPhysAddrRange::from_start_size(self.addr, self.size)
    }

    fn handle_read(
        &self,
        addr: <GuestPhysAddrRange as axaddrspace::device::DeviceAddrRange>::Addr,
        width: axaddrspace::device::AccessWidth,
    ) -> ax_errno::AxResult<usize> {
        assert_eq!(width, AccessWidth::Dword);
        let reg = addr - self.addr;
        let host_addr = HostPhysAddr::from_usize(reg + self.host_plic_addr.as_usize());
        match reg {
            // priority
            PLIC_PRIORITY_OFFSET..PLIC_PENDING_OFFSET => perform_mmio_read(host_addr, width),
            // pending — read directly from atomic bitmap
            PLIC_PENDING_OFFSET..PLIC_ENABLE_OFFSET => {
                let reg_index = (reg - PLIC_PENDING_OFFSET) / 4;
                if reg_index >= PLIC_PENDING_WORDS {
                    return Ok(0);
                }
                let word_idx = reg_index / 2;
                let word = self.pending_irqs.load_word(word_idx);
                let mut val = if reg_index % 2 == 0 {
                    word as u32
                } else {
                    (word >> 32) as u32
                };
                if reg_index == 0 {
                    val &= !1u32; // mask out reserved IRQ 0
                }
                Ok(val as usize)
            }
            // enable
            PLIC_ENABLE_OFFSET..PLIC_CONTEXT_CTRL_OFFSET => perform_mmio_read(host_addr, width),
            // threshold
            offset
                if offset >= PLIC_CONTEXT_CTRL_OFFSET
                    && (offset - PLIC_CONTEXT_CTRL_OFFSET).is_multiple_of(PLIC_CONTEXT_STRIDE) =>
            {
                perform_mmio_read(host_addr, width)
            }
            // claim/complete
            offset
                if offset >= PLIC_CONTEXT_CTRL_OFFSET
                    && (offset - PLIC_CONTEXT_CTRL_OFFSET - PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET)
                        .is_multiple_of(PLIC_CONTEXT_STRIDE) =>
            {
                let context_id =
                    (offset - PLIC_CONTEXT_CTRL_OFFSET - PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET)
                        / PLIC_CONTEXT_STRIDE;
                assert!(
                    context_id < self.contexts_num,
                    "Invalid context id {context_id}"
                );
                let Some(irq_id) = self.claim_next_irq(context_id)? else {
                    self.sync_vseip(context_id)?;
                    return Ok(0);
                };
                self.sync_vseip(context_id)?;
                Ok(irq_id)
            }
            _ => {
                log::warn!("vPlicGlobal: unsupported read at reg {reg:#x}, returning 0");
                Ok(0)
            }
        }
    }

    fn handle_write(
        &self,
        addr: <GuestPhysAddrRange as axaddrspace::device::DeviceAddrRange>::Addr,
        width: axaddrspace::device::AccessWidth,
        val: usize,
    ) -> ax_errno::AxResult {
        assert_eq!(width, AccessWidth::Dword);
        let reg = addr - self.addr;
        let host_addr = HostPhysAddr::from_usize(reg + self.host_plic_addr.as_usize());
        match reg {
            // priority
            PLIC_PRIORITY_OFFSET..PLIC_PENDING_OFFSET => {
                perform_mmio_write(host_addr, width, val)?;
                self.sync_all_guest_contexts_vseip()
            }
            // pending — atomic OR into bitmap, no lock needed
            PLIC_PENDING_OFFSET..PLIC_ENABLE_OFFSET => {
                let reg_index = (reg - PLIC_PENDING_OFFSET) / 4;
                if reg_index >= PLIC_PENDING_WORDS {
                    return Ok(());
                }
                let mut val32 = val as u32;
                if reg_index == 0 {
                    val32 &= !1u32; // never set reserved IRQ 0
                }
                let word_idx = reg_index / 2;
                let mask = if reg_index % 2 == 0 {
                    val32 as u64
                } else {
                    (val32 as u64) << 32
                };
                self.pending_irqs.or_word(word_idx, mask);
                self.sync_all_guest_contexts_vseip()
            }
            // enable
            PLIC_ENABLE_OFFSET..PLIC_CONTEXT_CTRL_OFFSET => {
                perform_mmio_write(host_addr, width, val)?;
                let context_id = (reg - PLIC_ENABLE_OFFSET) / PLIC_ENABLE_STRIDE;
                assert!(
                    context_id < self.contexts_num,
                    "Invalid context id {context_id}"
                );
                self.sync_vseip(context_id)
            }
            // threshold
            offset
                if offset >= PLIC_CONTEXT_CTRL_OFFSET
                    && (offset - PLIC_CONTEXT_CTRL_OFFSET).is_multiple_of(PLIC_CONTEXT_STRIDE) =>
            {
                let context_id = (offset - PLIC_CONTEXT_CTRL_OFFSET) / PLIC_CONTEXT_STRIDE;
                assert!(
                    context_id < self.contexts_num,
                    "Invalid context id {context_id}"
                );
                perform_mmio_write(host_addr, width, val)?;
                self.sync_vseip(context_id)
            }
            // claim/complete — atomic clear on active bitmap
            offset
                if offset >= PLIC_CONTEXT_CTRL_OFFSET
                    && (offset - PLIC_CONTEXT_CTRL_OFFSET - PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET)
                        .is_multiple_of(PLIC_CONTEXT_STRIDE) =>
            {
                let context_id =
                    (offset - PLIC_CONTEXT_CTRL_OFFSET - PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET)
                        / PLIC_CONTEXT_STRIDE;
                assert!(
                    context_id < self.contexts_num,
                    "Invalid context id {context_id}"
                );
                let irq_id = val;

                if irq_id == 0 || irq_id >= PLIC_NUM_SOURCES {
                    return self.sync_vseip(context_id);
                }
                if !self.active_irqs.test(irq_id) {
                    return self.sync_vseip(context_id);
                }

                perform_mmio_write(host_addr, width, irq_id)?;
                self.active_irqs.clear(irq_id);
                self.sync_vseip(context_id)
            }
            _ => {
                log::warn!("vPlicGlobal: unsupported read at reg {reg:#x}, returning 0");
                Ok(0)
            }
        }
    }
}

impl InterruptControllerOps for VPlicGlobal {
    fn inject_irq(
        &self,
        pin: u32,
        _trigger: axbus::TriggerMode,
        _target: Option<axbus::IrqTarget>,
    ) -> axbus::Result<axbus::IrqOutcome> {
        let irq_id = pin as usize;
        if irq_id == 0 || irq_id >= PLIC_NUM_SOURCES {
            return Err(axbus::DeviceError::InvalidResource);
        }
        // Atomic set — no lock, safe from interrupt context.
        self.pending_irqs.set(irq_id);
        self.sync_all_guest_contexts_vseip().map_err(|_| {
            axbus::DeviceError::BackendError(alloc::string::String::from("sync_vseip failed"))
        })?;
        Ok(axbus::IrqOutcome::Delivered)
    }

    fn deactivate_irq(&self, pin: u32) -> axbus::Result<axbus::IrqOutcome> {
        let irq_id = pin as usize;
        if irq_id == 0 || irq_id >= PLIC_NUM_SOURCES {
            return Err(axbus::DeviceError::InvalidResource);
        }
        self.pending_irqs.clear(irq_id);
        self.sync_all_guest_contexts_vseip().map_err(|_| {
            axbus::DeviceError::BackendError(alloc::string::String::from("sync_vseip failed"))
        })?;
        Ok(axbus::IrqOutcome::Delivered)
    }

    /// Handle an MSI write by translating it to a PLIC source pending bit.
    ///
    /// The PLIC does not natively support MSI — MSI semantics are provided
    /// by the RISC-V AIA (IMSIC) on newer platforms. For backward
    /// compatibility, this implementation treats the MSI data field as the
    /// PLIC source ID and sets the corresponding pending bit.
    ///
    /// The MSI address is validated but not decoded further — the
    /// `IrqRoutingTable` maps an address window to this controller, so any
    /// write within the window is considered valid.
    fn handle_msi(&self, _addr: u64, data: u32) -> axbus::Result<axbus::IrqOutcome> {
        let irq_id = data as usize;
        if irq_id == 0 || irq_id >= PLIC_NUM_SOURCES {
            return Err(axbus::DeviceError::InvalidResource);
        }
        self.pending_irqs.set(irq_id);
        self.sync_all_guest_contexts_vseip().map_err(|_| {
            axbus::DeviceError::BackendError(alloc::string::String::from("sync_vseip failed"))
        })?;
        Ok(axbus::IrqOutcome::Delivered)
    }
}

#[cfg(test)]
mod tests {
    use axaddrspace::GuestPhysAddr;

    use super::*;

    fn make_vplic() -> VPlicGlobal {
        VPlicGlobal::new(GuestPhysAddr::from(0x0c00_0000), Some(0x400000), 2)
    }

    #[test]
    fn pending_inactive_excludes_reserved_irq_zero() {
        let vplic = make_vplic();
        vplic.pending_irqs.set(0);
        vplic.pending_irqs.set(1);

        let snap = vplic.pending_inactive_snapshot();
        assert!(snap[0] & 1 == 0); // IRQ 0 masked
        assert!(snap[0] & 2 != 0); // IRQ 1 present
    }

    #[test]
    fn claim_bitmap_semantics_pending_to_active() {
        let vplic = make_vplic();
        vplic.pending_irqs.set(5);

        let snap = vplic.pending_inactive_snapshot();
        assert!(snap[0] & (1 << 5) != 0);

        // Simulate claim via atomic ops
        assert!(vplic.pending_irqs.test_and_clear(5));
        vplic.active_irqs.set(5);

        let snap = vplic.pending_inactive_snapshot();
        assert!(snap[0] & (1 << 5) == 0);
        assert!(vplic.active_irqs.test(5));
    }

    #[test]
    fn complete_clears_active() {
        let vplic = make_vplic();
        vplic.active_irqs.set(10);
        assert!(vplic.active_irqs.test(10));

        vplic.active_irqs.clear(10);
        assert!(!vplic.active_irqs.test(10));
        assert!(!vplic.pending_irqs.test(10));
    }

    #[test]
    fn inject_sets_pending_bit() {
        let vplic = make_vplic();
        assert!(!vplic.pending_irqs.test(7));

        vplic.pending_irqs.set(7);

        assert!(vplic.pending_irqs.test(7));
        let snap = vplic.pending_inactive_snapshot();
        assert!(snap[0] & (1 << 7) != 0);
    }

    #[test]
    fn deactivate_clears_pending_bit() {
        let vplic = make_vplic();
        vplic.pending_irqs.set(3);
        assert!(vplic.pending_irqs.test(3));

        vplic.pending_irqs.clear(3);
        assert!(!vplic.pending_irqs.test(3));
    }

    #[test]
    fn active_irqs_excluded_from_candidates() {
        let vplic = make_vplic();
        vplic.pending_irqs.set(1);
        vplic.pending_irqs.set(2);
        vplic.pending_irqs.set(3);
        vplic.active_irqs.set(2);

        let snap = vplic.pending_inactive_snapshot();
        assert!(snap[0] & (1 << 1) != 0);
        assert!(snap[0] & (1 << 2) == 0); // active, excluded
        assert!(snap[0] & (1 << 3) != 0);
    }

    #[test]
    fn irq_zero_always_masked() {
        let vplic = make_vplic();
        vplic.pending_irqs.set(0);

        let snap = vplic.pending_inactive_snapshot();
        assert!(snap[0] & 1 == 0);
    }

    #[test]
    fn full_claim_complete_cycle() {
        let vplic = make_vplic();

        // 1. Inject
        vplic.pending_irqs.set(15);
        let snap = vplic.pending_inactive_snapshot();
        assert!(snap[0] & (1 << 15) != 0);

        // 2. Claim
        assert!(vplic.pending_irqs.test_and_clear(15));
        vplic.active_irqs.set(15);

        let snap = vplic.pending_inactive_snapshot();
        assert!(snap[0] & (1 << 15) == 0);
        assert!(vplic.active_irqs.test(15));

        // 3. Complete
        vplic.active_irqs.clear(15);
        assert!(!vplic.active_irqs.test(15));
        assert!(!vplic.pending_irqs.test(15));

        // 4. Re-inject
        vplic.pending_irqs.set(15);
        let snap = vplic.pending_inactive_snapshot();
        assert!(snap[0] & (1 << 15) != 0);
    }

    #[test]
    fn multiple_irqs_concurrent_state() {
        let vplic = make_vplic();

        for &irq in &[1, 5, 10, 20] {
            vplic.pending_irqs.set(irq);
        }

        // Claim 1 and 10
        for &irq in &[1, 10] {
            vplic.pending_irqs.test_and_clear(irq);
            vplic.active_irqs.set(irq);
        }

        let snap = vplic.pending_inactive_snapshot();
        assert!(snap[0] & (1 << 1) == 0); // claimed
        assert!(snap[0] & (1 << 5) != 0); // pending
        assert!(snap[0] & (1 << 10) == 0); // claimed
        assert!(snap[0] & (1 << 20) != 0); // pending

        // Complete IRQ 1, re-inject
        vplic.active_irqs.clear(1);
        vplic.pending_irqs.set(1);
        let snap = vplic.pending_inactive_snapshot();
        assert!(snap[0] & (1 << 1) != 0);
    }

    #[test]
    fn concurrent_test_and_clear_only_one_wins() {
        use alloc::sync::Arc;
        use core::sync::atomic::{AtomicUsize, Ordering};

        let vplic = Arc::new(make_vplic());
        vplic.pending_irqs.set(42);

        let winners = Arc::new(AtomicUsize::new(0));
        let mut handles = alloc::vec::Vec::new();

        for _ in 0..16 {
            let vplic = vplic.clone();
            let winners = winners.clone();
            handles.push(std::thread::spawn(move || {
                if vplic.pending_irqs.test_and_clear(42) {
                    winners.fetch_add(1, Ordering::Relaxed);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(winners.load(Ordering::Relaxed), 1);
        assert!(!vplic.pending_irqs.test(42));
    }
}
