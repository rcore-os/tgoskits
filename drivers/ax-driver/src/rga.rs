use alloc::vec::Vec;

use log::{info, warn};
use rdrive::{
    probe::OnProbeError,
    register::{FdtInfo, ProbeFdt},
};
use rockchip_pm::{PowerDomain, RockchipPM};
use rockchip_rga::{RgaCoreConfig, RgaCoreResource, RgaVersion, RockchipRga};

use crate::mmio::iomap;

crate::model_register!(
    name: "Rockchip RGA",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    // Match all three RGA cores. rdrive probes each matching FDT node independently (its probed
    // set is keyed per node, not per driver), so every core gets its own probe() call; PR-1
    // brings up RGA2 and defers the RGA3 cores in probe().
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &[
                "rockchip,rga3_core0",
                "rockchip,rga3_core1",
                "rockchip,rga2_core0",
            ],
            on_probe: probe
        }
    ],
);

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, plat_dev) = probe.into_parts();
    let config = detect_core_config(&info)
        .ok_or_else(|| OnProbeError::other("unsupported Rockchip RGA compatible string"))?;

    // PR-1 brings up RGA2 only. RGA3's clock tree / reset / real version-register offset are
    // unverified and deferred to a later phase, so skip the RGA3 cores here (they still each get a
    // probe() call now that rdrive probes every matching node). Skipping before any MMIO avoids
    // the synchronous external abort an unclocked/unverified RGA3 version-read would raise.
    if config.version == RgaVersion::Rga3 {
        info!(
            "RGA3 core (index {}) probe deferred in PR-1; skipping (RGA2-only bring-up)",
            config.core_index
        );
        return Ok(());
    }

    // Bring the RGA2 core onto the bus BEFORE any MMIO: RgaCore::new reads the version register
    // at base+0x28, which raises a synchronous external abort if the bus-interface clocks are
    // still gated (U-Boot leaves the RGA clocks off at handoff). Power the domain, then ungate
    // aclk/hclk/clk. If the clocks cannot be established, skip the core rather than fault the
    // kernel on the version read.
    enable_power(config);
    if let Err(e) = enable_rga2_clocks() {
        warn!("RGA2 clock bring-up failed ({e:?}); skipping core to avoid an MMIO abort");
        return Ok(());
    }

    let irq = crate::binding_info_from_fdt(&info)?.irq_num();

    let mut resources = Vec::new();
    for reg in info.node.regs() {
        let start_raw = reg.address as usize;
        let size_raw = reg.size.unwrap_or(0x1000) as usize;
        let (start, size, offset) = page_aligned_region(start_raw, size_raw);
        // SAFETY: `start`/`size` are a page-aligned MMIO window from the FDT `reg`; iomap maps it and `offset` is within it.
        let base = unsafe { iomap(start, size)?.add(offset) };
        resources.push(RgaCoreResource {
            base,
            size: size_raw,
            irq,
            config,
        });
    }

    let dma = axklib::dma::device_with_mask(u32::MAX as u64);
    let rga = RockchipRga::new(&resources, dma);
    let core_count = rga.core_count();
    let version = rga.cores().first().map(|c| c.version());
    plat_dev.register(rga);
    info!("RGA registered: cores={core_count} version={version:?}");
    Ok(())
}

fn enable_power(config: RgaCoreConfig) {
    // RK3588 power-domain IDs (from rockchip-pm rk3588 variant): RGA30=22, RGA31=30, RGA2/VDPU=21.
    let domain = match config.version {
        RgaVersion::Rga3 => {
            if config.core_index == 0 {
                PowerDomain(22) // RGA30
            } else {
                PowerDomain(30) // RGA31
            }
        }
        RgaVersion::Rga2 => PowerDomain(21), // RGA2/VDPU
    };
    match rdrive::get_one::<RockchipPM>() {
        Some(pm) => match pm.lock() {
            Ok(mut pm) => {
                if let Err(e) = pm.power_domain_on(domain) {
                    warn!("RGA power_domain_on({domain:?}) failed: {e:?}");
                }
            }
            Err(e) => warn!("RGA: RockchipPM lock failed: {e:?}"),
        },
        None => warn!("RGA: RockchipPM not found; assuming domain already powered"),
    }
    // Bus clocks are ungated separately in enable_rga2_clocks() (RGA2 needs them live before any
    // MMIO). Resets are left to the GLB power-on reset — the RGA DTS nodes carry no `resets`
    // property and the cores are not held in reset at boot, so a manual deassert is unnecessary.
}

/// Ungate the RGA2 core's three CRU bus clocks (hclk, aclk, clk). U-Boot leaves the RGA clocks
/// gated at handoff, so this is the load-bearing step that makes the version-register read at
/// base+0x28 succeed (otherwise it aborts on a gated bus). The clock ids are the RK3588 BSP
/// rk3588-cru.h values; their gate positions (CLKGATE_CON45 bits 7/8/9) live in the rockchip-soc
/// CRU gate table.
fn enable_rga2_clocks() -> Result<(), OnProbeError> {
    // HCLK_RGA2 = 438, ACLK_RGA2 = 439, CLK_RGA2_CORE = 440 (rk3588-cru.h).
    for &clk_id in &[438u32, 439, 440] {
        crate::soc::rk3588_enable_clock(clk_id)?;
    }
    Ok(())
}

fn detect_core_config(info: &FdtInfo<'_>) -> Option<RgaCoreConfig> {
    for compatible in info.node.as_node().compatibles() {
        match compatible {
            "rockchip,rga3_core0" => return Some(RgaCoreConfig::rga3(0)),
            "rockchip,rga3_core1" => return Some(RgaCoreConfig::rga3(1)),
            "rockchip,rga2_core0" => return Some(RgaCoreConfig::rga2(0)),
            _ => {}
        }
    }
    None
}

fn page_aligned_region(start_raw: usize, size_raw: usize) -> (usize, usize, usize) {
    let page_size = 0x1000;
    let start = start_raw & !(page_size - 1);
    let offset = start_raw - start;
    let end = (start_raw + size_raw + page_size - 1) & !(page_size - 1);
    (start, end - start, offset)
}
