// SPDX-License-Identifier: GPL-2.0-or-later
// Register sequences derived from the CVITEK ADC/I2S drivers:
// Copyright 2020 CVITEK Inc.; Copyright 2018 CVITEK (author: EthanChen).
use crate::{Error, Mmio};

pub(super) struct Frontend {
    adc: Mmio,
    dac: Mmio,
    mclk: Mmio,
    crg: Mmio,
    oscillator_hz: u32,
}

impl Frontend {
    pub fn new(
        adc: Mmio,
        dac: Mmio,
        mclk: Mmio,
        aiao: Mmio,
        crg: Mmio,
        reset: Mmio,
        oscillator_hz: u32,
    ) -> Result<Self, Error> {
        if adc.size() < 0x20
            || dac.size() < 0x24
            || mclk.size() < 0x68
            || aiao.size() < 0x64
            || crg.size() < 0x858
            || reset.size() < 0xc
            || oscillator_hz == 0
        {
            return Err(Error::Invalid);
        }
        // Shared gate/bypass/reset RMWs happen only during serialized platform
        // probing. Runtime touches dedicated audio dividers and ADC registers.
        update(&crg, 0x004, 0, (1 << 1) | (1 << 2) | (1 << 5));
        update(&crg, 0x030, (1 << 11) | (1 << 14), 0);
        update(&reset, 8, 1 << 29, 0);
        mbarrier::mb();
        update(&reset, 8, 0, 1 << 29);
        for offset in [0, 4, 8] {
            update(&aiao, offset, 7, 4);
        }
        update(&aiao, 0x60, 0, 1); // Subsystem IRQ gate for I2S0.
        Ok(Self {
            adc,
            dac,
            mclk,
            crg,
            oscillator_hz,
        })
    }

    pub fn prepare(
        &self,
        i2s: &Mmio,
        rate: u32,
        gain: u8,
        now_ns: impl Fn() -> u64,
    ) -> Result<(), Error> {
        self.disable();
        let source = match rate {
            16_000 => 16_384_000,
            48_000 => 24_576_000,
            _ => return Err(Error::Invalid),
        };
        if self.crg.read::<u32>(0x804) & 3 != 0 {
            return Err(Error::Clock);
        }
        let div = audio_divider(
            self.oscillator_hz,
            self.crg.read(0x808),
            self.crg.read(0x80c),
            self.crg.read(0x840),
            self.crg.read(0x854),
            source,
        )
        .ok_or(Error::Clock)?;
        for offset in [0x98, 0xa4] {
            update(&self.crg, offset, 0x00ff_0308, (div << 16) | 8);
        }
        let (mclk_div, cic, sck, delay, ctune) = if rate == 16_000 {
            (1, 2, 15, 0x21, 8)
        } else {
            (2, 0, 3, 0x19, 12)
        };
        update(&self.mclk, 0x64, 0xffff, mclk_div);
        update(&self.mclk, 0x60, 1, (1 << 7) | (1 << 8));
        update(&self.dac, 0x20, 1, 0); // ADC gain-ratio ECO is in DAC ANA0.
        // Keep the reset DC-blocking filter. SINGLE is for differential analog
        // input, not the mono PCM format; the board uses single-ended left input.
        update(&self.adc, 4, 0xf, 0x100 | cic);
        update(&self.adc, 0x0c, 0x00ff_ff00, (sck << 8) | (delay << 16));
        update(&self.adc, 0x1c, 0xf00, ctune << 8);
        update(&self.adc, 0x18, 0x30003, 0); // Single-ended, unmuted microphone input.
        self.set_gain(gain)?;
        i2s.write(0x00, 0x84u32); // Slave RX, positive sample edge, hardware DMA.
        i2s.write(0x04, 15u32 | (1 << 13) | (15 << 16));
        i2s.write(0x08, (15u32 << 8) | (15 << 16));
        i2s.write(0x0c, 1u32);
        i2s.write(0x10, 2u32); // Packed 16-bit words in memory.
        i2s.write(0x28, 7u32 | (7 << 16) | (31 << 24));
        update(i2s, 0x60, 1, 1 << 8);
        self.adc.write(0, self.adc.read::<u32>(0) | 3);
        // ADC supplies BCLK; resetting I2S before enabling ADC cannot complete.
        i2s.write(0x30, 1u32);
        i2s.write(0x30, 0u32);
        i2s.write(0x1c, 1u32);
        let start = now_ns();
        while i2s.read::<u32>(0x40) & (1 << 23) == 0 {
            if now_ns().saturating_sub(start) >= 10_000_000 {
                i2s.write(0x1c, 0u32);
                self.disable();
                return Err(Error::Timeout);
            }
            core::hint::spin_loop();
        }
        i2s.write(0x1c, 0u32);
        Ok(())
    }

    pub fn disable(&self) {
        update(&self.adc, 0, 3, 0);
    }

    pub fn set_gain(&self, gain: u8) -> Result<(), Error> {
        let bits = match gain {
            0..=12 => 1u32 << gain,
            13..=24 => [
                0x2400, 0x2800, 0x3000, 0x6400, 0x6800, 0x7000, 0xa400, 0xa800, 0xb000, 0xe400,
                0xe800, 0xf000,
            ][usize::from(gain - 13)],
            _ => return Err(Error::Invalid),
        };
        update(&self.adc, 0x10, 0xffff, bits);
        Ok(())
    }
}

fn update(mmio: &Mmio, offset: usize, clear: u32, set: u32) {
    mmio.write(offset, (mmio.read::<u32>(offset) & !clear) | set);
}

fn pll_rate(parent: u64, csr: u32) -> Option<u64> {
    let pre = u64::from(csr & 0x7f);
    let post = u64::from((csr >> 8) & 0x7f);
    parent
        .checked_mul(u64::from((csr >> 17) & 0x7f))?
        .checked_div(pre * post)
}

fn audio_divider(
    oscillator: u32,
    mipi: u32,
    apll: u32,
    synth_control: u32,
    synth_set: u32,
    source: u32,
) -> Option<u32> {
    let parent = pll_rate(u64::from(oscillator), mipi)?;
    let reference = if synth_control & 1 == 0 {
        parent / 2
    } else {
        parent
    };
    let reference = reference
        .checked_mul(1 << 26)?
        .checked_div(u64::from(synth_set))?;
    let rate = pll_rate(reference, apll)?;
    let divisor = rate.checked_div(u64::from(source))?;
    (rate.is_multiple_of(u64::from(source)) && (1..=255).contains(&divisor))
        .then_some(divisor as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dividers_follow_both_firmware_pll_profiles_without_reprogramming_pll() {
        for (apll, expected) in [(0x0012_8201, [27, 18]), (0x0020_8201, [48, 32])] {
            for (source, div) in [16_384_000, 24_576_000].into_iter().zip(expected) {
                assert_eq!(
                    audio_divider(25_000_000, 0x0548_8101, apll, 1, 614_400_000, source),
                    Some(div)
                );
            }
        }
        assert_eq!(
            audio_divider(
                24_000_000,
                0x0548_8101,
                0x0012_8201,
                1,
                614_400_000,
                16_384_000
            ),
            None
        );
        assert_eq!(audio_divider(25_000_000, 0, 0, 0, 0, 16_384_000), None);
    }
}
