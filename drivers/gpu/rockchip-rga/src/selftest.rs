//! OS-independent RGA bring-up selftest helpers. The OS glue supplies a powered `RgaCore`, a delay
//! function, and DMA buffers; this module runs fill/copy, polls to completion, and CRC-checks output.
use crate::{
    RgaCore,
    backend::{RgaDiag, RgaStatus},
    buffer::RgaDmaBuffer,
    error::RgaError,
    operation::{Blit, CscStandard, ImageDesc, PixelFormat, Rect, RgaOperation},
};

/// IEEE 802.3 CRC-32 (poly 0xEDB88320), used to fingerprint destination buffers.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Solid color the smoke fill writes. Packed ABGR/RGBA byte order [0xff,0x33,0x22,0x11].
pub const SMOKE_FILL_COLOR: u32 = 0x1122_33ff;
/// Sentinel the smoke fill writes into the destination BEFORE the fill, so a board run can tell
/// "engine wrote nothing" (readback == this) apart from "wrote zeros" / "wrote the wrong color".
pub const SMOKE_FILL_POISON: u32 = 0xDEAD_BEEF;

pub struct SelftestReport {
    pub fill_ok: bool,
    pub copy_ok: bool,
    pub crc: u32,
    /// Engine state captured the instant the fill op reported Done, BEFORE finish() W1C-cleared
    /// INT. Lets the OS glue print whether af (INT bit2) actually latched on a wrong-output fill.
    pub fill_diag: RgaDiag,
    /// Same, captured at the copy op's completion.
    pub copy_diag: RgaDiag,
    /// Destination samples [px0, px1, pxmid, pxlast] read back AFTER the fill (the dst was poisoned
    /// with `SMOKE_FILL_POISON` before it). The OS glue classifies these: all==color → fill works;
    /// all==poison → engine wrote nothing; all==0 → wrote zeros; uniform-other → wrong channel/format.
    pub fill_px: [u32; 4],
}

/// Run a fill then a copy on a powered RGA2 core, polling to completion with a bounded number of
/// `poll`/`delay` iterations. `delay_us` is an OS-supplied busy/sleep of ~`step_us` microseconds.
pub fn run_rga2_smoke(
    core: &mut RgaCore,
    src: &mut RgaDmaBuffer,
    dst: &mut RgaDmaBuffer,
    width: u32,
    height: u32,
    mut delay_us: impl FnMut(u32),
) -> core::result::Result<SelftestReport, (RgaError, RgaDiag)> {
    let fmt = PixelFormat::Rgba8888;
    let stride = width * fmt.bytes_per_pixel();
    let src_img = ImageDesc::rgb(width, height, stride, fmt, src.phys_addr());
    let dst_img = ImageDesc::rgb(width, height, stride, fmt, dst.phys_addr());

    // 1) Fill dst with a known color, verify the engine wrote it. POISON the destination with a
    //    sentinel first and flush it to the device: a freshly-zeroed buffer cannot distinguish
    //    "engine wrote nothing" from "engine wrote zeros", but a poisoned one can.
    let color: u32 = SMOKE_FILL_COLOR;
    // SAFETY: the mutable slice is not retained across the device submission below.
    {
        let dbytes = unsafe { dst.cpu_bytes_mut() };
        for px in dbytes.chunks_exact_mut(4) {
            px.copy_from_slice(&SMOKE_FILL_POISON.to_le_bytes());
        }
    }
    dst.prepare_for_device();
    core.start(&RgaOperation::Fill {
        dst: dst_img,
        color,
    })
    .map_err(|e| (e, core.diag()))?;
    let fill_diag = poll_done(core, &mut delay_us)?;
    dst.complete_for_cpu();
    let fill_ok = dst
        .cpu_bytes()
        .chunks_exact(4)
        .all(|px| u32::from_le_bytes([px[0], px[1], px[2], px[3]]) == color);
    let fill_px = {
        let b = dst.cpu_bytes();
        let n = b.len() / 4;
        let px =
            |i: usize| u32::from_le_bytes([b[i * 4], b[i * 4 + 1], b[i * 4 + 2], b[i * 4 + 3]]);
        [px(0), px(1), px(n / 2), px(n.saturating_sub(1))]
    };

    // 2) Fill src via CPU, copy src->dst, CRC dst and compare to src.
    // SAFETY: the mutable slice is not retained across the submission below.
    let src_bytes = unsafe { src.cpu_bytes_mut() };
    for (i, b) in src_bytes.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(31).wrapping_add(7);
    }
    src.prepare_for_device();
    let src_crc = crc32(src.cpu_bytes());

    core.start(&RgaOperation::Copy {
        src: src_img,
        dst: dst_img,
    })
    .map_err(|e| (e, core.diag()))?;
    let copy_diag = poll_done(core, &mut delay_us)?;
    dst.complete_for_cpu();
    let crc = crc32(dst.cpu_bytes());
    let copy_ok = crc == src_crc;

    Ok(SelftestReport {
        fill_ok,
        copy_ok,
        crc,
        fill_diag,
        copy_diag,
        fill_px,
    })
}

