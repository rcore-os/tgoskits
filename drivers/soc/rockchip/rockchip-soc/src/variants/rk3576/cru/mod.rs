use crate::{
    ClockError, ClockResult, Mmio, ResetRockchip, RstId,
    clock::{ClkId, ClockOp, ResetOp},
};

const OSC_HZ: u64 = 24_000_000;
const CPLL_HZ: u64 = 1_000_000_000;
const GPLL_HZ: u64 = 1_188_000_000;

const CLKSEL_CON_OFFSET: u32 = 0x0300;
const CLKGATE_CON_OFFSET: u32 = 0x0800;
const SOFTRST_CON_OFFSET: u32 = 0x0a00;

const CCLK_SRC_SDMMC0: ClkId = ClkId::new(303);
const HCLK_SDMMC0: ClkId = ClkId::new(304);

const SDMMC_CLKSEL_INDEX: u32 = 105;
const SDMMC_MUX_SHIFT: u32 = 13;
const SDMMC_MUX_MASK: u32 = 0x3 << SDMMC_MUX_SHIFT;
const SDMMC_DIV_SHIFT: u32 = 7;
const SDMMC_DIV_MASK: u32 = 0x3f << SDMMC_DIV_SHIFT;
const SDMMC_GATE_INDEX: u32 = 43;
const CCLK_SDMMC_GATE: u32 = 1 << 1;
const HCLK_SDMMC_GATE: u32 = 1 << 2;

#[derive(Clone, Copy)]
struct ClockParent {
    selector: u32,
    rate_hz: u64,
}

const CLOCK_PARENTS: [ClockParent; 3] = [
    ClockParent {
        selector: 0,
        rate_hz: GPLL_HZ,
    },
    ClockParent {
        selector: 1,
        rate_hz: CPLL_HZ,
    },
    ClockParent {
        selector: 2,
        rate_hz: OSC_HZ,
    },
];

#[derive(Clone, Copy)]
struct RateConfig {
    parent: ClockParent,
    divider: u32,
    rate_hz: u64,
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
            reset: ResetRockchip::new(base + SOFTRST_CON_OFFSET as usize, 524_408),
        }
    }

    fn gate_mask(id: ClkId) -> Option<u32> {
        match id {
            CCLK_SRC_SDMMC0 => Some(CCLK_SDMMC_GATE),
            HCLK_SDMMC0 => Some(HCLK_SDMMC_GATE),
            _ => None,
        }
    }

    fn clk_enable(&mut self, id: ClkId) -> ClockResult<()> {
        let mask = Self::gate_mask(id).ok_or_else(|| ClockError::unsupported(id))?;
        self.write_mask(clkgate_con(SDMMC_GATE_INDEX), mask, 0);
        Ok(())
    }

    fn clk_disable(&mut self, id: ClkId) -> ClockResult<()> {
        let mask = Self::gate_mask(id).ok_or_else(|| ClockError::unsupported(id))?;
        self.write_mask(clkgate_con(SDMMC_GATE_INDEX), mask, mask);
        Ok(())
    }

    fn clk_is_enabled(&self, id: ClkId) -> ClockResult<bool> {
        let mask = Self::gate_mask(id).ok_or_else(|| ClockError::unsupported(id))?;
        Ok(self.read(clkgate_con(SDMMC_GATE_INDEX)) & mask == 0)
    }

    fn clk_get_rate(&self, id: ClkId) -> ClockResult<u64> {
        if id != CCLK_SRC_SDMMC0 {
            return Err(ClockError::unsupported(id));
        }
        let value = self.read(clksel_con(SDMMC_CLKSEL_INDEX));
        let selector = (value & SDMMC_MUX_MASK) >> SDMMC_MUX_SHIFT;
        let divider = ((value & SDMMC_DIV_MASK) >> SDMMC_DIV_SHIFT) + 1;
        let parent = CLOCK_PARENTS
            .into_iter()
            .find(|parent| parent.selector == selector)
            .ok_or_else(|| ClockError::invalid_clock_source(id, selector))?;
        Ok(parent.rate_hz / u64::from(divider))
    }

    fn clk_set_rate(&mut self, id: ClkId, rate_hz: u64) -> ClockResult<u64> {
        if id != CCLK_SRC_SDMMC0 {
            return Err(ClockError::unsupported(id));
        }
        let config = select_rate(rate_hz).ok_or_else(|| ClockError::invalid_rate(id, rate_hz))?;
        let value =
            (config.parent.selector << SDMMC_MUX_SHIFT) | ((config.divider - 1) << SDMMC_DIV_SHIFT);
        self.write_mask(
            clksel_con(SDMMC_CLKSEL_INDEX),
            SDMMC_MUX_MASK | SDMMC_DIV_MASK,
            value,
        );
        Ok(config.rate_hz)
    }

    fn reg(&self, offset: u32) -> *mut u32 {
        (self.base + offset as usize) as *mut u32
    }

    fn read(&self, offset: u32) -> u32 {
        // SAFETY: `base` comes from the CRU MMIO mapping and every caller uses an
        // aligned register offset within that mapping.
        unsafe { core::ptr::read_volatile(self.reg(offset)) }
    }

    fn write_mask(&mut self, offset: u32, mask: u32, value: u32) {
        // SAFETY: `base` comes from the CRU MMIO mapping and every caller uses an
        // aligned register offset within that mapping. Exclusive `&mut self` access
        // serializes register updates through the provider lock.
        unsafe { core::ptr::write_volatile(self.reg(offset), (mask << 16) | value) }
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

fn select_rate(request_hz: u64) -> Option<RateConfig> {
    let mut best = None;
    for parent in CLOCK_PARENTS {
        for divider in 1..=64 {
            let rate_hz = parent.rate_hz / u64::from(divider);
            if rate_hz > request_hz {
                continue;
            }
            if best
                .as_ref()
                .is_none_or(|current: &RateConfig| rate_hz > current.rate_hz)
            {
                best = Some(RateConfig {
                    parent,
                    divider,
                    rate_hz,
                });
            }
        }
    }
    best
}

const fn clksel_con(index: u32) -> u32 {
    CLKSEL_CON_OFFSET + index * 4
}

const fn clkgate_con(index: u32) -> u32 {
    CLKGATE_CON_OFFSET + index * 4
}
