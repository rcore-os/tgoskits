//! GICv2 register frontends over the shared VM-local controller state.

use alloc::{sync::Arc, vec::Vec};

use axvm_types::AccessWidth;

use super::{GicV3Controller, GicV3VcpuWake, state::DeliveryRetirement};
use crate::{
    GicVcpuId, IntId, InterruptState, RegisterRegion, SgiId, SgiTarget, SpiId, VgicError,
    VgicResult, backend_result,
    register::{
        GICC_ABPR, GICC_AEOIR, GICC_AHPPIR, GICC_AIAR, GICC_APR, GICC_BPR, GICC_CTLR, GICC_DIR,
        GICC_EOIR, GICC_HPPIR, GICC_IAR, GICC_IIDR, GICC_PMR, GICC_RPR, GICD_CPENDSGIR, GICD_CTLR,
        GICD_ICACTIVER, GICD_ICENABLER, GICD_ICFGR, GICD_ICPENDR, GICD_IGROUPR, GICD_IIDR,
        GICD_IPRIORITYR, GICD_ISACTIVER, GICD_ISENABLER, GICD_ISPENDR, GICD_ITARGETSR, GICD_SGIR,
        GICD_SPENDSGIR, GICD_TYPER,
    },
};

const GIC_SPURIOUS_INTID: u64 = 1023;
const GICV2_MAX_INTIDS: u64 = 1020;

impl GicV3Controller {
    /// Reads the GICv2 Distributor view for one accessing vCPU.
    pub fn read_v2_distributor(
        &self,
        vcpu: GicVcpuId,
        offset: u64,
        width: AccessWidth,
    ) -> VgicResult<u64> {
        validate_frame_access(
            RegisterRegion::Distributor,
            self.inner.config.distributor_size(),
            offset,
            width,
            "read",
        )?;
        match offset {
            GICD_CTLR => {
                require_width(RegisterRegion::Distributor, offset, width, "read")?;
                Ok(u64::from(self.inner.state.lock().distributor.enabled()))
            }
            GICD_TYPER => {
                require_width(RegisterRegion::Distributor, offset, width, "read")?;
                let intids = u64::from(self.inner.config.spi_limit()).div_ceil(32);
                let cpus = self.inner.config.vcpu_count().saturating_sub(1) as u64;
                Ok(intids.saturating_sub(1) | (cpus.min(7) << 5))
            }
            GICD_IIDR => {
                require_width(RegisterRegion::Distributor, offset, width, "read")?;
                Ok(0x0002_043b)
            }
            _ if is_private_register(offset) => self
                .inner
                .state
                .lock()
                .redistributor(vcpu, "read GICv2 private Distributor register")?
                .read_private_register(offset, width, &self.inner.config),
            _ if (GICD_ITARGETSR..GICD_ITARGETSR + GICV2_MAX_INTIDS).contains(&offset) => {
                self.read_v2_targets(vcpu, offset, width)
            }
            _ if (GICD_CPENDSGIR..GICD_CPENDSGIR + 16).contains(&offset)
                || (GICD_SPENDSGIR..GICD_SPENDSGIR + 16).contains(&offset) =>
            {
                self.read_v2_sgi_sources(vcpu, offset, width)
            }
            GICD_SGIR => {
                require_width(RegisterRegion::Distributor, offset, width, "read")?;
                Ok(0)
            }
            _ => self.read_distributor(offset, width),
        }
    }