/// Fill a destination plane referenced by a raw physical address (e.g. an imported dma-buf) with a
/// solid color, polling to completion. Proves the imported-buffer submission path; the caller owns
/// the backing memory and verifies the result (CRC / pixel check).
pub fn run_rga2_fill_imported(
    core: &mut RgaCore,
    dst_phys: u64,
    width: u32,
    height: u32,
    color: u32,
    mut delay_us: impl FnMut(u32),
) -> core::result::Result<RgaDiag, (RgaError, RgaDiag)> {
    let fmt = PixelFormat::Rgba8888;
    let dst = ImageDesc::rgb(width, height, width * fmt.bytes_per_pixel(), fmt, dst_phys);
    core.start(&RgaOperation::Fill { dst, color })
        .map_err(|e| (e, core.diag()))?;
    poll_done(core, &mut delay_us)
}

/// Downscale a src plane into a (possibly smaller) dst via the general Blit path, polling to
/// completion. Caller owns the buffers + verifies output. Proves the resize encoding on hardware.
///
/// `src_phys`/`dst_phys` are the physical base addresses; dimensions are in pixels (RGBA8888).
pub fn run_rga2_blit_resize(
    core: &mut RgaCore,
    src_phys: u64,
    src_dims: (u32, u32),
    dst_phys: u64,
    dst_dims: (u32, u32),
    mut delay_us: impl FnMut(u32),
) -> core::result::Result<RgaDiag, (RgaError, RgaDiag)> {
    let fmt = PixelFormat::Rgba8888;
    let (src_w, src_h) = src_dims;
    let (dst_w, dst_h) = dst_dims;
    let src = ImageDesc::rgb(src_w, src_h, src_w * fmt.bytes_per_pixel(), fmt, src_phys);
    let dst = ImageDesc::rgb(dst_w, dst_h, dst_w * fmt.bytes_per_pixel(), fmt, dst_phys);
    core.start(&RgaOperation::Blit(crate::operation::Blit::resize(
        src, dst,
    )))
    .map_err(|e| (e, core.diag()))?;
    poll_done(core, &mut delay_us)
}

