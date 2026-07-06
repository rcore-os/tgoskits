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
use rdrive::{
    probe::{OnProbeError, fdt::ResetLine},
    register::ProbeFdt,
};
use rockchip_jpeg::RockchipJpeg;

use crate::{
    mmio::iomap,
    soc::{rk3588_enable_clock, rk3588_enable_power_domain},
};

// RK3588 jpegd (VDPU720) constants, from the OrangePi-5-Plus device tree.
const PD_VDPU: usize = 21;
// Synthetic `ClkId` keys into the rockchip-soc gate table — NOT the DT binding
// numbers. The canonical `ACLK/HCLK_JPEG_DECODER` = 421/422 are already taken by
// the USB3OTG1 entries in that crate, so 436/437 are used as unique keys instead.
// The actual hardware gate is the verified `CLKGATE_CON(45)` bit 2/3 in gate.rs.
const CLK_ACLK_JPEG_DECODER: u32 = 436;
const CLK_HCLK_JPEG_DECODER: u32 = 437;

// The per-block Rockchip IOMMU v2 sits 0x480 into the same register page.
const IOMMU_OFFSET: usize = 0x480;
const RK_MMU_DTE_ADDR: usize = 0x00;
const RK_MMU_STATUS: usize = 0x04;
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
    let resets = info.reset_lines()?;

    bring_up_power_and_clocks(&resets);
    bypass_iommu(base);

    let dma = axklib::dma::device_with_mask(u32::MAX as u64);
    let jpeg = RockchipJpeg::new(base, dma);

    info!(
        "JPEG decoder probed: base={start_raw:#x} id={:#010x}",
        jpeg.read_id()
    );

    // The self-test needs `&mut`; rebind as mutable only when that feature is on,
    // so the normal build doesn't carry an unused `mut`.
    #[cfg(feature = "jpu-selftest")]
    let jpeg = {
        let mut jpeg = jpeg;
        run_selftest(&mut jpeg);
        jpeg
    };

    plat_dev.register(jpeg);
    info!("JPEG decoder registered");
    Ok(())
}

/// Bring the engine out of reset. All steps are best-effort and idempotent; the
/// shared VDPU root clocks are left enabled by the bootloader (as for RGA2), so
/// failures here are logged but not fatal.
fn bring_up_power_and_clocks(resets: &[ResetLine]) {
    if let Err(e) = rk3588_enable_power_domain(PD_VDPU) {
        info!("JPEG: enable PD_VDPU failed (continuing): {e}");
    }
    for clk in [CLK_ACLK_JPEG_DECODER, CLK_HCLK_JPEG_DECODER] {
        if let Err(e) = rk3588_enable_clock(clk) {
            info!("JPEG: enable clock {clk} failed (continuing): {e:?}");
        }
    }
    for reset in resets {
        if let Err(e) = reset.reset() {
            info!(
                "JPEG: pulse reset {:?} ({:#x}) failed (continuing): {e}",
                reset.name(),
                reset.id().raw()
            );
        }
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
        // The force-reset is asynchronous — the block ignores register writes while
        // it is in flight (like Linux `rk_iommu_force_reset`). Wait for it to settle
        // (STATUS reads back 0) before reprogramming, bounded so a wedged block can
        // never hang probe; the volatile read also orders the following writes.
        for _ in 0..1000 {
            if mmu.add(RK_MMU_STATUS).cast::<u32>().read_volatile() == 0 {
                break;
            }
            core::hint::spin_loop();
        }
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
    let mut buf = [0u8; rockchip_jpeg::SELFTEST_JPEG_CAPACITY];
    let Some(len) = rockchip_jpeg::write_selftest_jpeg(&mut buf) else {
        info!("JPU_SELFTEST_FAIL: could not encode the self-test JPEG");
        return;
    };
    match jpeg.decode_jpeg(&buf[..len], &mut clock, SELFTEST_TIMEOUT_NS) {
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

// --- Kernel-facing accessors for the /dev/mpp_service node (mirror rknpu) ---

pub use rockchip_jpeg::{JpuError, mpp, registers};

/// Errors surfaced to the `/dev/mpp_service` node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// No JPEG decoder device is registered.
    NotFound,
    /// The device is busy (locked by another caller).
    Busy,
    /// The decode timed out.
    Timeout,
    /// The hardware reported a decode error.
    Decode,
}

/// Whether a JPEG decoder device has been probed and registered.
pub fn is_available() -> bool {
    rdrive::get_one::<RockchipJpeg>().is_some()
}

/// Read the hardware id register (`prod_num` in the upper half).
pub fn read_id() -> Result<u32, Error> {
    let dev = rdrive::get_one::<RockchipJpeg>()
        .ok_or(Error::NotFound)?
        .try_lock()
        .map_err(|_| Error::Busy)?;
    Ok(dev.read_id())
}

/// Program a resolved MPP register array, run the decode, and copy the register
/// file back into `readback`. `timeout_ns` bounds the polled wait.
pub fn run_raw(
    regs: &[u32; registers::REG_COUNT],
    readback: &mut [u32; registers::REG_COUNT],
    timeout_ns: u64,
) -> Result<(), Error> {
    let mut dev = rdrive::get_one::<RockchipJpeg>()
        .ok_or(Error::NotFound)?
        .try_lock()
        .map_err(|_| Error::Busy)?;
    let mut clock = axklib::time::monotonic_nanos;
    dev.core()
        .run_raw(regs, readback, &mut clock, timeout_ns)
        .map(|_| ())
        .map_err(|e| match e {
            JpuError::DecodeTimeout => Error::Timeout,
            _ => Error::Decode,
        })
}
