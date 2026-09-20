//! PWM clock slice of CV181x's vendor Linux 5.10 clock tree.
//! PLLs, muxes and dividers are read-only; only PWM gates may be enabled.
#![no_std]
pub use mmio_api::MmioRaw;
use rdif_clk::{ClockId, DriverGeneric, Interface, KError};
use tock_registers::{
    LocalRegisterCopy,
    interfaces::{ReadWriteable, Readable},
    register_bitfields, register_structs,
    registers::{ReadOnly, ReadWrite},
};

pub const PWM: usize = 51;
pub const PWM_SRC: usize = 143;
pub const MMIO_SIZE: usize = 0x1000;
register_structs! {
    Registers {
        (0x000 => _reserved0),
        (0x004 => enable1: ReadWrite<u32, Enable1::Register>),
        (0x008 => _reserved1),
        (0x010 => enable4: ReadWrite<u32, Enable4::Register>),
        (0x014 => _reserved2),
        (0x030 => bypass: ReadOnly<u32, Bypass::Register>),
        (0x034 => _reserved3),
        (0x120 => pwm_src: ReadOnly<u32, PwmSource::Register>),
        (0x124 => _reserved4),
        (0x808 => mipimpll: ReadOnly<u32>),
        (0x80c => _reserved5),
        (0x810 => disppll: ReadOnly<u32>),
        (0x814 => _reserved6),
        (0x840 => synthesizer: ReadOnly<u32, Synthesizer::Register>),
        (0x844 => _reserved7),
        (0x864 => disppll_set: ReadOnly<u32>),
        (0x868 => _reserved8),
        (0x910 => fpll: ReadOnly<u32>),
        (0x914 => @END),
    }
}
register_bitfields![u32,
    Enable1 [PWM OFFSET(8) NUMBITS(1) []],
    Enable4 [PWM_SRC OFFSET(4) NUMBITS(1) []],
    Bypass [PWM OFFSET(15) NUMBITS(1) []],
    PwmSource [DIV_ENABLE OFFSET(3) NUMBITS(1) [], SELECT OFFSET(8) NUMBITS(2) [], DIVISOR OFFSET(16) NUMBITS(6) []],
    Synthesizer [FULL_RATE OFFSET(0) NUMBITS(1) []],
    Pll [PRE_DIVISOR OFFSET(0) NUMBITS(7) [], POST_DIVISOR OFFSET(8) NUMBITS(7) [], MULTIPLIER OFFSET(17) NUMBITS(7) []]
];

