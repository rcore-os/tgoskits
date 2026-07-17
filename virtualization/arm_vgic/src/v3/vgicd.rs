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

use ax_kspin::SpinNoIrq;
use axdevice_base::{AccessWidth, BaseDeviceOps, DeviceResult, EmuDeviceType};
use axvm_types::{GuestPhysAddr, GuestPhysAddrRange, HostPhysAddr};
use bitmaps::Bitmap;
use log::debug;

use super::{
    registers::*,
    utils::{perform_mmio_read, perform_mmio_write},
};
use crate::{VgicError, VgicResult};

/// Default size for GICD region.
pub const DEFAULT_GICD_SIZE: usize = 0x10000; // 64K

#[derive(Clone, Copy)]
struct SpiRevocationBatch {
    generation: u64,
    irqs: Bitmap<{ MAX_IRQ_V3 }>,
}

struct IrqOwnership {
    assigned: Bitmap<{ MAX_IRQ_V3 }>,
    assigning: Bitmap<{ MAX_IRQ_V3 }>,
    revocation: Option<SpiRevocationBatch>,
    next_generation: u64,
}

impl IrqOwnership {
    fn new() -> Self {
        Self {
            assigned: Bitmap::new(),
            assigning: Bitmap::new(),
            revocation: None,
            next_generation: 1,
        }
    }

    fn begin_revocation(&mut self) -> VgicResult<SpiRevocationBatch> {
        if !self.assigning.is_empty() {
            return Err(VgicError::Busy {
                operation: "begin VGIC SPI revocation while assignment is in progress",
            });
        }
        if let Some(batch) = self.revocation {
            return Ok(batch);
        }

        let batch = SpiRevocationBatch {
            generation: self.next_generation,
            irqs: self.assigned,
        };
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        self.revocation = Some(batch);
        Ok(batch)
    }

    fn finish_revocation(&mut self, batch: SpiRevocationBatch) -> VgicResult<usize> {
        let Some(active) = self.revocation else {
            return Err(VgicError::StaleRevocation {
                generation: batch.generation,
                active_generation: 0,
            });
        };
        if active.generation != batch.generation {
            return Err(VgicError::StaleRevocation {
                generation: batch.generation,
                active_generation: active.generation,
            });
        }

        let mut released = 0;
        for irq in &batch.irqs {
            self.assigned.set(irq, false);
            released += 1;
        }
        self.revocation = None;
        Ok(released)
    }

    fn is_guest_visible(&self, irq: usize) -> bool {
        self.assigned.get(irq) && self.revocation.is_none_or(|batch| !batch.irqs.get(irq))
    }
}

trait PhysicalSpiControl {
    fn begin_spi_quiesce(&self, irq: u32) -> VgicResult;

    fn poll_distributor_write_complete(&self) -> VgicResult<bool>;
}

struct HostPhysicalSpiControl;

impl PhysicalSpiControl for HostPhysicalSpiControl {
    fn begin_spi_quiesce(&self, irq: u32) -> VgicResult {
        crate::api_reexp::begin_physical_spi_quiesce(irq)
    }

    fn poll_distributor_write_complete(&self) -> VgicResult<bool> {
        crate::api_reexp::poll_physical_distributor_write_complete()
    }
}

/// An in-progress physical SPI ownership revocation.
///
/// Dropping this token does not restore guest access. The distributor retains
/// its fail-closed revocation generation so a later call can retry it.
#[must_use = "SPI ownership remains fail-closed until revocation completes"]
pub struct SpiRevocation<'a> {
    distributor: &'a VGicD,
    batch: SpiRevocationBatch,
}

/// Result of one non-blocking distributor write-completion observation.
pub enum SpiRevocationPoll<'a> {
    /// GICv3 still reports GICD_CTLR.RWP; poll again from task context.
    Pending(SpiRevocation<'a>),
    /// Every physical SPI is drained and its VGIC ownership is released.
    Complete(SpisRevoked),
}

