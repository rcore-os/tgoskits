use crate::{
    common::{SDIOWIFI_REGISTER_BLOCK, SDIOWIFI_V3_WAKEUP_VALUE},
    device::*,
    registers::INTERRUPTS_ENABLED,
};

impl AicDevice {
    pub(super) fn vendor_setup_operation(
        &self,
        index: u8,
        reinitialize: bool,
    ) -> Option<SdioRequestKind> {
        if self.chip.is_v3() {
            match index {
                0 => Some(write_byte(0, 0xf2, 0x7f)),
                1 => Some(write_byte(1, self.registers.byte_mode_enable, 1)),
                2 => Some(write_byte(
                    1,
                    self.registers
                        .wakeup
                        .expect("v3 chips define a wakeup register"),
                    SDIOWIFI_V3_WAKEUP_VALUE,
                )),
                3 if reinitialize => Some(write_byte(0, 0x04, INTERRUPTS_ENABLED)),
                _ => None,
            }
        } else {
            match index {
                0 => Some(write_byte(1, SDIOWIFI_REGISTER_BLOCK, 1)),
                1 => Some(write_byte(1, self.registers.byte_mode_enable, 1)),
                2 => Some(write_byte(
                    1,
                    self.registers.interrupt_enable,
                    INTERRUPTS_ENABLED,
                )),
                _ => None,
            }
        }
    }
}
