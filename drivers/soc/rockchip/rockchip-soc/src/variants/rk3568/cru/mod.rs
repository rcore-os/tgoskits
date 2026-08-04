use crate::{
    ClockError, ClockResult, Mmio, ResetRockchip, RstId,
    clock::{ClkId, ClockOp, ResetOp},
    variants::MHZ,
};

mod assignment;

const OSC_HZ: u64 = 24 * MHZ;

const CLKSEL_CON_OFFSET: u32 = 0x0100;
const CLKGATE_CON_OFFSET: u32 = 0x0300;
const SOFTRST_CON_OFFSET: u32 = 0x0400;
const EMMC_CON_OFFSET: u32 = 0x0598;

const CCLK_EMMC: ClkId = ClkId::new(0x7c);
const BCLK_EMMC: ClkId = ClkId::new(0x7b);
const ACLK_EMMC: ClkId = ClkId::new(0x79);
const HCLK_EMMC: ClkId = ClkId::new(0x7a);
const TCLK_EMMC: ClkId = ClkId::new(0x7d);
const HCLK_SDMMC0: ClkId = ClkId::new(0xb0);
const CLK_SDMMC0: ClkId = ClkId::new(0xb1);
const HCLK_SDMMC1: ClkId = ClkId::new(0xb2);
const CLK_SDMMC1: ClkId = ClkId::new(0xb3);
const HCLK_SDMMC2: ClkId = ClkId::new(0xc1);
const CLK_SDMMC2: ClkId = ClkId::new(0xc2);
const SCLK_SDMMC0_DRV: ClkId = ClkId::new(0x18a);
const SCLK_SDMMC0_SAMPLE: ClkId = ClkId::new(0x18b);
const SCLK_SDMMC1_DRV: ClkId = ClkId::new(0x18c);
const SCLK_SDMMC1_SAMPLE: ClkId = ClkId::new(0x18d);
const SCLK_SDMMC2_DRV: ClkId = ClkId::new(0x18e);
const SCLK_SDMMC2_SAMPLE: ClkId = ClkId::new(0x18f);
const EMMC_RESETS: [RstId; 5] = [
    RstId::new(0x78),
    RstId::new(0x76),
    RstId::new(0x75),
    RstId::new(0x77),
    RstId::new(0x79),
];

const CLKGATE_CON09: u32 = 9;
const CLKGATE_CON15: u32 = 15;
const CLKGATE_CON17: u32 = 17;
const GATE_ACLK_EMMC: u32 = bit(5);
const GATE_HCLK_EMMC: u32 = bit(6);
const GATE_BCLK_EMMC: u32 = bit(7);
const GATE_CCLK_EMMC: u32 = bit(8);
const GATE_TCLK_EMMC: u32 = bit(9);

const CLKSEL_CON28: u32 = 28;
const CCLK_EMMC_SEL_SHIFT: u32 = 12;
const CCLK_EMMC_SEL_MASK: u32 = 0x7 << CCLK_EMMC_SEL_SHIFT;
const CCLK_EMMC_XIN_SOC0: u32 = 0x0 << CCLK_EMMC_SEL_SHIFT;
const CCLK_EMMC_GPLL_200M: u32 = 0x1 << CCLK_EMMC_SEL_SHIFT;
const CCLK_EMMC_GPLL_150M: u32 = 0x2 << CCLK_EMMC_SEL_SHIFT;
const CCLK_EMMC_CPLL_100M: u32 = 0x3 << CCLK_EMMC_SEL_SHIFT;
const CCLK_EMMC_CPLL_50M: u32 = 0x4 << CCLK_EMMC_SEL_SHIFT;
const CCLK_EMMC_XIN_375K: u32 = 0x5 << CCLK_EMMC_SEL_SHIFT;

const BCLK_EMMC_SEL_SHIFT: u32 = 8;
const BCLK_EMMC_SEL_MASK: u32 = 0x3 << BCLK_EMMC_SEL_SHIFT;
const BCLK_EMMC_GPLL_200M: u32 = 0x0 << BCLK_EMMC_SEL_SHIFT;