/// Proof that a VGIC distributor no longer owns its assigned physical SPIs.
#[must_use]
pub struct SpisRevoked {
    released_spi_count: usize,
}

impl SpisRevoked {
    /// Returns how many physical SPI ownership records were released.
    pub const fn released_spi_count(&self) -> usize {
        self.released_spi_count
    }
}

impl<'a> SpiRevocation<'a> {
    /// Polls host distributor write completion exactly once.
    pub fn poll(self) -> VgicResult<SpiRevocationPoll<'a>> {
        self.poll_with(&HostPhysicalSpiControl)
    }

    fn poll_with(self, control: &impl PhysicalSpiControl) -> VgicResult<SpiRevocationPoll<'a>> {
        if !self.batch.irqs.is_empty() && !control.poll_distributor_write_complete()? {
            return Ok(SpiRevocationPoll::Pending(self));
        }

        let released_spi_count = self
            .distributor
            .irq_ownership
            .lock()
            .finish_revocation(self.batch)?;
        Ok(SpiRevocationPoll::Complete(SpisRevoked {
            released_spi_count,
        }))
    }
}

/// Virtual Generic Interrupt Controller (VGIC) Distributor (D) implementation.
///
/// For GIC version 3.
pub struct VGicD {
    /// The address of the VGicD in the guest physical address space.
    pub addr: GuestPhysAddr,
    /// The size of the VGicD in bytes.
    pub size: usize,

    /// IRQ assignment and fail-closed revocation state.
    irq_ownership: SpinNoIrq<IrqOwnership>,

    /// The host physical address of the VGicD.
    ///
    /// TODO: move host gicd access to a separate crate, maybe arm_gic_driver.
    pub host_gicd_addr: HostPhysAddr,
}

impl VGicD {
    /// Validates that an IRQ identifier can be represented by the VGIC.
    pub fn validate_irq(irq: u32) -> VgicResult {
        if irq >= MAX_IRQ_V3 as u32 {
            return Err(VgicError::InvalidIrq {
                irq: irq as usize,
                max: MAX_IRQ_V3,
            });
        }
        Ok(())
    }

    /// Creates a new VGicD instance.
    pub fn new(addr: GuestPhysAddr, size: Option<usize>) -> Self {
        let size = size.unwrap_or(DEFAULT_GICD_SIZE);

        Self {
            addr,
            size,
            irq_ownership: SpinNoIrq::new(IrqOwnership::new()),
            host_gicd_addr: crate::api_reexp::get_host_gicd_base(),
        }
    }

    /// Assigns an IRQ to a specific CPU.
    pub fn assign_irq(
        &self,
        irq: u32,
        cpu_phys_id: usize,
        target_cpu_affinity: (u8, u8, u8, u8),
    ) -> VgicResult {
        debug!(
            "Physically assigning IRQ {irq} to CPU {cpu_phys_id} with affinity \
             {target_cpu_affinity:?}"
        );

        Self::validate_irq(irq)?;
        if !self.is_irq_spi(irq) {
            return Err(VgicError::NotSpi { irq: irq as usize });
        }

        {
            let mut ownership = self.irq_ownership.lock();
            if ownership.revocation.is_some() || ownership.assigning.get(irq as usize) {
                return Err(VgicError::Busy {
                    operation: "assign physical SPI to VGIC",
                });
            }
            ownership.assigning.set(irq as usize, true);
        }

        let route_result =
            crate::api_reexp::route_physical_spi(irq, cpu_phys_id, target_cpu_affinity);
        let mut ownership = self.irq_ownership.lock();
        ownership.assigning.set(irq as usize, false);
        route_result?;
        ownership.assigned.set(irq as usize, true);
        Ok(())
    }