/// Owns PWM gates; all rate-setting registers remain firmware-owned and read-only.
pub struct Cv181xClock {
    mmio: MmioRaw,
    oscillator_hz: u64,
}
impl Cv181xClock {
    /// # Safety
    /// The mapping is device memory exclusively owned for this object's lifetime.
    /// Other clock-register writers must be serialized with this owner.
    pub unsafe fn new(mmio: MmioRaw, oscillator_hz: u64) -> Result<Self, KError> {
        if mmio.size() < MMIO_SIZE || mmio.as_ptr().align_offset(4) != 0 || oscillator_hz == 0 {
            return Err(KError::InvalidArg {
                name: "clock configuration",
            });
        }
        Ok(Self {
            mmio,
            oscillator_hz,
        })
    }
    fn regs(&self) -> &Registers {
        // SAFETY: mapping ownership/lifetime is required by new; size and alignment are checked.
        unsafe { &*self.mmio.as_ptr().cast::<Registers>() }
    }
    fn pwm_rate(&self) -> Result<u64, KError> {
        let regs = self.regs();
        if regs.bypass.is_set(Bypass::PWM) {
            return Ok(self.oscillator_hz);
        }
        let config = regs.pwm_src.extract();
        let parent = match config.read(PwmSource::SELECT) {
            0 => pll_rate(regs.fpll.get(), self.oscillator_hz)?,
            1 => {
                let mipi = pll_rate(regs.mipimpll.get(), self.oscillator_hz)?;
                let reference = if regs.synthesizer.is_set(Synthesizer::FULL_RATE) {
                    mipi
                } else {
                    mipi >> 1
                };
                let denominator = regs.disppll_set.get();
                if denominator == 0 {
                    return Err(KError::InvalidArg {
                        name: "clock configuration",
                    });
                }
                let reference =
                    u64::try_from((u128::from(reference) << 26) / u128::from(denominator))
                        .map_err(|_| KError::InvalidArg {
                            name: "clock configuration",
                        })?;
                pll_rate(regs.disppll.get(), reference)?
            }
            _ => {
                return Err(KError::InvalidArg {
                    name: "clock configuration",
                });
            }
        };
        // BSP reset divider is 10 until DIV_EN (bit 3) is programmed.
        let divisor = if !config.is_set(PwmSource::DIV_ENABLE) {
            10
        } else {
            config.read(PwmSource::DIVISOR)
        };
        // CLK_DIVIDER_ONE_BASED | ALLOW_ZERO: zero means passthrough.
        Ok(parent.div_ceil(u64::from(divisor.max(1))))
    }
}
fn pll_rate(csr: u32, parent: u64) -> Result<u64, KError> {
    let csr = LocalRegisterCopy::<u32, Pll::Register>::new(csr);
    let denominator = csr.read(Pll::PRE_DIVISOR) * csr.read(Pll::POST_DIVISOR);
    if denominator == 0 {
        return Err(KError::InvalidArg {
            name: "clock configuration",
        });
    }
    let rate = u64::try_from(
        u128::from(parent) * u128::from(csr.read(Pll::MULTIPLIER)) / u128::from(denominator),
    )
    .map_err(|_| KError::InvalidArg {
        name: "clock configuration",
    })?;
    if rate == 0 {
        return Err(KError::InvalidArg {
            name: "clock configuration",
        });
    }
    Ok(rate)
}
impl DriverGeneric for Cv181xClock {
    fn name(&self) -> &str {
        "cv181x-pwm-clock"
    }
}
impl Interface for Cv181xClock {
    fn perper_enable(&mut self) {}
    fn enable(&mut self, id: ClockId) -> Result<(), KError> {
        self.get_rate(id)?;
        let regs = self.regs();
        regs.enable4.modify(Enable4::PWM_SRC::SET);
        if id == ClockId::from(PWM) {
            regs.enable1.modify(Enable1::PWM::SET);
        }
        Ok(())
    }
    fn get_rate(&self, id: ClockId) -> Result<u64, KError> {
        if id != ClockId::from(PWM) && id != ClockId::from(PWM_SRC) {
            return Err(KError::InvalidArg {
                name: "unsupported clock operation",
            });
        }
        self.pwm_rate()
    }
    fn set_rate(&mut self, id: ClockId, rate: u64) -> Result<(), KError> {
        if self.get_rate(id)? == rate {
            Ok(())
        } else {
            Err(KError::InvalidArg {
                name: "unsupported clock operation",
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bsp_parent_divider_and_gates() {
        let mut regs = [0u32; MMIO_SIZE / 4];
        regs[0x910 / 4] = (40 << 17) | (1 << 8) | 1;
        // SAFETY: aligned exclusive register storage remains live for the test.
        let mut clk = unsafe {
            Cv181xClock::new(
                MmioRaw::new(
                    0usize.into(),
                    core::ptr::NonNull::new(regs.as_mut_ptr().cast()).unwrap(),
                    MMIO_SIZE,
                ),
                25_000_000,
            )
            .unwrap()
        };
        assert_eq!(clk.get_rate(PWM.into()), Ok(100_000_000));
        let pll = regs[0x910 / 4];
        clk.enable(PWM.into()).unwrap();
        assert_eq!(regs[1], 1 << 8);
        assert_eq!(regs[4], 1 << 4);
        regs[0x120 / 4] = (5 << 16) | 8;
        assert_eq!(clk.get_rate(PWM.into()), Ok(200_000_000));
        regs[0x030 / 4] = 1 << 15;
        assert_eq!(clk.get_rate(PWM.into()), Ok(25_000_000));
        assert_eq!(regs[0x910 / 4], pll);
        regs[0x030 / 4] = 0;
        regs[0x120 / 4] = (5 << 16) | (1 << 8) | 8;
        regs[0x808 / 4] = (40 << 17) | (1 << 8) | 1;
        regs[0x840 / 4] = 1;
        regs[0x864 / 4] = 1 << 26;
        regs[0x810 / 4] = (2 << 17) | (4 << 8) | 1;
        assert_eq!(clk.get_rate(PWM.into()), Ok(100_000_000));
        regs[0x864 / 4] = 0;
        assert!(clk.get_rate(PWM.into()).is_err());
        assert!(clk.set_rate(PWM.into(), 1).is_err());
        assert!(clk.enable(999usize.into()).is_err());
    }
}