const SDMMC_SEL_MASK: u32 = 0x7;
const SDMMC_XIN24M: u32 = 0;
const SDMMC_GPLL_400M: u32 = 1;
const SDMMC_GPLL_300M: u32 = 2;
const SDMMC_CPLL_100M: u32 = 3;
const SDMMC_CPLL_50M: u32 = 4;
const SDMMC_OSC0_DIV_750K: u32 = 5;

const EMMC_DELAYNUM: u32 = 0x20;
const EMMC_CON0_DRV_ENABLE: u32 = bit(11);
const EMMC_CON0_DRV_DELAYNUM_SHIFT: u32 = 3;
const EMMC_CON0_DRV_DELAYNUM_MASK: u32 = 0xff << EMMC_CON0_DRV_DELAYNUM_SHIFT;
const EMMC_CON0_DRV_DEGREE_SHIFT: u32 = 1;
const EMMC_CON0_DRV_DEGREE_MASK: u32 = 0x3 << EMMC_CON0_DRV_DEGREE_SHIFT;
const EMMC_CON1_SAMPLE_ENABLE: u32 = bit(10);
const EMMC_CON1_SAMPLE_DELAYNUM_SHIFT: u32 = 2;
const EMMC_CON1_SAMPLE_DELAYNUM_MASK: u32 = 0xff << EMMC_CON1_SAMPLE_DELAYNUM_SHIFT;
const EMMC_CON1_SAMPLE_DEGREE_SHIFT: u32 = 0;
const EMMC_CON1_SAMPLE_DEGREE_MASK: u32 = 0x3 << EMMC_CON1_SAMPLE_DEGREE_SHIFT;

const fn bit(bit: u32) -> u32 {
    1 << bit
}

const fn clksel_con(index: u32) -> u32 {
    CLKSEL_CON_OFFSET + index * 4
}

const fn clkgate_con(index: u32) -> u32 {
    CLKGATE_CON_OFFSET + index * 4
}

const fn emmc_con(index: u32) -> u32 {
    EMMC_CON_OFFSET + index * 4
}

const fn rate_selector(rate_hz: u64) -> Option<(u32, u64)> {
    match rate_hz {
        375_000..=400_000 => Some((CCLK_EMMC_XIN_375K, 375_000)),
        400_001..=24_000_000 => Some((CCLK_EMMC_XIN_SOC0, OSC_HZ)),
        24_000_001..=50_000_000 => Some((CCLK_EMMC_CPLL_50M, 50 * MHZ)),
        50_000_001..=150_000_000 => Some((CCLK_EMMC_CPLL_100M, 100 * MHZ)),
        150_000_001..=200_000_000 => Some((CCLK_EMMC_GPLL_200M, 200 * MHZ)),
        _ => None,
    }
}

const fn fixed_emmc_rate(id: ClkId) -> Option<u64> {
    match id {
        BCLK_EMMC | ACLK_EMMC | HCLK_EMMC => Some(200 * MHZ),
        TCLK_EMMC => Some(OSC_HZ),
        _ => None,
    }
}

#[derive(Clone, Copy)]
struct ClockGate {
    register: u32,
    mask: u32,
}

#[derive(Clone, Copy)]
struct SdMmcClock {
    selector_register: u32,
    selector_shift: u32,
}

const fn sdmmc_clock(id: ClkId) -> Option<SdMmcClock> {
    match id {
        CLK_SDMMC0 => Some(SdMmcClock {
            selector_register: 30,
            selector_shift: 8,
        }),
        CLK_SDMMC1 => Some(SdMmcClock {
            selector_register: 30,
            selector_shift: 12,
        }),
        CLK_SDMMC2 => Some(SdMmcClock {
            selector_register: 32,
            selector_shift: 8,
        }),
        _ => None,
    }
}

const fn sdmmc_phase_parent(id: ClkId) -> Option<ClkId> {
    match id {
        SCLK_SDMMC0_DRV | SCLK_SDMMC0_SAMPLE => Some(CLK_SDMMC0),
        SCLK_SDMMC1_DRV | SCLK_SDMMC1_SAMPLE => Some(CLK_SDMMC1),
        SCLK_SDMMC2_DRV | SCLK_SDMMC2_SAMPLE => Some(CLK_SDMMC2),
        _ => None,
    }
}