    /// Begins fail-closed revocation of every physical SPI assigned here.
    ///
    /// Guest MMIO visibility is removed before any physical distributor write
    /// is issued. A backend failure retains the revocation generation and all
    /// ownership bits so callers can retry without fabricating a release. The
    /// host must stop and join every vCPU before calling this method; the VGIC
    /// cannot prove that OS-level lifecycle condition itself.
    pub fn begin_assigned_spi_revocation(&self) -> VgicResult<SpiRevocation<'_>> {
        self.begin_assigned_spi_revocation_with(&HostPhysicalSpiControl)
    }

    fn begin_assigned_spi_revocation_with(
        &self,
        control: &impl PhysicalSpiControl,
    ) -> VgicResult<SpiRevocation<'_>> {
        let batch = self.irq_ownership.lock().begin_revocation()?;
        for irq in &batch.irqs {
            control.begin_spi_quiesce(irq as u32)?;
        }
        Ok(SpiRevocation {
            distributor: self,
            batch,
        })
    }
}

impl BaseDeviceOps<GuestPhysAddrRange> for VGicD {
    fn emu_type(&self) -> axdevice_base::EmuDeviceType {
        EmuDeviceType::GPPTDistributor
    }

    fn address_range(&self) -> GuestPhysAddrRange {
        GuestPhysAddrRange::from_start_size(self.addr, self.size)
    }

    fn handle_read(
        &self,
        addr: <GuestPhysAddrRange as axdevice_base::DeviceAddrRange>::Addr,
        width: AccessWidth,
    ) -> DeviceResult<usize> {
        let gicd_base = self.host_gicd_addr;
        let reg = addr - self.addr;

        debug!("vGICD read reg {reg:#x} width {width:?}");

        let result = match reg {
            reg if GICD_IROUTER_RANGE.contains(&reg) => {
                let irq = (reg - GICD_IROUTER) as u32 / 8;

                if self.is_irq_guest_visible(irq) && self.is_irq_spi(irq) {
                    perform_mmio_read(gicd_base + reg, width)
                } else {
                    // If the IRQ is not assigned, return 0
                    Ok(0)
                }
            }
            reg if GICD_ITARGETSR_RANGE.contains(&reg) => {
                let irq = (reg - GICD_ITARGETSR) as u32;

                if self.is_irq_guest_visible(irq) && self.is_irq_spi(irq) {
                    perform_mmio_read(gicd_base + reg, width)
                } else {
                    // If the IRQ is not assigned, return 0
                    Ok(0)
                }
            }
            reg if GICD_ICENABLER_RANGE.contains(&reg)
                || GICD_ISENABLER_RANGE.contains(&reg)
                || GICD_ICPENDR_RANGE.contains(&reg)
                || GICD_ISPENDR_RANGE.contains(&reg)
                || GICD_ICACTIVER_RANGE.contains(&reg)
                || GICD_ISACTIVER_RANGE.contains(&reg) =>
            {
                self.irq_masked_read(reg, reg & 0x7f, 0, width, true)
            }
            reg if GICD_IGROUPR_RANGE.contains(&reg) => {
                self.irq_masked_read(reg, reg & 0x7f, 0, width, false)
            }
            reg if GICD_IGRPMODR_RANGE.contains(&reg) => {
                self.irq_masked_read(reg, reg & 0x7f, 0, width, false)
            }
            reg if GICD_ICFGR_RANGE.contains(&reg) => {
                self.irq_masked_read(reg, reg & 0xff, 1, width, false)
            }
            reg if GICD_IPRIORITYR_RANGE.contains(&reg) => {
                self.irq_masked_read(reg, reg & 0x3ff, 3, width, false)
            }
            reg if GICDV3_PIDR0_RANGE.contains(&reg)
                || GICDV3_PIDR4_RANGE.contains(&reg)
                || GICDV3_CIDR0_RANGE.contains(&reg)
                || reg == GICD_CTLR
                || reg == GICD_TYPER
                || reg == GICD_IIDR
                || reg == GICD_TYPER2 =>
            {
                // read-only
                // ignore write
                perform_mmio_read(gicd_base + reg, width)
            }
            _ => {
                todo!("vgicdv3 read unimplemented for reg {:#x}", reg);
            }
        };
        Ok(result?)
    }