    /// Writes the GICv2 Distributor view and performs wakeups outside its lock.
    pub fn write_v2_distributor(
        &self,
        vcpu: GicVcpuId,
        offset: u64,
        width: AccessWidth,
        value: u64,
    ) -> VgicResult {
        validate_frame_access(
            RegisterRegion::Distributor,
            self.inner.config.distributor_size(),
            offset,
            width,
            "write",
        )?;
        match offset {
            GICD_CTLR => {
                require_width(RegisterRegion::Distributor, offset, width, "write")?;
                self.write_distributor(
                    GICD_CTLR,
                    AccessWidth::Dword,
                    u64::from(value & 1 != 0) << 1,
                )
            }
            GICD_TYPER | GICD_IIDR => {
                require_width(RegisterRegion::Distributor, offset, width, "write")
            }
            _ if is_private_register(offset) => {
                let wakes = {
                    let mut state = self.inner.state.lock();
                    let candidates = state
                        .redistributor_mut(vcpu, "write GICv2 private Distributor register")?
                        .write_private_register(offset, width, value, &self.inner.config)?;
                    let mut wakes = Vec::new();
                    for intid in candidates {
                        if let Some(wake) = state.queue_local_if_deliverable(vcpu, intid)? {
                            wakes.push(wake);
                        }
                    }
                    wakes
                };
                wake_all(wakes)
            }
            _ if (GICD_ITARGETSR..GICD_ITARGETSR + GICV2_MAX_INTIDS).contains(&offset) => {
                self.write_v2_targets(offset, width, value)
            }
            GICD_SGIR => {
                require_width(RegisterRegion::Distributor, offset, width, "write")?;
                self.write_v2_sgir(vcpu, value as u32)
            }
            _ if (GICD_CPENDSGIR..GICD_CPENDSGIR + 16).contains(&offset) => {
                self.write_v2_sgi_sources(vcpu, offset, width, value, false)
            }
            _ if (GICD_SPENDSGIR..GICD_SPENDSGIR + 16).contains(&offset) => {
                self.write_v2_sgi_sources(vcpu, offset, width, value, true)
            }
            _ => self.write_distributor(offset, width, value),
        }
    }

    /// Reads the GICv2 memory-mapped CPU interface.
    pub fn read_v2_cpu_interface(
        &self,
        vcpu: GicVcpuId,
        offset: u64,
        width: AccessWidth,
    ) -> VgicResult<u64> {
        validate_frame_access(RegisterRegion::CpuInterface, 0x2_000, offset, width, "read")?;
        require_width(RegisterRegion::CpuInterface, offset, width, "read")?;
        match offset {
            GICC_IAR | GICC_AIAR => self.acknowledge_v2(vcpu),
            GICC_HPPIR | GICC_AHPPIR => self.highest_pending_v2(vcpu),
            GICC_CTLR => Ok(u64::from(
                self.cpu_interface(vcpu, "read GICC_CTLR")?.v2_control(),
            )),
            GICC_PMR => Ok(u64::from(
                self.cpu_interface(vcpu, "read GICC_PMR")?
                    .v2_priority_mask()
                    .raw(),
            )),
            GICC_BPR | GICC_ABPR => Ok(u64::from(
                self.cpu_interface(vcpu, "read GICC_BPR")?.v2_binary_point(),
            )),
            GICC_RPR => Ok(u64::from(
                self.cpu_interface(vcpu, "read GICC_RPR")?
                    .v2_running_priority()
                    .raw(),
            )),
            GICC_APR => Ok(0),
            GICC_IIDR => Ok(0x0002_043b),
            _ => Ok(0),
        }
    }