fn sdmmc_rate_selector(rate_hz: u64) -> Option<(u32, u64)> {
    const CANDIDATES: [(u32, u64); 6] = [
        (SDMMC_OSC0_DIV_750K, 750_000),
        (SDMMC_XIN24M, OSC_HZ),
        (SDMMC_CPLL_50M, 50 * MHZ),
        (SDMMC_CPLL_100M, 100 * MHZ),
        (SDMMC_GPLL_300M, 300 * MHZ),
        (SDMMC_GPLL_400M, 400 * MHZ),
    ];

    if rate_hz == 0 || rate_hz > 400 * MHZ {
        return None;
    }
    CANDIDATES
        .into_iter()
        .min_by_key(|(_, candidate_hz)| candidate_hz.abs_diff(rate_hz))
}

#[derive(Clone)]
pub struct Cru {
    base: usize,
    reset: ResetRockchip,
}

impl Cru {
    pub fn new(base: Mmio, _sys_grf: Mmio) -> Self {
        let base = base.as_ptr() as usize;
        Self {
            base,
            reset: ResetRockchip::new(base + SOFTRST_CON_OFFSET as usize, 512),
        }
    }

    pub fn init_emmc(&mut self) {
        self.enable_emmc_gates();
        self.deassert_emmc_resets();
        self.write_clksel(CCLK_EMMC_SEL_MASK, CCLK_EMMC_GPLL_200M);
        self.write_clksel(BCLK_EMMC_SEL_MASK, BCLK_EMMC_GPLL_200M);
        self.configure_emmc_delay();
    }

    pub fn clk_enable(&mut self, id: ClkId) -> ClockResult<()> {
        if sdmmc_phase_parent(id).is_some() {
            // The Linux MMC phase clocks have no independent gate. Their
            // parent card clock is enabled through its own FDT clock line.
            return Ok(());
        }
        let gate = clock_gate(id).ok_or_else(|| ClockError::unsupported(id))?;
        self.clrreg(clkgate_con(gate.register), gate.mask);
        Ok(())
    }

    pub fn clk_disable(&mut self, id: ClkId) -> ClockResult<()> {
        if sdmmc_phase_parent(id).is_some() {
            return Ok(());
        }
        let gate = clock_gate(id).ok_or_else(|| ClockError::unsupported(id))?;
        self.setreg(clkgate_con(gate.register), gate.mask);
        Ok(())
    }

    pub fn clk_is_enabled(&self, id: ClkId) -> ClockResult<bool> {
        let id = sdmmc_phase_parent(id).unwrap_or(id);
        let gate = clock_gate(id).ok_or_else(|| ClockError::unsupported(id))?;
        Ok(self.read(clkgate_con(gate.register)) & gate.mask == 0)
    }

    pub fn clk_get_rate(&self, id: ClkId) -> ClockResult<u64> {
        let id = sdmmc_phase_parent(id).unwrap_or(id);
        match id {
            CCLK_EMMC => self.cclk_emmc_rate(),
            BCLK_EMMC => Ok(200 * MHZ),
            ACLK_EMMC | HCLK_EMMC => Ok(200 * MHZ),
            TCLK_EMMC => Ok(OSC_HZ),
            _ => {
                let clock = sdmmc_clock(id).ok_or_else(|| ClockError::unsupported(id))?;
                self.sdmmc_rate(id, clock)
            }
        }
    }