    fn handle_write(
        &self,
        addr: <GuestPhysAddrRange as axdevice_base::DeviceAddrRange>::Addr,
        width: AccessWidth,
        val: usize,
    ) -> DeviceResult {
        let gicd_base = self.host_gicd_addr;
        let reg = addr - self.addr;

        debug!("vGICD write reg {reg:#x} width {width:?} val {val:#x}");

        let result = match reg {
            reg if GICD_IROUTER_RANGE.contains(&reg) => {
                let irq = (reg - GICD_IROUTER) as u32 / 8;

                if self.is_irq_guest_visible(irq) && self.is_irq_spi(irq) {
                    perform_mmio_write(gicd_base + reg, width, val)
                } else {
                    // If the IRQ is not assigned, ignore the write
                    Ok(())
                }
            }
            reg if GICD_ITARGETSR_RANGE.contains(&reg) => {
                let irq = (reg - GICD_ITARGETSR) as u32; // it was wrong in hVisor

                if self.is_irq_guest_visible(irq) && self.is_irq_spi(irq) {
                    perform_mmio_write(gicd_base + reg, width, val)
                } else {
                    // If the IRQ is not assigned, ignore the write
                    Ok(())
                }
            }
            reg if GICD_ICENABLER_RANGE.contains(&reg)
                || GICD_ISENABLER_RANGE.contains(&reg)
                || GICD_ICPENDR_RANGE.contains(&reg)
                || GICD_ISPENDR_RANGE.contains(&reg)
                || GICD_ICACTIVER_RANGE.contains(&reg)
                || GICD_ISACTIVER_RANGE.contains(&reg) =>
            {
                self.irq_masked_write(reg, reg & 0x7f, 0, width, true, val)
            }
            reg if GICD_IGROUPR_RANGE.contains(&reg) => {
                self.irq_masked_write(reg, reg & 0x7f, 0, width, false, val)
            }
            reg if GICD_IGRPMODR_RANGE.contains(&reg) => {
                self.irq_masked_write(reg, reg & 0x7f, 0, width, false, val)
            }
            reg if GICD_ICFGR_RANGE.contains(&reg) => {
                self.irq_masked_write(reg, reg & 0xff, 1, width, false, val)
            }
            reg if GICD_IPRIORITYR_RANGE.contains(&reg) => {
                self.irq_masked_write(reg, reg & 0x3ff, 3, width, false, val)
            }
            reg if GICDV3_PIDR0_RANGE.contains(&reg)
                || GICDV3_PIDR4_RANGE.contains(&reg)
                || GICDV3_CIDR0_RANGE.contains(&reg)
                || reg == GICD_CTLR
                || reg == GICD_TYPER
                || reg == GICD_IIDR
                || reg == GICD_TYPER2 =>
            {
                // read-only
                // ignore write
                Ok(())
            }
            _ => {
                todo!("vgicdv3 write unimplemented for reg {:#x}", reg);
            }
        };
        Ok(result?)
    }
}

impl VGicD {
    /// Checks if an IRQ is assigned to this VGicD.
    pub fn is_irq_assigned(&self, irq: u32) -> bool {
        Self::validate_irq(irq).is_ok() && self.irq_ownership.lock().assigned.get(irq as usize)
    }

    /// Checks whether guest MMIO may still access an assigned IRQ.
    ///
    /// Revoking SPIs remain owned until physical synchronization completes,
    /// but become invisible to the guest at the start of revocation.
    pub fn is_irq_guest_visible(&self, irq: u32) -> bool {
        Self::validate_irq(irq).is_ok() && self.irq_ownership.lock().is_guest_visible(irq as usize)
    }

    /// Checks if an IRQ is a Software Generated Interrupt (SGI).
    pub fn is_irq_sgi(&self, irq: u32) -> bool {
        // Check if the IRQ is a Software Generated Interrupt (SGI)
        irq < 16
    }

