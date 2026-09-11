//! Performance-monitoring capability helpers.

#[cfg(all(target_arch = "aarch64", feature = "pmu"))]
pub use ax_cpu::pmu::{CounterId, EventConfig, EventSupport, Pmu, PmuError, PmuInfo};

#[cfg(not(all(target_arch = "aarch64", feature = "pmu")))]
/// Information probed from a CPU PMU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PmuInfo {
    /// Number of programmable event counters.
    pub num_counters: usize,
}

/// Returns PMU information when the current architecture/runtime supports it.
pub fn info() -> Option<PmuInfo> {
    #[cfg(all(target_arch = "aarch64", feature = "pmu"))]
    {
        // SAFETY: copying immutable capabilities does not alter CPU ownership.
        unsafe { with_current(|pmu| pmu.info()) }
    }
    #[cfg(not(all(target_arch = "aarch64", feature = "pmu")))]
    {
        None
    }
}

/// Returns the raw CPU identification register used by PMU tooling.
pub fn cpu_id_raw() -> Option<u64> {
    #[cfg(target_arch = "aarch64")]
    {
        Some(ax_cpu::capability::read_midr_el1())
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        None
    }
}

/// Returns the platform IRQ id used for PMU overflows.
pub fn irq() -> Result<crate::irq::IrqId, crate::irq::IrqError> {
    #[cfg(target_arch = "aarch64")]
    {
        static IRQ: ax_lazyinit::OnceLock<crate::irq::IrqId> = ax_lazyinit::OnceLock::new();
        if let Some(irq) = IRQ.get() {
            return Ok(*irq);
        }
        if crate::irq::in_irq_context() {
            return Err(crate::irq::IrqError::InIrqContext);
        }
        let discovered = discover_pmu_ppi()?;
        Ok(*IRQ.call_once(|| discovered))
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        Err(crate::irq::IrqError::Unsupported)
    }
}

/// Runs a bounded PMU operation on the pinned, IRQ-excluded current CPU.
///
/// # Safety
/// The operation must not block, enable interrupts, recursively access the PMU,
/// or retain hardware access. The caller owns the affected counter slots.
#[cfg(all(target_arch = "aarch64", feature = "pmu"))]
pub unsafe fn with_current<R>(operation: impl FnOnce(&mut Pmu) -> R) -> Option<R> {
    struct Irqs(bool);
    impl Drop for Irqs {
        fn drop(&mut self) {
            if self.0 {
                ax_cpu::interrupt::enable_irqs();
            }
        }
    }
    let enabled = ax_cpu::interrupt::irqs_enabled();
    ax_cpu::interrupt::disable_irqs();
    let _irqs = Irqs(enabled);
    // SAFETY: IRQ exclusion prevents migration and PMU IRQ reentry. The
    // runtime's CPU pin and exclusive scope serialize local hardware users.
    unsafe {
        crate::percpu::with_cpu_pin(|pin| {
            crate::percpu::with_exclusive_cpu(pin, |_| {
                ax_cpu::pmu::Pmu::current()
                    .ok()
                    .map(|mut pmu| operation(&mut pmu))
            })
        })
    }
    .ok()
    .flatten()
}

/// Resolves the supported common-PPI firmware topology before IRQ registration.
#[cfg(target_arch = "aarch64")]
fn discover_pmu_ppi() -> Result<crate::irq::IrqId, crate::irq::IrqError> {
    use crate::irq::{HwIrq, IrqError, IrqTrigger};
    let fdt = crate::dtb::get_fdt().ok_or(IrqError::Unsupported)?;
    let mut selected = None;
    let mut affinity = heapless::Vec::<u32, { usize::BITS as usize }>::new();
    let mut all_cpus = false;
    for node in fdt.find_compatible(&[
        "arm,armv8-pmuv3",
        "arm,cortex-a53-pmu",
        "arm,cortex-a55-pmu",
        "arm,cortex-a72-pmu",
        "arm,cortex-a76-pmu",
    ]) {
        if !matches!(
            node.find_property("status").map(|property| property.str()),
            None | Some("okay" | "ok")
        ) {
            continue;
        }
        if let Some(property) = node.find_property("interrupt-affinity") {
            if property.raw_value().is_empty() || !property.raw_value().len().is_multiple_of(4) {
                return Err(IrqError::InvalidIrq);
            }
            for phandle in property.u32_list() {
                let cpu = fdt
                    .get_node_by_phandle(phandle.into())
                    .ok_or(IrqError::InvalidIrq)?;
                if cpu.find_property("device_type").map(|p| p.str()) != Some("cpu") {
                    return Err(IrqError::InvalidIrq);
                }
                if !affinity.contains(&phandle) {
                    affinity.push(phandle).map_err(|_| IrqError::Unsupported)?;
                }
            }
        } else {
            all_cpus = true;
        }
        let parent = node.interrupt_parent().ok_or(IrqError::InvalidIrq)?;
        if parent
            .node
            .find_property("#interrupt-cells")
            .map(|property| property.u32())
            != Some(3)
            || !parent.node.compatibles().any(|compatible| {
                matches!(
                    compatible,
                    "arm,gic-v3" | "arm,cortex-a15-gic" | "arm,gic-400"
                )
            })
        {
            return Err(IrqError::Unsupported);
        }
        let mut rows = node.interrupts().ok_or(IrqError::InvalidIrq)?;
        let mut cells = rows.next().ok_or(IrqError::InvalidIrq)?;
        let kind = cells.next().ok_or(IrqError::InvalidIrq)?;
        let number = cells.next().ok_or(IrqError::InvalidIrq)?;
        let flags = cells.next().ok_or(IrqError::InvalidIrq)?;
        // Per-core SPI and partitioned-PPI routing need a platform affinity
        // owner. Do not invent one from a CPU index or fall back to IRQ 23.
        if kind != 1 || number >= 16 || rows.next().is_some() {
            return Err(IrqError::Unsupported);
        }
        let trigger = match flags & 15 {
            1 | 2 => IrqTrigger::Edge,
            4 | 8 => IrqTrigger::Level,
            _ => return Err(IrqError::InvalidIrq),
        };
        let candidate = (number + 16, trigger);
        if selected.is_some_and(|previous| previous != candidate) {
            return Err(IrqError::Unsupported);
        }
        selected = Some(candidate);
    }
    let (number, trigger) = selected.ok_or(IrqError::NotFound)?;
    if !all_cpus {
        // Multiple cluster nodes may describe one common PPI. Accept their
        // union only when every enabled CPU is covered; this API deliberately
        // does not manufacture a CPU-mask owner for a partial PMU topology.
        let mut found_cpu = false;
        for cpu in fdt.all_nodes() {
            if cpu.find_property("device_type").map(|p| p.str()) != Some("cpu")
                || !matches!(
                    cpu.find_property("status").map(|p| p.str()),
                    None | Some("okay" | "ok")
                )
            {
                continue;
            }
            found_cpu = true;
            let handle = cpu.find_property("phandle").ok_or(IrqError::Unsupported)?;
            if handle.raw_value().len() != 4 || !affinity.contains(&handle.u32()) {
                return Err(IrqError::Unsupported);
            }
        }
        if !found_cpu {
            return Err(IrqError::Unsupported);
        }
    }
    let irq = crate::irq::resolve_percpu_irq(HwIrq(number))?;
    crate::irq::set_trigger(irq, trigger)?;
    Ok(irq)
}
