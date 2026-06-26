//! Feature-gated RGA2 bring-up selftest. Logs one machine-parseable line over serial so the board
//! harness can match it. No /dev/rga involved.
use alloc::sync::Arc;

use dma_api::DmaDirection;
use rockchip_rga::{
    RgaVersion, RockchipRga,
    backend::RgaDiag,
    buffer::RgaDmaBuffer,
    selftest::{
        SMOKE_FILL_COLOR, SMOKE_FILL_POISON, crc32, run_rga2_blit_resize, run_rga2_csc_yuyv,
        run_rga2_fill_imported, run_rga2_fill_via_blit, run_rga2_smoke,
    },
};

const W: u32 = 64;
const H: u32 = 48;

fn log_diag(tag: &str, core_index: u8, d: &RgaDiag, src: u64, dst: u64) {
    let done = d.int & (1 << 2) != 0;
    let err = d.int & (1 << 0) != 0;
    let busy = !done && !err;
    warn!(
        "{tag}_DIAG core={} int=0x{:08x} sys_ctrl=0x{:08x} cmd_ctrl=0x{:08x} cmd_base=0x{:08x} \
         status=0x{:08x} ver=0x{:08x} cmd_phys=0x{:x} src=0x{:x} dst=0x{:x} busy={} done={} err={}",
        core_index,
        d.int,
        d.sys_ctrl,
        d.cmd_ctrl,
        d.cmd_base,
        d.status,
        d.version,
        d.cmd_phys,
        src,
        dst,
        busy,
        done,
        err
    );
}

/// Classify the four destination samples a poisoned native Fill produced, so the board log states
/// exactly what the engine did (instead of just PASS/FAIL):
///   COLOR_OK      — all samples == requested fill color (the fill works)
///   NOWRITE       — all samples == the pre-fill poison (engine completed but never wrote the dst)
///   ZERO          — all samples == 0 (wrote, but cleared instead of the color)
///   OTHER_UNIFORM — all samples equal but neither color/poison/0 (wrong channel order / format)
///   MIXED         — samples differ (partial write / pattern / gradient)
fn classify_fill(px: &[u32; 4], want: u32, poison: u32) -> &'static str {
    if px.iter().all(|&p| p == want) {
        "COLOR_OK"
    } else if px.iter().all(|&p| p == poison) {
        "NOWRITE"
    } else if px.iter().all(|&p| p == 0) {
        "ZERO"
    } else if px.iter().all(|&p| p == px[0]) {
        "OTHER_UNIFORM"
    } else {
        "MIXED"
    }
}

