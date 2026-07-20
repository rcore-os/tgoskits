use core::ops::RangeBounds;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RstId(u64);

impl From<u64> for RstId {
    fn from(value: u64) -> Self {
        RstId(value)
    }
}

impl From<usize> for RstId {
    fn from(value: usize) -> Self {
        RstId(value as u64)
    }
}

impl From<u32> for RstId {
    fn from(value: u32) -> Self {
        RstId(value as u64)
    }
}

impl From<RstId> for u64 {
    fn from(clk_id: RstId) -> Self {
        clk_id.0
    }
}

impl core::fmt::Display for RstId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "RstId({:#x})", self.0)
    }
}

impl RstId {
    /// 获取时钟 ID 的数值表示
    pub const fn value(&self) -> u64 {
        self.0
    }

    pub const fn new(value: u64) -> Self {
        RstId(value)
    }
}

impl RangeBounds<RstId> for RstId {
    fn start_bound(&self) -> core::ops::Bound<&RstId> {
        core::ops::Bound::Included(self)
    }

    fn end_bound(&self) -> core::ops::Bound<&RstId> {
        core::ops::Bound::Included(self)
    }
}

#[derive(Clone)]
pub struct ResetRockchip {
    base: usize,
    reset_num: usize,
}

impl ResetRockchip {
    pub(crate) fn new(base: usize, reset_num: usize) -> Self {
        ResetRockchip { base, reset_num }
    }

    pub fn reset_is_asserted(&self, id: RstId) -> Option<bool> {
        if id.value() >= self.reset_num as u64 {
            return None;
        }
        let bank = id.value() / 16;
        let offset = id.value() % 16;
        let addr = self.base + bank as usize * 4;
        // SAFETY: `base` is the mapped CRU soft-reset register aperture and
        // the checked reset id keeps this read within that aperture.
        let value = unsafe { core::ptr::read_volatile(addr as *const u32) };
        Some(value & (1 << offset) != 0)
    }

    pub fn reset_assert(&self, id: RstId) {
        let bank = id.value() / 16;
        let offset = id.value() % 16;
        let addr = self.base + (bank as usize * 4);
        debug!("reset (id={id}) (reg_addr={addr:#x})",);

        unsafe {
            let reg = addr as *mut u32;
            core::ptr::write_volatile(reg, 1 << offset | (1 << offset) << 16);
        }
    }

    pub fn reset_deassert(&self, id: RstId) {
        let bank = id.value() / 16;
        let offset = id.value() % 16;
        let addr = self.base + (bank as usize * 4);
        debug!("deassert reset (id={id}) (reg_addr={addr:#x})",);

        unsafe {
            let reg = addr as *mut u32;
            core::ptr::write_volatile(reg, (1 << offset) << 16);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_state_query_checks_the_selected_register_bit() {
        let mut registers = alloc::vec![0_u32; 2];
        let reset = ResetRockchip::new(registers.as_mut_ptr() as usize, 32);

        registers[0] = 1 << 3;
        assert_eq!(reset.reset_is_asserted(RstId::new(3)), Some(true));
        registers[0] = 0;
        assert_eq!(reset.reset_is_asserted(RstId::new(3)), Some(false));
        assert_eq!(reset.reset_is_asserted(RstId::new(32)), None);
    }
}