    pub fn clk_set_rate(&mut self, id: ClkId, rate_hz: u64) -> ClockResult<u64> {
        let id = sdmmc_phase_parent(id).unwrap_or(id);
        if id == CCLK_EMMC {
            let Some((selector, actual_hz)) = rate_selector(rate_hz) else {
                return Err(ClockError::invalid_rate(id, rate_hz));
            };
            self.write_clksel(CCLK_EMMC_SEL_MASK, selector);
            return Ok(actual_hz);
        }

        if let Some(clock) = sdmmc_clock(id) {
            let Some((selector, actual_hz)) = sdmmc_rate_selector(rate_hz) else {
                return Err(ClockError::invalid_rate(id, rate_hz));
            };
            let mask = SDMMC_SEL_MASK << clock.selector_shift;
            self.update_bits(
                clksel_con(clock.selector_register),
                mask,
                selector << clock.selector_shift,
            );
            return Ok(actual_hz);
        }

        let Some(actual_hz) = fixed_emmc_rate(id) else {
            return Err(ClockError::unsupported(id));
        };
        if rate_hz != actual_hz {
            return Err(ClockError::invalid_rate(id, rate_hz));
        }
        if id == BCLK_EMMC {
            self.write_clksel(BCLK_EMMC_SEL_MASK, BCLK_EMMC_GPLL_200M);
        }
        Ok(actual_hz)
    }

    fn enable_emmc_gates(&mut self) {
        self.clrreg(
            clkgate_con(CLKGATE_CON09),
            GATE_ACLK_EMMC | GATE_HCLK_EMMC | GATE_BCLK_EMMC | GATE_CCLK_EMMC | GATE_TCLK_EMMC,
        );
    }

    fn deassert_emmc_resets(&mut self) {
        for reset in EMMC_RESETS {
            self.reset_deassert(reset);
        }
    }

    fn configure_emmc_delay(&mut self) {
        self.clrsetreg(
            emmc_con(0),
            EMMC_CON0_DRV_ENABLE | EMMC_CON0_DRV_DELAYNUM_MASK | EMMC_CON0_DRV_DEGREE_MASK,
            EMMC_CON0_DRV_ENABLE | (EMMC_DELAYNUM << EMMC_CON0_DRV_DELAYNUM_SHIFT),
        );
        self.clrsetreg(
            emmc_con(1),
            EMMC_CON1_SAMPLE_ENABLE | EMMC_CON1_SAMPLE_DELAYNUM_MASK | EMMC_CON1_SAMPLE_DEGREE_MASK,
            EMMC_CON1_SAMPLE_ENABLE | (EMMC_DELAYNUM << EMMC_CON1_SAMPLE_DELAYNUM_SHIFT),
        );
    }

    fn cclk_emmc_rate(&self) -> ClockResult<u64> {
        let selector = self.read(clksel_con(CLKSEL_CON28)) & CCLK_EMMC_SEL_MASK;
        match selector {
            CCLK_EMMC_XIN_SOC0 => Ok(OSC_HZ),
            CCLK_EMMC_GPLL_200M => Ok(200 * MHZ),
            CCLK_EMMC_GPLL_150M => Ok(150 * MHZ),
            CCLK_EMMC_CPLL_100M => Ok(100 * MHZ),
            CCLK_EMMC_CPLL_50M => Ok(50 * MHZ),
            CCLK_EMMC_XIN_375K => Ok(375_000),
            _ => Err(ClockError::rate_read_failed(
                CCLK_EMMC,
                "invalid RK3568 eMMC selector",
            )),
        }
    }

    fn sdmmc_rate(&self, id: ClkId, clock: SdMmcClock) -> ClockResult<u64> {
        let selector = (self.read(clksel_con(clock.selector_register)) >> clock.selector_shift)
            & SDMMC_SEL_MASK;
        match selector {
            SDMMC_XIN24M => Ok(OSC_HZ),
            SDMMC_GPLL_400M => Ok(400 * MHZ),
            SDMMC_GPLL_300M => Ok(300 * MHZ),
            SDMMC_CPLL_100M => Ok(100 * MHZ),
            SDMMC_CPLL_50M => Ok(50 * MHZ),
            SDMMC_OSC0_DIV_750K => Ok(750_000),
            _ => Err(ClockError::rate_read_failed(
                id,
                "invalid RK3568 SDMMC selector",
            )),
        }
    }

    fn write_clksel(&mut self, mask: u32, value: u32) {
        self.update_bits(clksel_con(CLKSEL_CON28), mask, value);
    }

    fn reg(&self, offset: u32) -> *mut u32 {
        (self.base + offset as usize) as *mut u32
    }

