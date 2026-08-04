use crate::{Iomux, Mmio, PinId, PinctrlResult, Pull, pinctrl::PinctrlError};

const RK3576_MUX_BASES: [[usize; 4]; 5] = [
    [0x0000, 0x0008, 0x2004, 0x200c],
    [0x4020, 0x4028, 0x4030, 0x4038],
    [0x4040, 0x4048, 0x4050, 0x4058],
    [0x4060, 0x4068, 0x4070, 0x4078],
    [0x4080, 0x4088, 0xa390, 0xb398],
];
const GPIO0_B4_B7_MUX_OFFSET: usize = 0x1ff4;
const SYS_GRF_I3C_PULL_OFFSET: usize = 0x4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RegisterField {
    offset: usize,
    bit: u32,
}

pub(crate) struct PinctrlReg {
    ioc_base: Mmio,
    sys_grf_base: Option<Mmio>,
}

impl PinctrlReg {
    /// Creates an RK3576 IOC register accessor.
    ///
    /// # Safety
    ///
    /// `ioc_base` must be aligned for `u32` access and cover offsets through
    /// `0xb398 + 4`. When present, `sys_grf_base` must cover offset `0x4 + 4`.
    /// Both mappings must remain valid for this object, and callers must
    /// synchronize conflicting register accesses.
    pub(crate) unsafe fn new(ioc_base: Mmio, sys_grf_base: Option<Mmio>) -> Self {
        Self {
            ioc_base,
            sys_grf_base,
        }
    }

    pub(crate) fn set_mux(&self, pin: PinId, mux: Iomux) -> PinctrlResult<()> {
        self.enable_i3c_weak_pull(pin, mux)?;
        self.write_field(mux_field(pin), 0xf, u32::from(mux.bits()));
        Ok(())
    }

    pub(crate) fn get_mux(&self, pin: PinId) -> PinctrlResult<Iomux> {
        Iomux::from_bits(self.read_field(mux_field(pin), 0xf) as u8)
            .ok_or(PinctrlError::InvalidConfig)
    }

    pub(crate) fn set_pull(&self, pin: PinId, pull: Pull) -> PinctrlResult<()> {
        let value = pull_to_reg_value(pull).ok_or(PinctrlError::InvalidConfig)?;
        self.write_field(pull_field(pin), 0x3, value);
        Ok(())
    }

    pub(crate) fn get_pull(&self, pin: PinId) -> PinctrlResult<Pull> {
        reg_value_to_pull(self.read_field(pull_field(pin), 0x3)).ok_or(PinctrlError::InvalidConfig)
    }

    pub(crate) fn set_drive(&self, pin: PinId, drive: u32) -> PinctrlResult<()> {
        if drive > 0xf {
            return Err(PinctrlError::InvalidConfig);
        }
        self.write_field(drive_field(pin), 0xf, encode_drive_level(drive));
        Ok(())
    }

    pub(crate) fn get_drive(&self, pin: PinId) -> PinctrlResult<u32> {
        Ok(decode_drive_level(self.read_field(drive_field(pin), 0xf)))
    }

    fn enable_i3c_weak_pull(&self, pin: PinId, mux: Iomux) -> PinctrlResult<()> {
        let mux = u32::from(mux.bits());
        let bank = pin.bank().raw();
        let pin = pin.pin_in_bank();
        let value = match (bank, pin, mux) {
            (0, 21, 0xb) | (1, 25, 0xa) => Some(0x00c0_00c0),
            (2, 5, 0xe) | (2, 30, 0xc) | (3, 25, 0xb) => Some(0x0300_0300),
            _ => None,
        };
        let Some(value) = value else {
            return Ok(());
        };
        let sys_grf = self.sys_grf_base.ok_or(PinctrlError::Unsupported)?;
        // SAFETY: The constructor requires the SYS GRF mapping to cover this
        // aligned register, and Rockchip uses high-halfword write masks.
        unsafe {
            (sys_grf.as_ptr().add(SYS_GRF_I3C_PULL_OFFSET) as *mut u32).write_volatile(value);
        }
        Ok(())
    }

    fn write_field(&self, field: RegisterField, mask: u32, value: u32) {
        let word = masked_write_value(field, mask, value);
        // SAFETY: The constructor requires a live, aligned mapping covering
        // every RK3576 field offset returned by this module.
        unsafe {
            (self.ioc_base.as_ptr().add(field.offset) as *mut u32).write_volatile(word);
        }
    }

    fn read_field(&self, field: RegisterField, mask: u32) -> u32 {
        // SAFETY: The constructor requires a live, aligned mapping covering
        // every RK3576 field offset returned by this module.
        let word =
            unsafe { (self.ioc_base.as_ptr().add(field.offset) as *const u32).read_volatile() };
        (word >> field.bit) & mask
    }
}

fn pull_to_reg_value(pull: Pull) -> Option<u32> {
    match pull {
        Pull::Disabled => Some(0),
        Pull::PullDown => Some(1),
        Pull::PullUp => Some(3),
        Pull::BusHold | Pull::PullPinDefault => None,
    }
}

fn reg_value_to_pull(value: u32) -> Option<Pull> {
    match value {
        0 | 2 => Some(Pull::Disabled),
        1 => Some(Pull::PullDown),
        3 => Some(Pull::PullUp),
        _ => None,
    }
}

fn encode_drive_level(level: u32) -> u32 {
    ((level & 0x4) >> 2) | (level & 0x2) | ((level & 0x1) << 2)
}

fn decode_drive_level(value: u32) -> u32 {
    encode_drive_level(value)
}