pub fn run() {
    for dev in rdrive::get_list::<RockchipRga>() {
        let mut guard = match dev.lock() {
            Ok(g) => g,
            Err(_) => continue,
        };
        let rga = &mut *guard;
        let dma = rga.dma().clone();
        let core = match rga
            .cores_mut()
            .iter_mut()
            .find(|c| c.config().version == RgaVersion::Rga2)
        {
            Some(c) => c,
            None => continue,
        };
        let bytes = (W * H * 4) as usize;
        // Destinations are Bidirectional, NOT FromDevice: the contiguous DMA backing is CACHED, so
        // the alloc-zero leaves dirty CPU lines over the dst. prepare_for_device (a clean) only runs
        // for ToDevice/Bidirectional — so a Bidirectional dst gets cleaned before the engine writes
        // (no dirty-line clobber) AND the pre-op poison reaches DRAM, which is what lets the readback
        // distinguish NOWRITE (poison survives) from ZERO (engine wrote zeros).
        let (mut src, mut dst) = match (
            RgaDmaBuffer::alloc(&dma, bytes, DmaDirection::ToDevice),
            RgaDmaBuffer::alloc(&dma, bytes, DmaDirection::Bidirectional),
        ) {
            (Ok(s), Ok(d)) => (s, d),
            _ => {
                warn!("RGA2_SELFTEST alloc=FAIL");
                return;
            }
        };
        let core_index = core.config().core_index;
        match run_rga2_smoke(core, &mut src, &mut dst, W, H, |us| {
            ax_runtime::hal::time::busy_wait(core::time::Duration::from_micros(us as u64))
        }) {
            Ok(r) => {
                info!(
                    "RGA2_SELFTEST core={} fill={} copy={} crc=0x{:08x}",
                    core_index,
                    if r.fill_ok { "PASS" } else { "FAIL" },
                    if r.copy_ok { "PASS" } else { "FAIL" },
                    r.crc
                );
                // Poison-sentinel probe: state exactly what the native (color_fill_mode) Fill did to
                // the destination, so a FAIL is actionable (no-write vs zero vs wrong-channel).
                let [fp0, fp1, fpmid, fplast] = r.fill_px;
                info!(
                    "RGA2_SELFTEST_FILL_PROBE core={} class={} want=0x{:08x} poison=0x{:08x} \
                     px0=0x{:08x} px1=0x{:08x} pxmid=0x{:08x} pxlast=0x{:08x}",
                    core_index,
                    classify_fill(&r.fill_px, SMOKE_FILL_COLOR, SMOKE_FILL_POISON),
                    SMOKE_FILL_COLOR,
                    SMOKE_FILL_POISON,
                    fp0,
                    fp1,
                    fpmid,
                    fplast
                );
                // Completed-but-wrong output: dump the completion diag so the run shows whether af
                // (INT bit2) latched (done=true => encoding/cache bug, not a detection problem).
                if !r.fill_ok {
                    log_diag(
                        "RGA2_SELFTEST_VERIFY_FILL",
                        core_index,
                        &r.fill_diag,
                        src.phys_addr(),
                        dst.phys_addr(),
                    );
                }
                if !r.copy_ok {
                    log_diag(
                        "RGA2_SELFTEST_VERIFY_COPY",
                        core_index,
                        &r.copy_diag,
                        src.phys_addr(),
                        dst.phys_addr(),
                    );
                }
            }
            Err((e, d)) => {
                warn!("RGA2_SELFTEST core={} result=FAIL err={:?}", core_index, e);
                log_diag(
                    "RGA2_SELFTEST",
                    core_index,
                    &d,
                    src.phys_addr(),
                    dst.phys_addr(),
                );
            }
        }
        // Imported-buffer path: allocate via the real dma-heap allocator (exactly as a dma-buf
        // would be) and fill it with RGA2 — proving the dma-buf -> phys import seam end to end on
        // hardware. Board-gated (no RGA2 engine in QEMU).
        match crate::pseudofs::dev::dma_heap::alloc(bytes) {
            Ok(mut obj) => {
                // Distinctive probe: all four bytes distinct so the engine's channel/format
                // transform is unambiguous in the pixel dump below (0x00FF00FF was R/B-symmetric).
                let color: u32 = 0x1122_3344;
                // Poison + flush before the fill (same protocol as the smoke fill): the dma-heap
                // backing is CACHED, so cleaning the dirty alloc-zero/poison lines to DRAM before the
                // engine writes prevents an eviction from clobbering the output, and a surviving
                // poison after the op means the engine never wrote (NOWRITE) vs wrote zeros (ZERO).
                // Arc::get_mut succeeds: freshly allocated, not yet shared.
                if let Some(m) = Arc::get_mut(&mut obj) {
                    // SAFETY: slice not retained across the device submission; sync_for_device follows.
                    let b = unsafe { m.cpu_bytes_mut() };
                    for px in b.chunks_exact_mut(4) {
                        px.copy_from_slice(&SMOKE_FILL_POISON.to_le_bytes());
                    }
                }
                obj.sync_for_device();
                match run_rga2_fill_imported(core, obj.phys_addr(), W, H, color, |us| {
                    ax_runtime::hal::time::busy_wait(core::time::Duration::from_micros(us as u64))
                }) {
                    Ok(diag) => {
                        obj.sync_for_cpu();
                        let pixels = &obj.cpu_bytes()[..bytes];
                        let fill_ok = pixels
                            .chunks_exact(4)
                            .all(|px| u32::from_le_bytes([px[0], px[1], px[2], px[3]]) == color);
                        let n = bytes / 4;
                        let px = |i: usize| {
                            u32::from_le_bytes([
                                pixels[i * 4],
                                pixels[i * 4 + 1],
                                pixels[i * 4 + 2],
                                pixels[i * 4 + 3],
                            ])
                        };
                        let fill_px = [px(0), px(1), px(n / 2), px(n - 1)];
                        info!(
                            "RGA2_DMABUF_SELFTEST core={} fill={} class={} crc=0x{:08x}",
                            core_index,
                            if fill_ok { "PASS" } else { "FAIL" },
                            classify_fill(&fill_px, color, SMOKE_FILL_POISON),
                            crc32(pixels)
                        );
                        if !fill_ok {
                            // want vs px0 reveals the byte permutation / CSC; px0 vs pxmid/pxlast
                            // reveals uniform-vs-pattern; the poison classifier separates no-write.
                            warn!(
                                "RGA2_DMABUF_SELFTEST_PIX core={} want=0x{:08x} poison=0x{:08x} \
                                 px0=0x{:08x} px1=0x{:08x} pxmid=0x{:08x} pxlast=0x{:08x}",
                                core_index,
                                color,
                                SMOKE_FILL_POISON,
                                fill_px[0],
                                fill_px[1],
                                fill_px[2],
                                fill_px[3]
                            );
                            log_diag(
                                "RGA2_DMABUF_SELFTEST_VERIFY",
                                core_index,
                                &diag,
                                0,
                                obj.phys_addr(),
                            );
                        }
                    }
                    Err((e, d)) => {
                        warn!(
                            "RGA2_DMABUF_SELFTEST core={} fill=FAIL err={:?}",
                            core_index, e
                        );
                        log_diag("RGA2_DMABUF_SELFTEST", core_index, &d, 0, obj.phys_addr());
                    }
                }
            }
            Err(e) => warn!(
                "RGA2_DMABUF_SELFTEST core={} alloc=FAIL err={:?}",
                core_index, e
            ),
        }
        // Real-source resize: downscale a SOLID-color W×H src into (W/2)×(H/2). A uniform source
        // downscales to the SAME uniform color (any averaging/bicubic of equal samples == that
        // value), so `dst == color` is an exact pixel-correctness check — unlike the prior
        // zero-source resize, which only proved the op completed. Src is CPU-filled then flushed;
        // the Bidirectional dst is poisoned + cleaned so a FAIL is classifiable. RgaDmaBuffer (not
        // dma-heap) so the src is CPU-writable.
        let (dw, dh) = (W / 2, H / 2);
        let dst_bytes = (dw * dh * 4) as usize;
        let resize_color: u32 = SMOKE_FILL_COLOR;
        match (
            RgaDmaBuffer::alloc(&dma, bytes, DmaDirection::ToDevice),
            RgaDmaBuffer::alloc(&dma, dst_bytes, DmaDirection::Bidirectional),
        ) {
            (Ok(mut rs_src), Ok(mut rs_dst)) => {
                // SAFETY: slices are not retained across the device submission below.
                {
                    let s = unsafe { rs_src.cpu_bytes_mut() };
                    for px in s.chunks_exact_mut(4) {
                        px.copy_from_slice(&resize_color.to_le_bytes());
                    }
                }
                rs_src.prepare_for_device();
                {
                    let d = unsafe { rs_dst.cpu_bytes_mut() };
                    for px in d.chunks_exact_mut(4) {
                        px.copy_from_slice(&SMOKE_FILL_POISON.to_le_bytes());
                    }
                }
                rs_dst.prepare_for_device();
                match run_rga2_blit_resize(
                    core,
                    rs_src.phys_addr(),
                    (W, H),
                    rs_dst.phys_addr(),
                    (dw, dh),
                    |us| {
                        ax_runtime::hal::time::busy_wait(core::time::Duration::from_micros(
                            us as u64,
                        ))
                    },
                ) {
                    Ok(_diag) => {
                        rs_dst.complete_for_cpu();
                        let pixels = rs_dst.cpu_bytes();
                        let n = dst_bytes / 4;
                        let px = |i: usize| {
                            u32::from_le_bytes([
                                pixels[i * 4],
                                pixels[i * 4 + 1],
                                pixels[i * 4 + 2],
                                pixels[i * 4 + 3],
                            ])
                        };
                        let rpx = [px(0), px(1), px(n / 2), px(n - 1)];
                        let resize_ok = pixels
                            .chunks_exact(4)
                            .all(|p| u32::from_le_bytes([p[0], p[1], p[2], p[3]]) == resize_color);
                        info!(
                            "RGA2_BLIT_SELFTEST core={} resize={} class={} crc=0x{:08x}",
                            core_index,
                            if resize_ok { "PASS" } else { "FAIL" },
                            classify_fill(&rpx, resize_color, SMOKE_FILL_POISON),
                            crc32(pixels)
                        );
                        if !resize_ok {
                            warn!(
                                "RGA2_BLIT_SELFTEST_PIX core={} want=0x{:08x} poison=0x{:08x} \
                                 px0=0x{:08x} px1=0x{:08x} pxmid=0x{:08x} pxlast=0x{:08x}",
                                core_index,
                                resize_color,
                                SMOKE_FILL_POISON,
                                rpx[0],
                                rpx[1],
                                rpx[2],
                                rpx[3]
                            );
                        }
                    }
                    Err((e, d)) => {
                        warn!(
                            "RGA2_BLIT_SELFTEST core={} resize=FAIL err={:?}",
                            core_index, e
                        );
                        log_diag(
                            "RGA2_BLIT_SELFTEST",
                            core_index,
                            &d,
                            rs_src.phys_addr(),
                            rs_dst.phys_addr(),
                        );
                    }
                }
            }
            _ => warn!("RGA2_BLIT_SELFTEST core={} alloc=FAIL", core_index),
        }
        // Bitblt-based fill via the PROVEN copy/blit datapath: CPU-fill a src with the solid color,
        // then same-size blit src→dst (an equal-dims Blit encodes byte-identically to a Copy).
        // Doubles as the fill fallback implementation if native color_fill regresses. Poison + clean
        // the dst first (cached backing) so the engine's output is not clobbered and a FAIL is
        // classifiable.
        let blitfill_color: u32 = SMOKE_FILL_COLOR;
        match (
            RgaDmaBuffer::alloc(&dma, bytes, DmaDirection::ToDevice),
            RgaDmaBuffer::alloc(&dma, bytes, DmaDirection::Bidirectional),
        ) {
            (Ok(mut bf_src), Ok(mut bf_dst)) => {
                // SAFETY: slice not retained across the submission below.
                {
                    let d = unsafe { bf_dst.cpu_bytes_mut() };
                    for px in d.chunks_exact_mut(4) {
                        px.copy_from_slice(&SMOKE_FILL_POISON.to_le_bytes());
                    }
                }
                bf_dst.prepare_for_device();
                match run_rga2_fill_via_blit(
                    core,
                    &mut bf_src,
                    bf_dst.phys_addr(),
                    W,
                    H,
                    blitfill_color,
                    |us| {
                        ax_runtime::hal::time::busy_wait(core::time::Duration::from_micros(
                            us as u64,
                        ))
                    },
                ) {
                    Ok(_d) => {
                        bf_dst.complete_for_cpu();
                        let pixels = bf_dst.cpu_bytes();
                        let n = bytes / 4;
                        let px = |i: usize| {
                            u32::from_le_bytes([
                                pixels[i * 4],
                                pixels[i * 4 + 1],
                                pixels[i * 4 + 2],
                                pixels[i * 4 + 3],
                            ])
                        };
                        let bpx = [px(0), px(1), px(n / 2), px(n - 1)];
                        let ok = pixels.chunks_exact(4).all(|p| {
                            u32::from_le_bytes([p[0], p[1], p[2], p[3]]) == blitfill_color
                        });
                        info!(
                            "RGA2_FILLBLIT_SELFTEST core={} fill={} class={} crc=0x{:08x}",
                            core_index,
                            if ok { "PASS" } else { "FAIL" },
                            classify_fill(&bpx, blitfill_color, SMOKE_FILL_POISON),
                            crc32(pixels)
                        );
                        if !ok {
                            warn!(
                                "RGA2_FILLBLIT_SELFTEST_PIX core={} want=0x{:08x} poison=0x{:08x} \
                                 px0=0x{:08x} px1=0x{:08x} pxmid=0x{:08x} pxlast=0x{:08x}",
                                core_index,
                                blitfill_color,
                                SMOKE_FILL_POISON,
                                bpx[0],
                                bpx[1],
                                bpx[2],
                                bpx[3]
                            );
                        }
                    }
                    Err((e, d)) => {
                        warn!(
                            "RGA2_FILLBLIT_SELFTEST core={} fill=FAIL err={:?}",
                            core_index, e
                        );
                        log_diag(
                            "RGA2_FILLBLIT_SELFTEST",
                            core_index,
                            &d,
                            bf_src.phys_addr(),
                            bf_dst.phys_addr(),
                        );
                    }
                }
            }
            _ => warn!("RGA2_FILLBLIT_SELFTEST core={} alloc=FAIL", core_index),
        }
        // YUYV422 -> RGB888 CSC: the EXACT op the tennis app submits via librga imcvtcolor, on the
        // same RgaDmaBuffer datapath the resize test PASSES with. A FAIL here isolates the
        // YUV-packed-src + YUV->RGB CSC register encoding (vs the proven RGBA copy/resize path).
        match (
            RgaDmaBuffer::alloc(&dma, (W * H * 2) as usize, DmaDirection::ToDevice),
            RgaDmaBuffer::alloc(&dma, (W * H * 3) as usize, DmaDirection::Bidirectional),
        ) {
            (Ok(mut yuyv_src), Ok(mut rgb_dst)) => {
                // SAFETY: slices not retained across the submission below.
                {
                    // Mid-gray YUYV (Y=128, U=V=128) -> ~mid-gray RGB. Packed order Y0 U Y1 V.
                    let s = unsafe { yuyv_src.cpu_bytes_mut() };
                    for q in s.chunks_exact_mut(4) {
                        q.copy_from_slice(&[128, 128, 128, 128]);
                    }
                }
                yuyv_src.prepare_for_device();
                {
                    let d = unsafe { rgb_dst.cpu_bytes_mut() };
                    for b in d.iter_mut() {
                        *b = 0xAB; // poison: distinguishes NOWRITE from a real (wrong) write
                    }
                }
                rgb_dst.prepare_for_device();
                match run_rga2_csc_yuyv(
                    core,
                    yuyv_src.phys_addr(),
                    rgb_dst.phys_addr(),
                    W,
                    H,
                    |us| {
                        ax_runtime::hal::time::busy_wait(core::time::Duration::from_micros(
                            us as u64,
                        ))
                    },
                ) {
                    Ok(diag) => {
                        rgb_dst.complete_for_cpu();
                        let p = rgb_dst.cpu_bytes();
                        info!(
                            "RGA2_CSC_SELFTEST core={} result=PASS rgb[0..6]={},{},{},{},{},{}",
                            core_index, p[0], p[1], p[2], p[3], p[4], p[5]
                        );
                        log_diag(
                            "RGA2_CSC_SELFTEST",
                            core_index,
                            &diag,
                            yuyv_src.phys_addr(),
                            rgb_dst.phys_addr(),
                        );
                    }
                    Err((e, d)) => {
                        warn!(
                            "RGA2_CSC_SELFTEST core={} result=FAIL err={:?}",
                            core_index, e
                        );
                        log_diag(
                            "RGA2_CSC_SELFTEST",
                            core_index,
                            &d,
                            yuyv_src.phys_addr(),
                            rgb_dst.phys_addr(),
                        );
                    }
                }
            }
            _ => warn!("RGA2_CSC_SELFTEST core={} alloc=FAIL", core_index),
        }
        // Same YUYV->RGB CSC, but on /dev/dma_heap buffers (the app's exact buffer source)
        // instead of dma-api RgaDmaBuffer. Isolates the dma-heap pool's phys/coherency from
        // the handle-resolution path: if RGA2_CSC_DMABUF fails while RGA2_CSC_SELFTEST passes,
        // the dma-heap buffers are the problem; if both pass, the app's failure is in the
        // librga handle->resolve_buf->phys path, not the buffers.
        match (
            crate::pseudofs::dev::dma_heap::alloc((W * H * 2) as usize),
            crate::pseudofs::dev::dma_heap::alloc((W * H * 3) as usize),
        ) {
            (Ok(mut ysrc), Ok(mut rdst)) => {
                if let Some(m) = Arc::get_mut(&mut ysrc) {
                    // SAFETY: slice not retained across the device submission below.
                    let b = unsafe { m.cpu_bytes_mut() };
                    for q in b.chunks_exact_mut(4) {
                        q.copy_from_slice(&[128, 128, 128, 128]); // mid-gray YUYV
                    }
                }
                ysrc.sync_for_device();
                if let Some(m) = Arc::get_mut(&mut rdst) {
                    let b = unsafe { m.cpu_bytes_mut() };
                    for x in b.iter_mut() {
                        *x = 0xAB; // poison
                    }
                }
                rdst.sync_for_device();
                match run_rga2_csc_yuyv(core, ysrc.phys_addr(), rdst.phys_addr(), W, H, |us| {
                    ax_runtime::hal::time::busy_wait(core::time::Duration::from_micros(us as u64))
                }) {
                    Ok(diag) => {
                        rdst.sync_for_cpu();
                        let p = rdst.cpu_bytes();
                        info!(
                            "RGA2_CSC_DMABUF core={} result=PASS rgb[0..6]={},{},{},{},{},{}",
                            core_index, p[0], p[1], p[2], p[3], p[4], p[5]
                        );
                        log_diag(
                            "RGA2_CSC_DMABUF",
                            core_index,
                            &diag,
                            ysrc.phys_addr(),
                            rdst.phys_addr(),
                        );
                    }
                    Err((e, d)) => {
                        warn!(
                            "RGA2_CSC_DMABUF core={} result=FAIL err={:?}",
                            core_index, e
                        );
                        log_diag(
                            "RGA2_CSC_DMABUF",
                            core_index,
                            &d,
                            ysrc.phys_addr(),
                            rdst.phys_addr(),
                        );
                    }
                }
            }
            _ => warn!("RGA2_CSC_DMABUF core={} alloc=FAIL", core_index),
        }
        // Completion path: PR-1 is polling-only (poll the RGA2 INT status register), which works
        // regardless of GIC routing. This board has a confirmed FDT->GIC gap (the dwmmc completion
        // IRQ never fires), so the RGA completion IRQ likely never fires either; confirming that is a
        // Phase B probe. Log the path explicitly so the board run reports it.
        info!("RGA2_SELFTEST completion=POLLED");
        // End-of-suite sentinel: the board success_regex matches THIS line, printed only after all
        // three selftests have run — never an intermediate PASS line, because ostool tears down on
        // the first success_regex match (which would otherwise drop the dmabuf/blit results).
        info!("RGA_SELFTEST_SUITE_DONE");
        return;
    }
    warn!("RGA2_SELFTEST no-rga2-core");
}