    fn read(&self, offset: u32) -> u32 {
        unsafe { core::ptr::read_volatile(self.reg(offset)) }
    }

    fn write(&mut self, offset: u32, value: u32) {
        unsafe { core::ptr::write_volatile(self.reg(offset), value) }
    }

    fn update_bits(&mut self, offset: u32, mask: u32, value: u32) {
        let current = self.read(offset);
        self.write(offset, (current & !mask) | (mask << 16) | value);
    }

    fn clrsetreg(&mut self, offset: u32, clr: u32, set: u32) {
        self.update_bits(offset, clr, set);
    }

    fn clrreg(&mut self, offset: u32, clr: u32) {
        self.update_bits(offset, clr, 0);
    }

    fn setreg(&mut self, offset: u32, set: u32) {
        self.update_bits(offset, set, set);
    }
}

impl ResetOp for Cru {
    fn reset_assert(&mut self, id: RstId) {
        self.reset.reset_assert(id);
    }

    fn reset_deassert(&mut self, id: RstId) {
        self.reset.reset_deassert(id);
    }
}

impl ClockOp for Cru {
    fn clk_enable(&mut self, id: ClkId) -> ClockResult<()> {
        self.clk_enable(id)
    }

    fn clk_disable(&mut self, id: ClkId) -> ClockResult<()> {
        self.clk_disable(id)
    }

    fn clk_is_enabled(&self, id: ClkId) -> ClockResult<bool> {
        self.clk_is_enabled(id)
    }

    fn clk_get_rate(&self, id: ClkId) -> ClockResult<u64> {
        self.clk_get_rate(id)
    }

    fn clk_set_rate(&mut self, id: ClkId, rate_hz: u64) -> ClockResult<u64> {
        self.clk_set_rate(id, rate_hz)
    }
}