fn masked_write_value(field: RegisterField, mask: u32, value: u32) -> u32 {
    let shifted_mask = mask << field.bit;
    (shifted_mask << 16) | ((value & mask) << field.bit)
}

fn mux_field(pin: PinId) -> RegisterField {
    let pin_in_bank = pin.pin_in_bank() as usize;
    let group = pin_in_bank / 8;
    let mut offset = RK3576_MUX_BASES[pin.bank().raw() as usize][group] + (pin_in_bank % 8 / 4) * 4;
    if pin.bank().raw() == 0 && (12..=15).contains(&pin_in_bank) {
        offset += GPIO0_B4_B7_MUX_OFFSET;
    }
    RegisterField {
        offset,
        bit: (pin_in_bank as u32 % 4) * 4,
    }
}

fn pull_field(pin: PinId) -> RegisterField {
    let bank = pin.bank().raw();
    let pin_in_bank = pin.pin_in_bank() as usize;
    let base = match (bank, pin_in_bank) {
        (0, 0..=11) => 0x0020,
        (0, _) => 0x2028 - 0x04,
        (1, _) => 0x6110,
        (2, _) => 0x6120,
        (3, _) => 0x6130,
        (4, 0..=15) => 0x6140,
        (4, 16..=23) => 0xa148 - 0x08,
        (4, _) => 0xb14c - 0x0c,
        _ => unreachable!("PinId only permits GPIO banks 0 through 4"),
    };
    RegisterField {
        offset: base + (pin_in_bank / 8) * 4,
        bit: (pin_in_bank as u32 % 8) * 2,
    }
}

fn drive_field(pin: PinId) -> RegisterField {
    let bank = pin.bank().raw();
    let pin_in_bank = pin.pin_in_bank() as usize;
    let base = match (bank, pin_in_bank) {
        (0, 0..=11) => 0x0010,
        (0, _) => 0x2014 - 0x0c,
        (1, _) => 0x6020,
        (2, _) => 0x6040,
        (3, _) => 0x6060,
        (4, 0..=15) => 0x6080,
        (4, 16..=23) => 0xa090 - 0x10,
        (4, _) => 0xb098 - 0x18,
        _ => unreachable!("PinId only permits GPIO banks 0 through 4"),
    };
    RegisterField {
        offset: base + (pin_in_bank / 4) * 4,
        bit: (pin_in_bank as u32 % 4) * 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(raw: u32) -> PinId {
        PinId::new(raw).unwrap()
    }

    #[test]
    fn rock_4d_sdmmc0_pins_use_rk3576_ioc_fields() {
        assert_eq!(
            mux_field(pin(64)),
            RegisterField {
                offset: 0x4040,
                bit: 0
            }
        );
        assert_eq!(
            mux_field(pin(69)),
            RegisterField {
                offset: 0x4044,
                bit: 4
            }
        );
        assert_eq!(
            mux_field(pin(7)),
            RegisterField {
                offset: 0x0004,
                bit: 12
            }
        );
        assert_eq!(
            mux_field(pin(14)),
            RegisterField {
                offset: 0x2000,
                bit: 8
            }
        );
    }

    #[test]
    fn rk3576_drive_level_uses_hardware_bit_order() {
        assert_eq!(encode_drive_level(0), 0);
        assert_eq!(encode_drive_level(1), 4);
        assert_eq!(encode_drive_level(3), 6);
        assert_eq!(encode_drive_level(4), 1);
        assert_eq!(encode_drive_level(7), 7);
        assert_eq!(decode_drive_level(encode_drive_level(3)), 3);
    }

    #[test]
    fn rk3576_ioc_access_uses_high_halfword_write_masks() {
        let mut memory = std::vec![0_u32; (0xb398 + 4) / 4];
        let ioc = core::ptr::NonNull::new(memory.as_mut_ptr().cast()).unwrap();
        let registers = unsafe { PinctrlReg::new(ioc, None) };

        registers
            .set_mux(pin(14), Iomux::from_bits_retain(1))
            .unwrap();
        assert_eq!(memory[0x2000 / 4], 0x0f00_0100);

        registers.set_pull(pin(64), Pull::PullUp).unwrap();
        assert_eq!(memory[0x6120 / 4], 0x0003_0003);

        registers.set_drive(pin(64), 3).unwrap();
        assert_eq!(memory[0x6040 / 4], 0x000f_0006);
    }

    #[test]
    fn rk3576_i3c_mux_requires_and_updates_sys_grf() {
        let mut ioc_memory = std::vec![0_u32; (0xb398 + 4) / 4];
        let mut sys_grf_memory = std::vec![0_u32; 0x2000 / 4];
        let ioc = core::ptr::NonNull::new(ioc_memory.as_mut_ptr().cast()).unwrap();
        let sys_grf = core::ptr::NonNull::new(sys_grf_memory.as_mut_ptr().cast()).unwrap();
        let without_sys_grf = unsafe { PinctrlReg::new(ioc, None) };
        let i3c0_scl = pin(21);
        let i3c_mux = Iomux::from_bits_retain(0xb);

        assert!(matches!(
            without_sys_grf.set_mux(i3c0_scl, i3c_mux),
            Err(PinctrlError::Unsupported)
        ));

        let registers = unsafe { PinctrlReg::new(ioc, Some(sys_grf)) };
        registers.set_mux(i3c0_scl, i3c_mux).unwrap();
        assert_eq!(sys_grf_memory[SYS_GRF_I3C_PULL_OFFSET / 4], 0x00c0_00c0);
    }
}
