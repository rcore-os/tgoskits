// SPDX-License-Identifier: GPL-2.0-or-later
use dma_api::{CoherentArray, DeviceDma};

use crate::{Config, Error, Shared};

// Channel error status, excluding transaction/period completion and suspend.
pub(super) const ERRORS: u64 = 0x0001_7fe0 | (1 << 31);
const BLOCK_BYTES: usize = Config::DMA_BLOCK_FRAMES as usize * 2;

#[derive(Clone, Copy, Default)]
#[repr(C, align(64))]
struct Descriptor {
    source: u64,
    destination: u64,
    block_ts: u64,
    next: u64,
    control: u64,
    source_status: u32,
    destination_status: u32,
    writeback: u64,
    reserved: u64,
}

pub(super) struct Ring {
    pub config: Config,
    samples: CoherentArray<i16>,
    descriptors: CoherentArray<Descriptor>,
}

impl Ring {
    pub fn new(allocator: &DeviceDma, shared: &Shared, config: Config) -> Result<Self, Error> {
        let samples =
            allocator.coherent_array_zero_with_align(config.buffer_frames as usize, 64)?;
        let count = config.buffer_frames as usize * 2 / BLOCK_BYTES;
        let mut descriptors = allocator.coherent_array_zero_with_align::<Descriptor>(count, 64)?;
        let route = shared.route;
        let control = u64::from(route.peripheral_master)
            | (u64::from(route.memory_master) << 2)
            | (1 << 4)
            | (2 << 8)
            | (2 << 11)
            | (1 << 14)
            | (1 << 18)
            | (1 << 38)
            | (1 << 47)
            | (7 << 48)
            | (1 << 63);
        for i in 0..count {
            let period_end =
                ((i + 1) * BLOCK_BYTES).is_multiple_of(config.period_frames as usize * 2);
            descriptors.set_cpu(
                i,
                Descriptor {
                    source: shared.i2s.phys_addr().as_usize() as u64 + 0x80,
                    destination: samples.dma_addr().as_u64() + (i * BLOCK_BYTES) as u64,
                    block_ts: (BLOCK_BYTES / 4 - 1) as u64,
                    next: descriptors.dma_addr().as_u64() + (((i + 1) % count) * 64) as u64,
                    control: control | (u64::from(period_end) << 58),
                    ..Default::default()
                },
            );
        }
        Ok(Self {
            config,
            samples,
            descriptors,
        })
    }

    pub fn start(&self, shared: &Shared) {
        let route = shared.route;
        let config =
            3 | (3 << 2) | (2 << 32) | (u64::from(route.request) << 39) | (15 << 55) | (15 << 59);
        shared.dma.write(shared.channel_reg(0x20), config);
        shared.dma.write(
            shared.channel_reg(0x28),
            self.descriptors.dma_addr().as_u64(),
        );
        shared
            .dma
            .write(shared.channel_reg(0x08), self.samples.dma_addr().as_u64());
        shared.dma.write(shared.channel_reg(0x98), u64::MAX);
        shared.dma.write(shared.channel_reg(0x80), ERRORS | 1);
        shared.dma.write(shared.channel_reg(0x90), ERRORS | 1);
        mbarrier::mb();
        shared.dma.write(0x10, shared.dma.read::<u64>(0x10) | 3);
        shared.dma.write(
            0x18,
            (1u64 << route.channel) | (1u64 << (route.channel + 8)),
        );
    }

    pub fn position(&self, shared: &Shared) -> Result<u32, Error> {
        let address = shared.dma.read::<u64>(shared.channel_reg(0x08));
        let offset = address
            .checked_sub(self.samples.dma_addr().as_u64())
            .ok_or(Error::Overrun)?;
        if offset > self.samples.bytes_len() as u64 {
            return Err(Error::Overrun);
        }
        mbarrier::mb();
        // Exclude the in-progress block, including its outstanding AXI writes.
        Ok(((offset as usize / BLOCK_BYTES * BLOCK_BYTES) % self.samples.bytes_len() / 2) as u32)
    }

    pub fn copy(&self, frame: u64, output: &mut [i16]) {
        for (i, sample) in output.iter_mut().enumerate() {
            let index =
                (frame as usize % self.samples.len() + i % self.samples.len()) % self.samples.len();
            // SAFETY: coherent allocation stays owned by Ring; index is in
            // bounds and i16-aligned. No reference into device-written memory
            // is created. The caller validates against wraparound after copy.
            *sample = unsafe { self.samples.as_ptr().add(index).read_volatile() };
        }
        mbarrier::mb();
    }
}

pub(super) fn stop(shared: &Shared) -> bool {
    let channel = shared.route.channel;
    shared.dma.write(shared.channel_reg(0x90), 0u64);
    // Write-enable bits address only our channel; never RMW CH_EN or reset DMAC.
    shared.dma.write(
        0x18,
        (1u64 << (channel + 8)) | (1u64 << (channel + 32)) | (1u64 << (channel + 40)),
    );
    for _ in 0..100_000 {
        if shared.dma.read::<u64>(0x18) & (1 << channel) == 0 {
            mbarrier::mb();
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

#[derive(Default)]
pub(super) struct Cursor {
    pub frames: u64,
    pub last_ns: u64,
    position: u32,
}

impl Cursor {
    pub fn advance(&mut self, position: u32, now_ns: u64, config: Config) -> Result<(), Error> {
        let elapsed = now_ns.checked_sub(self.last_ns).ok_or(Error::Overrun)?;
        // Subtract one DMA block for the unobserved partial block at last sample.
        let safe_gap = u64::from(config.buffer_frames - Config::DMA_BLOCK_FRAMES) * 1_000_000_000
            / u64::from(config.rate);
        if elapsed >= safe_gap {
            return Err(Error::Overrun);
        }
        self.frames +=
            u64::from((position + config.buffer_frames - self.position) % config.buffer_frames);
        self.position = position;
        self.last_ns = now_ns;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_accumulates_coalesced_progress_and_rejects_ambiguous_laps() {
        let config = Config {
            rate: 16_000,
            period_frames: 128,
            buffer_frames: 1024,
        };
        let mut cursor = Cursor::default();
        cursor.advance(768, 48_000_000, config).unwrap();
        cursor.advance(128, 72_000_000, config).unwrap();
        assert_eq!(cursor.frames, 1152);
        assert!(matches!(
            cursor.advance(128, 132_000_000, config),
            Err(Error::Overrun)
        ));
        assert_eq!(cursor.frames, 1152);

        // Every accepted ring must permit an observation at the first period
        // interrupt, rounded up to the next nanosecond, without an ambiguous lap.
        for config in Config::supported() {
            let mut cursor = Cursor::default();
            let period_ns =
                (u64::from(config.period_frames) * 1_000_000_000).div_ceil(u64::from(config.rate));
            cursor
                .advance(config.period_frames, period_ns, config)
                .expect("accepted geometry must allow one period of progress");
            assert_eq!(cursor.frames, u64::from(config.period_frames));
        }
    }
}