const fn clock_gate(id: ClkId) -> Option<ClockGate> {
    match id {
        CCLK_EMMC => Some(ClockGate {
            register: CLKGATE_CON09,
            mask: GATE_CCLK_EMMC,
        }),
        BCLK_EMMC => Some(ClockGate {
            register: CLKGATE_CON09,
            mask: GATE_BCLK_EMMC,
        }),
        ACLK_EMMC => Some(ClockGate {
            register: CLKGATE_CON09,
            mask: GATE_ACLK_EMMC,
        }),
        HCLK_EMMC => Some(ClockGate {
            register: CLKGATE_CON09,
            mask: GATE_HCLK_EMMC,
        }),
        TCLK_EMMC => Some(ClockGate {
            register: CLKGATE_CON09,
            mask: GATE_TCLK_EMMC,
        }),
        HCLK_SDMMC0 => Some(ClockGate {
            register: CLKGATE_CON15,
            mask: bit(0),
        }),
        CLK_SDMMC0 => Some(ClockGate {
            register: CLKGATE_CON15,
            mask: bit(1),
        }),
        HCLK_SDMMC1 => Some(ClockGate {
            register: CLKGATE_CON15,
            mask: bit(2),
        }),
        CLK_SDMMC1 => Some(ClockGate {
            register: CLKGATE_CON15,
            mask: bit(3),
        }),
        HCLK_SDMMC2 => Some(ClockGate {
            register: CLKGATE_CON17,
            mask: bit(0),
        }),
        CLK_SDMMC2 => Some(ClockGate {
            register: CLKGATE_CON17,
            mask: bit(1),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rk3568_cru_offsets_match_u_boot_layout() {
        assert_eq!(clksel_con(28), 0x0170);
        assert_eq!(clkgate_con(9), 0x0324);
        assert_eq!(SOFTRST_CON_OFFSET, 0x0400);
        assert_eq!(emmc_con(0), 0x0598);
        assert_eq!(emmc_con(1), 0x059c);
    }

    #[test]
    fn emmc_delay_programs_drive_and_sample_delaynum() {
        let con0_mask =
            EMMC_CON0_DRV_ENABLE | EMMC_CON0_DRV_DELAYNUM_MASK | EMMC_CON0_DRV_DEGREE_MASK;
        let con0_value = EMMC_CON0_DRV_ENABLE | (EMMC_DELAYNUM << EMMC_CON0_DRV_DELAYNUM_SHIFT);
        let con1_mask =
            EMMC_CON1_SAMPLE_ENABLE | EMMC_CON1_SAMPLE_DELAYNUM_MASK | EMMC_CON1_SAMPLE_DEGREE_MASK;
        let con1_value =
            EMMC_CON1_SAMPLE_ENABLE | (EMMC_DELAYNUM << EMMC_CON1_SAMPLE_DELAYNUM_SHIFT);

        assert_eq!((con0_mask << 16) | con0_value, 0x0ffe_0900);
        assert_eq!((con1_mask << 16) | con1_value, 0x07ff_0480);
    }

    #[test]
    fn emmc_reset_ids_match_rk3568_dts() {
        let ids = EMMC_RESETS.map(|reset| reset.value());

        assert_eq!(ids, [0x78, 0x76, 0x75, 0x77, 0x79]);
    }

    #[test]
    fn emmc_rate_selector_uses_supported_rk3568_steps() {
        assert_eq!(rate_selector(400_000), Some((CCLK_EMMC_XIN_375K, 375_000)));
        assert_eq!(
            rate_selector(25 * MHZ),
            Some((CCLK_EMMC_CPLL_50M, 50 * MHZ))
        );
        assert_eq!(
            rate_selector(50 * MHZ),
            Some((CCLK_EMMC_CPLL_50M, 50 * MHZ))
        );
        assert_eq!(
            rate_selector(104 * MHZ),
            Some((CCLK_EMMC_CPLL_100M, 100 * MHZ))
        );
        assert_eq!(
            rate_selector(200 * MHZ),
            Some((CCLK_EMMC_GPLL_200M, 200 * MHZ))
        );
        assert_eq!(rate_selector(374_999), None);
        assert_eq!(rate_selector(201 * MHZ), None);
    }

    #[test]
    fn emmc_assigned_fixed_rates_are_accepted() {
        let mut regs = [0_u32; 0x600 / core::mem::size_of::<u32>()];
        let base = regs.as_mut_ptr() as usize;
        let mut cru = Cru {
            base,
            reset: ResetRockchip::new(base + SOFTRST_CON_OFFSET as usize, 512),
        };

        assert_eq!(cru.clk_set_rate(BCLK_EMMC, 200 * MHZ).unwrap(), 200 * MHZ);
        assert_eq!(cru.clk_set_rate(TCLK_EMMC, OSC_HZ).unwrap(), OSC_HZ);
    }

    #[test]
    fn emmc_fixed_rates_reject_mismatched_assigned_rate() {
        let mut regs = [0_u32; 0x600 / core::mem::size_of::<u32>()];
        let base = regs.as_mut_ptr() as usize;
        let mut cru = Cru {
            base,
            reset: ResetRockchip::new(base + SOFTRST_CON_OFFSET as usize, 512),
        };

        let err = cru.clk_set_rate(BCLK_EMMC, 100 * MHZ).unwrap_err();
        assert!(matches!(
            err,
            ClockError::InvalidRate {
                clk_id: BCLK_EMMC,
                rate_hz,
            } if rate_hz == 100 * MHZ
        ));
    }

    #[test]
    fn sdmmc_card_clocks_accept_rates_used_by_rk3568_dwmmc() {
        let mut regs = [0_u32; 0x600 / core::mem::size_of::<u32>()];
        let base = regs.as_mut_ptr() as usize;
        let mut cru = Cru {
            base,
            reset: ResetRockchip::new(base + SOFTRST_CON_OFFSET as usize, 512),
        };

        let sdmmc0 = ClkId::new(0xb1);
        let sdmmc2 = ClkId::new(0xc2);

        assert_eq!(cru.clk_set_rate(sdmmc0, 50 * MHZ).unwrap(), 50 * MHZ);
        assert_eq!(cru.clk_get_rate(sdmmc0).unwrap(), 50 * MHZ);
        assert_eq!(cru.clk_set_rate(sdmmc2, 800_000).unwrap(), 750_000);
        assert_eq!(cru.clk_get_rate(sdmmc2).unwrap(), 750_000);
    }
}