    /// Checks if an IRQ is a Shared Peripheral Interrupt (SPI).
    pub fn is_irq_spi(&self, irq: u32) -> bool {
        // Check if the IRQ is a Shared Peripheral Interrupt (SPI)
        (32..1020).contains(&irq)
    }

    /// Returns the mask of bits for the irqs assigned to this VGicD, in a bit-field reg.
    pub fn irq_access_mask(
        &self,
        reg_offset: usize,
        bits_per_irq_shift: usize,
        width: AccessWidth,
    ) -> usize {
        if bits_per_irq_shift > 3 {
            panic!(
                "bits_per_irq_shift must be <= 3, got {}",
                bits_per_irq_shift
            );
        }

        // How many IRQs there are in the mmio region the access width covers?
        let irqs_in_access_width = width.size() << (3 - bits_per_irq_shift);
        // The first IRQ at the given register offset.
        let first_irq = reg_offset << (3 - bits_per_irq_shift);
        // The mask of a single IRQ in the bit-field register.
        let single_irq_mask = (1 << (bits_per_irq_shift + 1)) - 1;

        let mut mask = 0;
        for irq in 0..irqs_in_access_width {
            if self.is_irq_guest_visible((first_irq + irq) as _) {
                // If the IRQ is assigned, set the corresponding bits in the mask.
                mask |= single_irq_mask << (irq << bits_per_irq_shift);
            }
        }

        mask
    }

    /// Performs masked read access to GICD registers.
    pub fn irq_masked_read(
        &self,
        offset: usize,
        reg_offset: usize,
        bits_per_irq_shift: usize,
        width: AccessWidth,
        _is_poke: bool,
    ) -> VgicResult<usize> {
        let mask = self.irq_access_mask(reg_offset, bits_per_irq_shift, width);

        Ok(perform_mmio_read(self.host_gicd_addr + offset, width)? & mask)
    }

    /// Performs masked write access to GICD registers.
    pub fn irq_masked_write(
        &self,
        offset: usize,
        reg_offset: usize,
        bits_per_irq_shift: usize,
        width: AccessWidth,
        is_poke: bool,
        val: usize,
    ) -> VgicResult<()> {
        let mask = self.irq_access_mask(reg_offset, bits_per_irq_shift, width);

        if is_poke {
            perform_mmio_write(self.host_gicd_addr + offset, width, val & mask)
        } else {
            let _lock = GICD_LOCK.lock();

            let current_value = perform_mmio_read(self.host_gicd_addr + offset, width)?;
            let new_value = (current_value & !mask) | (val & mask);
            perform_mmio_write(self.host_gicd_addr + offset, width, new_value)
        }
    }
}

