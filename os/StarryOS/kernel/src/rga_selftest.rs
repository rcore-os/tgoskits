//! Feature-gated RGA2 bring-up selftest. Logs one machine-parseable line over serial so the board
//! harness can match it. No /dev/rga involved.
use dma_api::DmaDirection;
use rockchip_rga::{RgaVersion, RockchipRga, buffer::RgaDmaBuffer, selftest::run_rga2_smoke};

const W: u32 = 64;
const H: u32 = 48;

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
        let (mut src, mut dst) = match (
            RgaDmaBuffer::alloc(&dma, bytes, DmaDirection::ToDevice),
            RgaDmaBuffer::alloc(&dma, bytes, DmaDirection::FromDevice),
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
            Ok(r) => info!(
                "RGA2_SELFTEST core={} fill={} copy={} crc=0x{:08x}",
                core_index,
                if r.fill_ok { "PASS" } else { "FAIL" },
                if r.copy_ok { "PASS" } else { "FAIL" },
                r.crc
            ),
            Err(e) => warn!("RGA2_SELFTEST core={} result=FAIL err={:?}", core_index, e),
        }
        return;
    }
    warn!("RGA2_SELFTEST no-rga2-core");
}