    /// Writes the GICv2 memory-mapped CPU interface.
    pub fn write_v2_cpu_interface(
        &self,
        vcpu: GicVcpuId,
        offset: u64,
        width: AccessWidth,
        value: u64,
    ) -> VgicResult {
        validate_frame_access(
            RegisterRegion::CpuInterface,
            0x2_000,
            offset,
            width,
            "write",
        )?;
        require_width(RegisterRegion::CpuInterface, offset, width, "write")?;
        match offset {
            GICC_CTLR => {
                self.inner
                    .state
                    .lock()
                    .redistributor_mut(vcpu, "write GICC_CTLR")?
                    .cpu_interface_mut()
                    .set_v2_control(value as u32);
                Ok(())
            }
            GICC_PMR => {
                self.inner
                    .state
                    .lock()
                    .redistributor_mut(vcpu, "write GICC_PMR")?
                    .cpu_interface_mut()
                    .set_v2_priority_mask(value as u8);
                Ok(())
            }
            GICC_BPR | GICC_ABPR => {
                self.inner
                    .state
                    .lock()
                    .redistributor_mut(vcpu, "write GICC_BPR")?
                    .cpu_interface_mut()
                    .set_v2_binary_point(value as u8);
                Ok(())
            }
            GICC_EOIR | GICC_AEOIR => self.eoi_v2(vcpu, value),
            GICC_DIR => self.dir_v2(vcpu, value),
            GICC_IAR | GICC_AIAR | GICC_RPR | GICC_HPPIR | GICC_AHPPIR | GICC_APR | GICC_IIDR => {
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn read_v2_targets(&self, vcpu: GicVcpuId, offset: u64, width: AccessWidth) -> VgicResult<u64> {
        validate_byte_array_access(
            RegisterRegion::Distributor,
            offset,
            width,
            GICD_ITARGETSR,
            GICV2_MAX_INTIDS,
            "read",
        )?;
        let state = self.inner.state.lock();
        let mut value = 0;
        for byte in 0..width.size() {
            let raw = (offset - GICD_ITARGETSR) as u32 + byte as u32;
            let mask = if raw < 32 {
                if vcpu.raw() < 8 { 1 << vcpu.raw() } else { 0 }
            } else if raw < self.inner.config.spi_limit() {
                state.distributor.cpu_target_mask(SpiId::new(raw)?)? as usize
            } else {
                0
            };
            value |= (mask as u64) << (byte * 8);
        }
        Ok(value)
    }

    fn write_v2_targets(&self, offset: u64, width: AccessWidth, value: u64) -> VgicResult {
        validate_byte_array_access(
            RegisterRegion::Distributor,
            offset,
            width,
            GICD_ITARGETSR,
            GICV2_MAX_INTIDS,
            "write",
        )?;
        let wakes = {
            let mut state = self.inner.state.lock();
            let mut wakes = Vec::new();
            for byte in 0..width.size() {
                let raw = (offset - GICD_ITARGETSR) as u32 + byte as u32;
                if raw < 32 || raw >= self.inner.config.spi_limit() {
                    continue;
                }
                let spi = SpiId::new(raw)?;
                let valid_cpu_bits = if self.inner.config.vcpu_count() >= 8 {
                    u8::MAX
                } else {
                    (1u8 << self.inner.config.vcpu_count()) - 1
                };
                let mask = ((value >> (byte * 8)) as u8) & valid_cpu_bits;
                state.distributor.set_cpu_target_mask(spi, mask)?;
                if state.has_software_backing(spi, &self.inner.config)
                    && let Some(wake) = state.queue_spi_if_deliverable(spi)?
                {
                    wakes.push(wake);
                }
            }
            wakes
        };
        wake_all(wakes)
    }

    fn write_v2_sgir(&self, source: GicVcpuId, value: u32) -> VgicResult {
        let sgi = SgiId::new((value & 0xf) as u8)?;
        let targets = match (value >> 24) & 0b11 {
            0 => {
                let mask = ((value >> 16) & 0xff) as u8;
                let state = self.inner.state.lock();
                let affinities = state
                    .redistributors
                    .iter()
                    .filter(|(vcpu, _)| vcpu.raw() < 8 && mask & (1 << vcpu.raw()) != 0)
                    .map(|(_, redistributor)| redistributor.affinity())
                    .collect();
                SgiTarget::Affinities(affinities)
            }
            1 => SgiTarget::AllExceptSelf,
            2 => SgiTarget::SelfOnly,
            _ => return Ok(()),
        };
        self.send_sgi(source, sgi, targets)
    }

    fn read_v2_sgi_sources(
        &self,
        vcpu: GicVcpuId,
        offset: u64,
        width: AccessWidth,
    ) -> VgicResult<u64> {
        let bank = if offset >= GICD_SPENDSGIR {
            GICD_SPENDSGIR
        } else {
            GICD_CPENDSGIR
        };
        validate_byte_array_access(RegisterRegion::Distributor, offset, width, bank, 16, "read")?;
        let state = self.inner.state.lock();
        let redistributor = state.redistributor(vcpu, "read SGI source-pending register")?;
        let mut value = 0;
        for byte in 0..width.size() {
            let sgi = SgiId::new((offset - bank) as u8 + byte as u8)?;
            value |= u64::from(redistributor.sgi_sources(sgi)) << (byte * 8);
        }
        Ok(value)
    }

    fn write_v2_sgi_sources(
        &self,
        vcpu: GicVcpuId,
        offset: u64,
        width: AccessWidth,
        value: u64,
        set: bool,
    ) -> VgicResult {
        let bank = if set { GICD_SPENDSGIR } else { GICD_CPENDSGIR };
        validate_byte_array_access(
            RegisterRegion::Distributor,
            offset,
            width,
            bank,
            16,
            "write",
        )?;
        let wakes = {
            let mut state = self.inner.state.lock();
            let mut wakes = Vec::new();
            for byte in 0..width.size() {
                let sgi = SgiId::new((offset - bank) as u8 + byte as u8)?;
                let mask = (value >> (byte * 8)) as u8;
                if set {
                    for source in 0..8usize {
                        if mask & (1 << source) != 0 {
                            state
                                .redistributor_mut(vcpu, "set SGI source-pending register")?
                                .pend_sgi(GicVcpuId::new(source), sgi);
                        }
                    }
                    if let Some(wake) = state.queue_local_if_deliverable(vcpu, IntId::Sgi(sgi))? {
                        wakes.push(wake);
                    }
                } else {
                    state
                        .redistributor_mut(vcpu, "clear SGI source-pending register")?
                        .clear_sgi_sources(sgi, mask);
                }
            }
            wakes
        };
        wake_all(wakes)
    }

    fn highest_pending_v2(&self, vcpu: GicVcpuId) -> VgicResult<u64> {
        let state = self.inner.state.lock();
        let cpu = state
            .redistributor(vcpu, "read GICC_HPPIR")?
            .cpu_interface();
        if !state.distributor.enabled() || !cpu.v2_enabled() {
            return Ok(GIC_SPURIOUS_INTID);
        }
        let pending = state
            .redistributor(vcpu, "read GICC_HPPIR")?
            .highest_pending(cpu.v2_priority_mask(), |spi| {
                Ok(state.distributor.interrupt(spi)?.priority())
            })?;
        Ok(pending.map_or(GIC_SPURIOUS_INTID, |(intid, _)| u64::from(intid.raw())))
    }

    fn acknowledge_v2(&self, vcpu: GicVcpuId) -> VgicResult<u64> {
        let mut state = self.inner.state.lock();
        if !state.distributor.enabled() {
            return Ok(GIC_SPURIOUS_INTID);
        }
        let (enabled, priority_mask) = {
            let cpu = state.redistributor(vcpu, "read GICC_IAR")?.cpu_interface();
            (cpu.v2_enabled(), cpu.v2_priority_mask())
        };
        if !enabled {
            return Ok(GIC_SPURIOUS_INTID);
        }
        let Some((intid, priority)) = state
            .redistributor(vcpu, "read GICC_IAR")?
            .highest_pending(priority_mask, |spi| {
                Ok(state.distributor.interrupt(spi)?.priority())
            })?
        else {
            return Ok(GIC_SPURIOUS_INTID);
        };
        let mut delivery = state
            .redistributor_mut(vcpu, "acknowledge GICv2 interrupt")?
            .take_pending_delivery(intid)
            .ok_or_else(|| VgicError::InvalidStateTransition {
                intid,
                operation: "read GICC_IAR",
                detail: "the selected pending delivery disappeared".into(),
            })?;
        state.mark_inflight(vcpu, intid)?;
        let mut active_state = state.synchronize_inflight(vcpu, intid, InterruptState::Active)?;
        let source = if let IntId::Sgi(sgi) = intid {
            let redistributor = state.redistributor_mut(vcpu, "acknowledge GICv2 SGI source")?;
            let source = redistributor.take_sgi_source(sgi);
            if redistributor.has_sgi_sources(sgi) {
                redistributor.private_mut(intid)?.set_pending(true);
                active_state = InterruptState::ActivePending;
            }
            source
        } else {
            0
        };
        delivery.set_state(active_state);
        state
            .redistributor_mut(vcpu, "record GICv2 active interrupt")?
            .store_active_delivery(delivery, active_state);
        state
            .redistributor_mut(vcpu, "record GICv2 running priority")?
            .cpu_interface_mut()
            .push_v2_active(intid, priority);
        Ok(u64::from(intid.raw()) | (u64::from(source) << 10))
    }

    fn eoi_v2(&self, vcpu: GicVcpuId, value: u64) -> VgicResult {
        let Some(intid) = decode_eoi_intid(value) else {
            return Ok(());
        };
        let retirement = {
            let mut state = self.inner.state.lock();
            let (priority_dropped, eoi_mode) = {
                let cpu = state
                    .redistributor_mut(vcpu, "write GICC_EOIR")?
                    .cpu_interface_mut();
                (cpu.drop_v2_priority(intid), cpu.v2_eoi_mode())
            };
            if !priority_dropped || eoi_mode {
                None
            } else {
                state.deactivate_interrupt(vcpu, intid)?
            }
        };
        self.apply_v2_retirement(vcpu, retirement)
    }

    fn dir_v2(&self, vcpu: GicVcpuId, value: u64) -> VgicResult {
        let Some(intid) = decode_eoi_intid(value) else {
            return Ok(());
        };
        let retirement = self.inner.state.lock().deactivate_interrupt(vcpu, intid)?;
        self.apply_v2_retirement(vcpu, retirement)
    }

    fn apply_v2_retirement(
        &self,
        vcpu: GicVcpuId,
        retirement: Option<DeliveryRetirement>,
    ) -> VgicResult {
        let Some(retirement) = retirement else {
            return Ok(());
        };
        match retirement {
            DeliveryRetirement::Emulated { intid } => {
                backend_result(self.inner.backend.retire_emulated_interrupt(vcpu, intid))
            }
            DeliveryRetirement::Physical { binding } => self.complete_physical_spi(vcpu, binding),
        }
    }

    fn cpu_interface(
        &self,
        vcpu: GicVcpuId,
        operation: &'static str,
    ) -> VgicResult<crate::CpuInterfaceState> {
        Ok(self
            .inner
            .state
            .lock()
            .redistributor(vcpu, operation)?
            .cpu_interface()
            .clone())
    }
}

fn is_private_register(offset: u64) -> bool {
    offset == GICD_ICFGR + 4
        || matches!(
            offset,
            GICD_IGROUPR
                | GICD_ISENABLER
                | GICD_ICENABLER
                | GICD_ISPENDR
                | GICD_ICPENDR
                | GICD_ISACTIVER
                | GICD_ICACTIVER
                | GICD_ICFGR
        )
        || (GICD_IPRIORITYR..GICD_IPRIORITYR + 32).contains(&offset)
}

fn decode_eoi_intid(value: u64) -> Option<IntId> {
    let raw = (value & 0x3ff) as u32;
    IntId::new(raw).ok()
}

fn wake_all(wakes: Vec<Arc<dyn GicV3VcpuWake>>) -> VgicResult {
    for wake in wakes {
        wake.wake()?;
    }
    Ok(())
}

fn validate_frame_access(
    region: RegisterRegion,
    size: u64,
    offset: u64,
    width: AccessWidth,
    operation: &'static str,
) -> VgicResult {
    if offset
        .checked_add(width.size() as u64)
        .is_none_or(|end| end > size)
        || !offset.is_multiple_of(width.size() as u64)
    {
        return Err(VgicError::InvalidAccess {
            region,
            operation,
            offset,
            width,
            detail: "access is unaligned or outside the register frame".into(),
        });
    }
    Ok(())
}

fn validate_byte_array_access(
    region: RegisterRegion,
    offset: u64,
    width: AccessWidth,
    base: u64,
    length: u64,
    operation: &'static str,
) -> VgicResult {
    if !matches!(
        width,
        AccessWidth::Byte | AccessWidth::Word | AccessWidth::Dword
    ) || offset < base
        || offset
            .checked_add(width.size() as u64)
            .is_none_or(|end| end > base + length)
        || !offset.is_multiple_of(width.size() as u64)
    {
        return Err(VgicError::InvalidAccess {
            region,
            operation,
            offset,
            width,
            detail: "byte-array access has an invalid width, alignment, or range".into(),
        });
    }
    Ok(())
}

fn require_width(
    region: RegisterRegion,
    offset: u64,
    width: AccessWidth,
    operation: &'static str,
) -> VgicResult {
    if width == AccessWidth::Dword {
        Ok(())
    } else {
        Err(VgicError::InvalidAccess {
            region,
            operation,
            offset,
            width,
            detail: "register requires a 32-bit access".into(),
        })
    }
}