/// Same-size YUYV422 -> RGB888 colour-space conversion — the exact op the tennis app submits via
/// librga (`imcvtcolor`). `src_phys` is a packed YUYV422 plane, `dst_phys` an RGB888 plane, both
/// `w*h`. This isolates the CSC datapath: the resize selftest (RGBA, no CSC) passes on hardware, so
/// if THIS fails the bug is in the YUYV-packed-src + YUV->RGB CSC register encoding, not the engine.
pub fn run_rga2_csc_yuyv(
    core: &mut RgaCore,
    src_phys: u64,
    dst_phys: u64,
    w: u32,
    h: u32,
    mut delay_us: impl FnMut(u32),
) -> core::result::Result<RgaDiag, (RgaError, RgaDiag)> {
    let src = ImageDesc::rgb(
        w,
        h,
        w * PixelFormat::Yuyv422.bytes_per_pixel(),
        PixelFormat::Yuyv422,
        src_phys,
    );
    let dst = ImageDesc::rgb(
        w,
        h,
        w * PixelFormat::Rgb888.bytes_per_pixel(),
        PixelFormat::Rgb888,
        dst_phys,
    );
    let full = Rect {
        x: 0,
        y: 0,
        width: w,
        height: h,
    };
    core.start(&RgaOperation::Blit(Blit::new(
        src,
        dst,
        full,
        full,
        Some(CscStandard::Bt601Limited),
    )))
    .map_err(|e| (e, core.diag()))?;
    poll_done(core, &mut delay_us)
}

/// Fill a destination via the PROVEN bitblt datapath instead of the dedicated color_fill_mode:
/// CPU-fill `src` with the solid color, flush it, then do a same-size (no-scale) blit src→dst.
/// If the engine's bitblt is correct (validated on hardware: copy + resize PASS) this is a
/// guaranteed-correct fill — independent of any color_fill quirk. Used as both a diagnostic and a
/// fallback implementation. The caller owns `dst` (raw phys) and verifies the output.
pub fn run_rga2_fill_via_blit(
    core: &mut RgaCore,
    src: &mut RgaDmaBuffer,
    dst_phys: u64,
    width: u32,
    height: u32,
    color: u32,
    mut delay_us: impl FnMut(u32),
) -> core::result::Result<RgaDiag, (RgaError, RgaDiag)> {
    let fmt = PixelFormat::Rgba8888;
    let stride = width * fmt.bytes_per_pixel();
    // SAFETY: the mutable slice is not retained across the device submission below.
    {
        let sbytes = unsafe { src.cpu_bytes_mut() };
        for px in sbytes.chunks_exact_mut(4) {
            px.copy_from_slice(&color.to_le_bytes());
        }
    }
    src.prepare_for_device();
    let src_img = ImageDesc::rgb(width, height, stride, fmt, src.phys_addr());
    let dst_img = ImageDesc::rgb(width, height, stride, fmt, dst_phys);
    core.start(&RgaOperation::Blit(crate::operation::Blit::resize(
        src_img, dst_img,
    )))
    .map_err(|e| (e, core.diag()))?;
    poll_done(core, &mut delay_us)
}

/// Poll to completion. On success returns the engine-state diag captured the instant Done was
/// observed — BEFORE `finish()` runs `ack()` and W1C-clears the INT af/err flags. The caller can
/// thus tell whether af (INT bit2) actually latched even when the op completed but produced wrong
/// pixels (the wrong-output case is otherwise indistinguishable from a clean completion).
fn poll_done(
    core: &mut RgaCore,
    delay_us: &mut impl FnMut(u32),
) -> core::result::Result<RgaDiag, (RgaError, RgaDiag)> {
    // ~50 ms budget at 100 us steps (spec letterbox target is < 5 ms; generous for bring-up).
    for _ in 0..500 {
        match core.poll_status() {
            RgaStatus::Done => {
                let d = core.diag(); // capture un-acked INT (af/err still set) before finish()
                core.finish();
                return Ok(d);
            }
            RgaStatus::Error => {
                let d = core.diag();
                core.finish();
                return Err((RgaError::Hardware, d));
            }
            RgaStatus::Busy => delay_us(100),
        }
    }
    let d = core.diag(); // capture BEFORE recover() clobbers SYS_CTRL/INT
    core.recover().ok(); // timeout → reset core
    Err((RgaError::Timeout, d))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn crc32_known_vector() {
        // CRC-32/ISO-HDLC of "123456789" == 0xCBF43926
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
    #[test]
    fn crc32_empty_is_zero() {
        assert_eq!(crc32(&[]), 0);
    }
}
