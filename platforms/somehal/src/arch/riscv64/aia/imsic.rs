use alloc::{format, vec, vec::Vec};
use core::num::NonZeroU32;

use kernutil::StaticCell;
use rdrive::{
    Phandle, module_driver,
    probe::{OnProbeError, fdt::NodeType},
    register::{FdtInfo, ProbeFdt},
};
use riscv_aia_driver::FileLayout;

static IMSIC: StaticCell<RiscvImsic> = StaticCell::uninit();

module_driver!(
    name: "RISC-V IMSIC",
    level: ProbeLevel::PreKernel,
    // APLIC consumes the IMSIC topology even if its FDT node appears first.
    priority: ProbePriority(ProbePriority::INTC.0 - 1),
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["riscv,imsics", "spacemit,k3-imsics"],
        on_probe: probe_imsic
    }],
);

pub(super) struct RiscvImsic {
    pub(super) phandle: Phandle,
    pub(super) num_ids: usize,
    pub(super) target_by_cpu: Vec<u32>,
}

pub(super) fn get() -> Option<&'static RiscvImsic> {
    IMSIC.is_init().then(|| &*IMSIC)
}

fn probe_imsic(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let info = probe.info();
    if !is_supervisor(&info.node)? {
        return Ok(());
    }
    if IMSIC.is_init() {
        return Err(OnProbeError::other(
            "multiple supervisor IMSIC instances are unsupported",
        ));
    }
    let imsic = parse_imsic(info)?;
    info!(
        "RISC-V IMSIC registered: num_ids={}, target_by_cpu={:?}",
        imsic.num_ids, imsic.target_by_cpu
    );
    IMSIC.init(imsic);
    init_current_imsic();
    Ok(())
}

/// Only the S-level external interrupt file belongs to this driver.
pub(super) fn is_supervisor(node: &NodeType<'_>) -> Result<bool, OnProbeError> {
    let cells = interrupt_cells(node)?;
    let cause = cells[1];
    if !matches!(cause, 9 | 11) || cells.as_chunks::<2>().0.iter().any(|pair| pair[1] != cause) {
        return Err(OnProbeError::other(
            "IMSIC must describe uniformly S- or M-level external interrupts",
        ));
    }
    Ok(cause == 9)
}

fn interrupt_cells(node: &NodeType<'_>) -> Result<Vec<u32>, OnProbeError> {
    let cells = node
        .as_node()
        .get_property("interrupts-extended")
        .ok_or_else(|| OnProbeError::other("IMSIC requires interrupts-extended"))?
        .get_u32_iter()
        .collect::<Vec<_>>();
    if cells.is_empty() || cells.len() % 2 != 0 {
        return Err(OnProbeError::other("invalid IMSIC interrupts-extended"));
    }
    Ok(cells)
}

fn parse_imsic(info: &FdtInfo<'_>) -> Result<RiscvImsic, OnProbeError> {
    let num_ids = property(info, "riscv,num-ids")
        .filter(|n| (63..=2047).contains(n) && (n + 1) % 64 == 0)
        .ok_or_else(|| OnProbeError::other("IMSIC num-ids must be 64*N-1 between 63 and 2047"))?
        as usize;
    let cells = interrupt_cells(&info.node)?;
    let files = cells.len() / 2;
    let hart_bits = property(info, "riscv,hart-index-bits")
        .unwrap_or(usize::BITS - (files - 1).leading_zeros());
    let layout = FileLayout::new(
        hart_bits,
        property(info, "riscv,guest-index-bits").unwrap_or(0),
        property(info, "riscv,group-index-bits").unwrap_or(0),
        property(info, "riscv,group-index-shift").unwrap_or(24),
    )
    .map_err(|err| OnProbeError::other(format!("invalid IMSIC layout: {err:?}")))?;
    let regs = info.node.regs();
    let mut addresses = vec![0; files];
    layout
        .file_addresses(
            regs.iter().map(|reg| (reg.address, reg.size)),
            &mut addresses,
        )
        .map_err(|err| OnProbeError::other(format!("invalid IMSIC file ranges: {err:?}")))?;
    let mut target_by_cpu = vec![None; someboot::smp::cpu_count()];
    let mut seen_targets = Vec::new();
    let mut seen_harts = Vec::new();
    for (pair, address) in cells.as_chunks::<2>().0.iter().zip(addresses) {
        let intc = info
            .get_by_phandle(Phandle::from(pair[0]))
            .ok_or_else(|| OnProbeError::other("IMSIC CPU interrupt controller is missing"))?;
        if intc
            .as_node()
            .get_property("#interrupt-cells")
            .and_then(|p| p.get_u32())
            != Some(1)
        {
            return Err(OnProbeError::other(
                "IMSIC CPU interrupt controller requires one interrupt cell",
            ));
        }
        let hart = intc
            .parent()
            .and_then(|cpu| cpu.regs().first().map(|r| r.address))
            .ok_or_else(|| OnProbeError::other("IMSIC interrupt file has no CPU hart ID"))?;
        let target = layout
            .target(address)
            .map_err(|err| OnProbeError::other(format!("invalid IMSIC target: {err:?}")))?;
        if seen_targets.contains(&target) || seen_harts.contains(&hart) {
            return Err(OnProbeError::other("duplicate IMSIC hart or target index"));
        }
        seen_targets.push(target);
        seen_harts.push(hart);
        if let Ok(hart) = usize::try_from(hart)
            && let Some(cpu) = someboot::smp::cpu_id_to_idx(hart)
        {
            let slot = target_by_cpu
                .get_mut(cpu)
                .ok_or_else(|| OnProbeError::other("IMSIC CPU index exceeds boot CPU count"))?;
            *slot = Some(target);
        }
    }
    let target_by_cpu = target_by_cpu
        .into_iter()
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| OnProbeError::other("a selected boot CPU has no supervisor IMSIC file"))?;
    let phandle = info
        .node
        .as_node()
        .phandle()
        .ok_or_else(|| OnProbeError::other("supervisor IMSIC requires a phandle"))?;
    Ok(RiscvImsic {
        phandle,
        num_ids,
        target_by_cpu,
    })
}

fn property(info: &FdtInfo<'_>, name: &str) -> Option<u32> {
    info.node.as_node().get_property(name)?.get_u32()
}

/// Initialize only the current hart, before its runtime can accept external IRQs.
/// All enabled EIID values are reserved exclusively for fixed APLIC source IDs.
pub(in crate::arch) fn init_current_imsic() {
    let Some(imsic) = get() else {
        return;
    };
    // No allocation, locks, scheduling, or calls into the runtime are allowed
    // while selecting an indirect CSR. Native IRQ exclusion also works during
    // secondary startup before the scheduler has installed its context hooks.
    riscv::interrupt::supervisor::free(|| {
        // SAFETY: probing validated the current CPU's supervisor file. Local
        // IRQ exclusion prevents CSR selector reentry and migration, including
        // secondary startup before scheduler context hooks are available.
        unsafe { riscv_aia_driver::init_supervisor(imsic.num_ids, imsic.num_ids.min(1023)) }
            .expect("validated IMSIC identity count");
    });
}

/// A single stopei exchange acknowledges this hart's highest-priority EIID.
pub(super) fn claim() -> Option<NonZeroU32> {
    // SAFETY: called only from the supervisor external-IRQ transaction after
    // successful IMSIC probe; stopei needs no shared indirect selector.
    unsafe { riscv_aia_driver::claim_supervisor() }
}
