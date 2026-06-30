//! OS glue for the RK3588 hardware JPEG decoder (`jpegd@fdb90000`, VDPU720).
//!
//! Probes the device-tree node, maps its registers, brings the engine out of
//! reset (power domain + clocks + soft reset), puts the per-block IOMMU into
//! pass-through (bypass) so contiguous buffers reach the engine by physical
//! address, and registers a [`RockchipJpeg`] device. An optional boot-time
//! self-test (feature `jpu-selftest`) decodes an embedded JPEG to validate the
//! datapath without userspace.

use core::ptr::NonNull;

use log::info;
use rdrive::{probe::OnProbeError, register::ProbeFdt};
use rockchip_jpeg::RockchipJpeg;

use crate::{
    mmio::iomap,
    soc::{
        rk3588_enable_clock, rk3588_enable_power_domain, rk3588_reset_assert, rk3588_reset_deassert,
    },
};

// RK3588 jpegd (VDPU720) constants, from the OrangePi-5-Plus device tree.
const PD_VDPU: usize = 21;
const CLK_ACLK_JPEG_DECODER: u32 = 436;
const CLK_HCLK_JPEG_DECODER: u32 = 437;
const RST_VIDEO_A: u64 = 722;
const RST_VIDEO_H: u64 = 723;

// The per-block Rockchip IOMMU v2 sits 0x480 into the same register page.
const IOMMU_OFFSET: usize = 0x480;
const RK_MMU_DTE_ADDR: usize = 0x00;
const RK_MMU_COMMAND: usize = 0x08;
const RK_MMU_INT_MASK: usize = 0x1c;
const RK_MMU_CMD_DISABLE_PAGING: u32 = 1;
const RK_MMU_CMD_FORCE_RESET: u32 = 6;

#[cfg(feature = "jpu-selftest")]
const SELFTEST_TIMEOUT_NS: u64 = 200_000_000;

crate::model_register!(
    name: "Rockchip JPEG Decoder",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["rockchip,rkv-jpeg-decoder-v1"],
            on_probe: probe
        }
    ],
);

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, plat_dev) = probe.into_parts();
    let reg = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other("jpegd node has no reg"))?;
    let start_raw = reg.address as usize;
    let size_raw = reg.size.unwrap_or(0x400) as usize;
    let (start, size, offset) = page_aligned_region(start_raw, size_raw);
    let base = unsafe { iomap(start, size)?.add(offset) };

    bring_up_power_and_clocks();
    bypass_iommu(base);

    let dma = axklib::dma::device_with_mask(u32::MAX as u64);
    let mut jpeg = RockchipJpeg::new(base, dma);

    info!(
        "JPEG decoder probed: base={start_raw:#x} id={:#010x}",
        jpeg.read_id()
    );

    #[cfg(feature = "jpu-selftest")]
    run_selftest(&mut jpeg);

    plat_dev.register(jpeg);
    info!("JPEG decoder registered");
    Ok(())
}

/// Bring the engine out of reset. All steps are best-effort and idempotent; the
/// shared VDPU root clocks are left enabled by the bootloader (as for RGA2), so
/// failures here are logged but not fatal.
fn bring_up_power_and_clocks() {
    if let Err(e) = rk3588_enable_power_domain(PD_VDPU) {
        info!("JPEG: enable PD_VDPU failed (continuing): {e}");
    }
    for clk in [CLK_ACLK_JPEG_DECODER, CLK_HCLK_JPEG_DECODER] {
        if let Err(e) = rk3588_enable_clock(clk) {
            info!("JPEG: enable clock {clk} failed (continuing): {e:?}");
        }
    }
    // Pulse the video resets (assert then deassert) for a known starting state.
    for rst in [RST_VIDEO_A, RST_VIDEO_H] {
        let _ = rk3588_reset_assert(rst);
    }
    for rst in [RST_VIDEO_A, RST_VIDEO_H] {
        let _ = rk3588_reset_deassert(rst);
    }
}

/// Put the per-block IOMMU into pass-through: force-reset to clear any stale page
/// table a prior OS left, keep paging disabled, and mask its interrupts. The
/// engine then accesses contiguous buffers by physical address.
fn bypass_iommu(base: NonNull<u8>) {
    unsafe {
        let mmu = base.as_ptr().add(IOMMU_OFFSET);
        mmu.add(RK_MMU_COMMAND)
            .cast::<u32>()
            .write_volatile(RK_MMU_CMD_FORCE_RESET);
        mmu.add(RK_MMU_DTE_ADDR).cast::<u32>().write_volatile(0);
        mmu.add(RK_MMU_COMMAND)
            .cast::<u32>()
            .write_volatile(RK_MMU_CMD_DISABLE_PAGING);
        mmu.add(RK_MMU_INT_MASK).cast::<u32>().write_volatile(0);
    }
}

#[cfg(feature = "jpu-selftest")]
fn run_selftest(jpeg: &mut RockchipJpeg) {
    let mut clock = axklib::time::monotonic_nanos;
    match jpeg.decode_jpeg(
        rockchip_jpeg::SELFTEST_JPEG,
        &mut clock,
        SELFTEST_TIMEOUT_NS,
    ) {
        Ok(status) if status.is_success() => {
            info!("JPU_SELFTEST_PASS reg1={:#010x}", status.raw())
        }
        Ok(status) => info!(
            "JPU_SELFTEST_FAIL (not done, no error bit) reg1={:#010x}",
            status.raw()
        ),
        Err(e) => info!("JPU_SELFTEST_FAIL: {e:?}"),
    }
}

fn page_aligned_region(start_raw: usize, size_raw: usize) -> (usize, usize, usize) {
    let page_size = 0x1000;
    let start = start_raw & !(page_size - 1);
    let offset = start_raw - start;
    let end = (start_raw + size_raw + page_size - 1) & !(page_size - 1);
    (start, end - start, offset)
}