// Todo: move this lock to arceos or axvisor
static GICD_LOCK: ax_kspin::SpinNoIrq<()> = ax_kspin::SpinNoIrq::new(());

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use ax_kspin_test_runtime as _;

    use super::*;

    struct TestSpiControl {
        quiesced_irq: Cell<Option<u32>>,
        poll_count: Cell<usize>,
        fail_quiesce: bool,
    }

    impl TestSpiControl {
        const fn pending_then_complete() -> Self {
            Self {
                quiesced_irq: Cell::new(None),
                poll_count: Cell::new(0),
                fail_quiesce: false,
            }
        }
    }

    impl PhysicalSpiControl for TestSpiControl {
        fn begin_spi_quiesce(&self, irq: u32) -> VgicResult {
            self.quiesced_irq.set(Some(irq));
            if self.fail_quiesce {
                return Err(VgicError::Backend {
                    operation: "quiesce physical SPI",
                    detail: "injected failure".into(),
                });
            }
            Ok(())
        }

        fn poll_distributor_write_complete(&self) -> VgicResult<bool> {
            let poll_count = self.poll_count.get();
            self.poll_count.set(poll_count + 1);
            Ok(poll_count != 0)
        }
    }

    fn distributor_owning(irq: u32) -> VGicD {
        let distributor = VGicD::new(GuestPhysAddr::from(0), None);
        distributor
            .irq_ownership
            .lock()
            .assigned
            .set(irq as usize, true);
        distributor
    }

    #[test]
    fn empty_revocation_completes_without_waiting_for_unrelated_gic_writes() {
        let distributor = VGicD::new(GuestPhysAddr::from(0), None);
        let control = TestSpiControl::pending_then_complete();
        let revocation = distributor
            .begin_assigned_spi_revocation_with(&control)
            .unwrap();

        let SpiRevocationPoll::Complete(proof) = revocation.poll_with(&control).unwrap() else {
            panic!("an empty ownership set has no physical writes to drain")
        };
        assert_eq!(proof.released_spi_count(), 0);
        assert_eq!(control.poll_count.get(), 0);
    }

    #[test]
    fn revocation_hides_spi_before_physical_drain_and_releases_after_sync() {
        let irq = 40;
        let distributor = distributor_owning(irq);
        let control = TestSpiControl::pending_then_complete();

        let revocation = distributor
            .begin_assigned_spi_revocation_with(&control)
            .unwrap();
        assert_eq!(control.quiesced_irq.get(), Some(irq));
        assert!(distributor.is_irq_assigned(irq));
        assert!(!distributor.is_irq_guest_visible(irq));

        let revocation = match revocation.poll_with(&control).unwrap() {
            SpiRevocationPoll::Pending(revocation) => revocation,
            SpiRevocationPoll::Complete(_) => panic!("RWP must retain ownership while pending"),
        };
        assert!(distributor.is_irq_assigned(irq));

        let proof = match revocation.poll_with(&control).unwrap() {
            SpiRevocationPoll::Complete(proof) => proof,
            SpiRevocationPoll::Pending(_) => panic!("second poll must observe completion"),
        };
        assert_eq!(proof.released_spi_count(), 1);
        assert!(!distributor.is_irq_assigned(irq));
    }

    #[test]
    fn failed_physical_quiesce_retains_fail_closed_ownership_for_retry() {
        let irq = 63;
        let distributor = distributor_owning(irq);
        let failing = TestSpiControl {
            fail_quiesce: true,
            ..TestSpiControl::pending_then_complete()
        };

        assert!(
            distributor
                .begin_assigned_spi_revocation_with(&failing)
                .is_err()
        );
        assert!(distributor.is_irq_assigned(irq));
        assert!(!distributor.is_irq_guest_visible(irq));

        let retry = TestSpiControl {
            poll_count: Cell::new(1),
            ..TestSpiControl::pending_then_complete()
        };
        let revocation = distributor
            .begin_assigned_spi_revocation_with(&retry)
            .unwrap();
        assert!(matches!(
            revocation.poll_with(&retry).unwrap(),
            SpiRevocationPoll::Complete(_)
        ));
        assert!(!distributor.is_irq_assigned(irq));
    }

    #[test]
    fn dropped_pending_poll_token_keeps_the_same_revocation_retryable() {
        let irq = 72;
        let distributor = distributor_owning(irq);
        let pending = TestSpiControl::pending_then_complete();
        let revocation = distributor
            .begin_assigned_spi_revocation_with(&pending)
            .unwrap();

        let SpiRevocationPoll::Pending(revocation) = revocation.poll_with(&pending).unwrap() else {
            panic!("first poll must remain pending")
        };
        drop(revocation);
        assert!(distributor.is_irq_assigned(irq));
        assert!(!distributor.is_irq_guest_visible(irq));

        let retry = TestSpiControl {
            poll_count: Cell::new(1),
            ..TestSpiControl::pending_then_complete()
        };
        let revocation = distributor
            .begin_assigned_spi_revocation_with(&retry)
            .unwrap();
        assert!(matches!(
            revocation.poll_with(&retry).unwrap(),
            SpiRevocationPoll::Complete(_)
        ));
        assert!(!distributor.is_irq_assigned(irq));
    }
}
