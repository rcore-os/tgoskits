use core::ptr::NonNull;

use log::info;
use rdrive::{
    DriverGeneric, KError,
    probe::OnProbeError,
    register::{FdtInfo, ProbeFdt},
};
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields, register_structs,
    registers::{ReadOnly, ReadWrite},
};

use crate::mmio::iomap;

const JH7110_SYSCLK_SDIO0_AHB: usize = 91;
const JH7110_SYSCLK_SDIO1_AHB: usize = 92;
const JH7110_SYSCLK_SDIO0_SDCARD: usize = 93;
const JH7110_SYSCLK_SDIO1_SDCARD: usize = 94;
const JH7110_SYSCLK_END: usize = 190;

const JH7110_SYSRST_SDIO0_AHB: u64 = 64;
const JH7110_SYSRST_SDIO1_AHB: u64 = 65;
const JH7110_SYSRST_END: u64 = 126;
const JH7110_SYSRST_WORDS: usize = JH7110_SYSRST_END.div_ceil(u32::BITS as u64) as usize;

const SDIO_SDCARD_PARENT_HZ: u64 = 400_000_000;
const SDIO_SDCARD_MAX_DIV: u32 = 15;
const RESET_STATUS_POLL_LIMIT: usize = 1_000;

register_bitfields! [
    u32,
    ClockControl [
        DIV OFFSET(0) NUMBITS(24) [],
        ENABLE OFFSET(31) NUMBITS(1) []
    ]
];

register_structs! {
    Jh7110SysClockRegisters {
        (0x000 => controls: [ReadWrite<u32, ClockControl::Register>; JH7110_SYSCLK_END]),
        (0x2f8 => @END),
    }
}

register_structs! {
    Jh7110SysResetRegisters {
        (0x000 => _reserved0),
        (0x2f8 => assert: [ReadWrite<u32>; JH7110_SYSRST_WORDS]),
        (0x308 => status: [ReadOnly<u32>; JH7110_SYSRST_WORDS]),
        (0x318 => @END),
    }
}

crate::model_register!(
    name: "StarFive JH7110 Clock Generator",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::CLK,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["starfive,jh7110-clkgen"],
            on_probe: probe_clkgen
        }
    ],
);

crate::model_register!(
    name: "StarFive JH7110 Reset Controller",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::CLK,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["starfive,jh7110-reset"],
            on_probe: probe_reset
        }
    ],
);

struct Jh7110SysClock {
    base: NonNull<Jh7110SysClockRegisters>,
}

struct Jh7110SysReset {
    base: NonNull<Jh7110SysResetRegisters>,
}

unsafe impl Send for Jh7110SysClock {}
unsafe impl Send for Jh7110SysReset {}

impl Jh7110SysClock {
    fn new(base: NonNull<u8>) -> Self {
        Self { base: base.cast() }
    }

    fn regs(&self) -> &Jh7110SysClockRegisters {
        unsafe { self.base.as_ref() }
    }

    fn control(&self, id: usize) -> Result<&ReadWrite<u32, ClockControl::Register>, KError> {
        if supported_sdio_clock(id) {
            Ok(&self.regs().controls[id])
        } else {
            Err(KError::InvalidArg { name: "clock_id" })
        }
    }

    fn set_sdcard_rate(&mut self, id: usize, rate: u64) -> Result<u64, KError> {
        if !matches!(id, JH7110_SYSCLK_SDIO0_SDCARD | JH7110_SYSCLK_SDIO1_SDCARD) || rate == 0 {
            return Err(KError::InvalidArg { name: "clock_id" });
        }

        let div = divider_for_rate(rate);
        self.control(id)?.modify(ClockControl::DIV.val(div));
        Ok(SDIO_SDCARD_PARENT_HZ / u64::from(div))
    }
}

impl DriverGeneric for Jh7110SysClock {
    fn name(&self) -> &str {
        "jh7110-syscrg-clock"
    }
}

impl rdif_clk::Interface for Jh7110SysClock {
    fn perper_enable(&mut self) {}

    fn enable(&mut self, id: rdif_clk::ClockId) -> Result<(), KError> {
        let id = clock_id(id)?;
        self.control(id)?.modify(ClockControl::ENABLE::SET);
        Ok(())
    }

    fn is_enabled(&self, id: rdif_clk::ClockId) -> Result<bool, KError> {
        let id = clock_id(id)?;
        Ok(self.control(id)?.read(ClockControl::ENABLE) != 0)
    }

    fn get_rate(&self, id: rdif_clk::ClockId) -> Result<u64, KError> {
        let id = clock_id(id)?;
        if !matches!(id, JH7110_SYSCLK_SDIO0_SDCARD | JH7110_SYSCLK_SDIO1_SDCARD) {
            return Err(KError::InvalidArg { name: "clock_id" });
        }

        let div = self.control(id)?.read(ClockControl::DIV);
        if div == 0 {
            return Ok(0);
        }
        Ok(SDIO_SDCARD_PARENT_HZ / u64::from(div))
    }

    fn set_rate(&mut self, id: rdif_clk::ClockId, rate: u64) -> Result<(), KError> {
        self.set_sdcard_rate(clock_id(id)?, rate)?;
        Ok(())
    }
}

impl Jh7110SysReset {
    fn new(base: NonNull<u8>) -> Self {
        Self { base: base.cast() }
    }

    fn regs(&self) -> &Jh7110SysResetRegisters {
        unsafe { self.base.as_ref() }
    }

    fn reset_word_bit(id: rdif_reset::ResetId) -> Result<(usize, u32), rdif_reset::ResetError> {
        let raw = id.raw();
        if !matches!(raw, JH7110_SYSRST_SDIO0_AHB | JH7110_SYSRST_SDIO1_AHB) {
            return Err(rdif_reset::ResetError::InvalidId);
        }
        Ok((
            (raw / u64::from(u32::BITS)) as usize,
            1_u32 << (raw % u64::from(u32::BITS)),
        ))
    }

    fn update(
        &mut self,
        id: rdif_reset::ResetId,
        assert: bool,
    ) -> Result<(), rdif_reset::ResetError> {
        let (word, mask) = Self::reset_word_bit(id)?;
        let assert_reg = &self.regs().assert[word];
        let value = assert_reg.get();
        if assert {
            assert_reg.set(value | mask);
            self.poll_status(word, mask, false)
        } else {
            assert_reg.set(value & !mask);
            self.poll_status(word, mask, true)
        }
    }

    fn poll_status(
        &self,
        word: usize,
        mask: u32,
        deasserted: bool,
    ) -> Result<(), rdif_reset::ResetError> {
        for _ in 0..RESET_STATUS_POLL_LIMIT {
            let status = self.regs().status[word].get() & mask != 0;
            if status == deasserted {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(rdif_reset::ResetError::Controller)
    }
}

impl DriverGeneric for Jh7110SysReset {
    fn name(&self) -> &str {
        "jh7110-syscrg-reset"
    }
}

impl rdif_reset::Interface for Jh7110SysReset {
    fn assert(&mut self, id: rdif_reset::ResetId) -> Result<(), rdif_reset::ResetError> {
        self.update(id, true)
    }

    fn deassert(&mut self, id: rdif_reset::ResetId) -> Result<(), rdif_reset::ResetError> {
        self.update(id, false)
    }

    fn is_asserted(&self, id: rdif_reset::ResetId) -> Result<bool, rdif_reset::ResetError> {
        let (word, mask) = Self::reset_word_bit(id)?;
        Ok(self.regs().status[word].get() & mask == 0)
    }
}

fn probe_clkgen(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, plat_dev) = probe.into_parts();
    let base = map_first_reg(&info)?;
    plat_dev.register(rdif_clk::Clk::new(Jh7110SysClock::new(base)));
    info!("StarFive JH7110 SYS clock provider registered");
    Ok(())
}

fn probe_reset(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, plat_dev) = probe.into_parts();
    let base = map_first_reg(&info)?;
    plat_dev.register(rdif_reset::Reset::new(Jh7110SysReset::new(base)));
    info!("StarFive JH7110 SYS reset provider registered");
    Ok(())
}

fn map_first_reg(info: &FdtInfo<'_>) -> Result<NonNull<u8>, OnProbeError> {
    let reg =
        info.node.regs().into_iter().next().ok_or_else(|| {
            OnProbeError::other(alloc::format!("[{}] has no reg", info.node.name()))
        })?;
    iomap(reg.address as usize, reg.size.unwrap_or(0x10000) as usize)
}

fn supported_sdio_clock(id: usize) -> bool {
    matches!(
        id,
        JH7110_SYSCLK_SDIO0_AHB
            | JH7110_SYSCLK_SDIO1_AHB
            | JH7110_SYSCLK_SDIO0_SDCARD
            | JH7110_SYSCLK_SDIO1_SDCARD
    )
}

fn clock_id(id: rdif_clk::ClockId) -> Result<usize, KError> {
    let id = id.raw();
    if supported_sdio_clock(id) {
        Ok(id)
    } else {
        Err(KError::InvalidArg { name: "clock_id" })
    }
}

fn divider_for_rate(rate: u64) -> u32 {
    let rounded = (SDIO_SDCARD_PARENT_HZ + rate / 2) / rate;
    u32::try_from(rounded)
        .unwrap_or(SDIO_SDCARD_MAX_DIV)
        .clamp(1, SDIO_SDCARD_MAX_DIV)
}

#[cfg(test)]
mod tests {
    use rdif_clk::Interface as _;
    use rdif_reset::Interface as _;

    use super::*;

    fn fake_syscrg() -> (alloc::vec::Vec<u32>, NonNull<u8>) {
        let mut regs = alloc::vec![0_u32; 0x10000 / core::mem::size_of::<u32>()];
        let base = NonNull::new(regs.as_mut_ptr().cast::<u8>()).unwrap();
        (regs, base)
    }

    #[test]
    fn sdio_clock_enable_and_rate_use_jh71x0_control_fields() {
        let (regs, base) = fake_syscrg();
        let mut clock = Jh7110SysClock::new(base);

        clock
            .set_rate(
                rdif_clk::ClockId::from(JH7110_SYSCLK_SDIO1_SDCARD),
                50_000_000,
            )
            .unwrap();
        clock
            .enable(rdif_clk::ClockId::from(JH7110_SYSCLK_SDIO1_AHB))
            .unwrap();
        clock
            .enable(rdif_clk::ClockId::from(JH7110_SYSCLK_SDIO1_SDCARD))
            .unwrap();

        assert_eq!(regs[JH7110_SYSCLK_SDIO1_SDCARD] & ClockControl::DIV.mask, 8);
        assert_ne!(
            regs[JH7110_SYSCLK_SDIO1_AHB] & ClockControl::ENABLE::SET.value,
            0
        );
        assert_eq!(
            regs[JH7110_SYSCLK_SDIO1_SDCARD] & ClockControl::ENABLE::SET.value,
            ClockControl::ENABLE::SET.value
        );
        assert_eq!(
            clock
                .get_rate(rdif_clk::ClockId::from(JH7110_SYSCLK_SDIO1_SDCARD))
                .unwrap(),
            50_000_000
        );
    }

    #[test]
    fn sys_reset_deassert_clears_assert_bit_for_sdio1() {
        let (mut regs, base) = fake_syscrg();
        let word = (JH7110_SYSRST_SDIO1_AHB / u64::from(u32::BITS)) as usize;
        let bit = 1_u32 << (JH7110_SYSRST_SDIO1_AHB % u64::from(u32::BITS));
        let assert_index = 0x2f8 / core::mem::size_of::<u32>() + word;
        let status_index = 0x308 / core::mem::size_of::<u32>() + word;
        regs[assert_index] = bit;
        regs[status_index] = bit;

        let mut reset = Jh7110SysReset::new(base);
        reset
            .deassert(rdif_reset::ResetId::new(JH7110_SYSRST_SDIO1_AHB))
            .unwrap();

        assert_eq!(regs[assert_index] & bit, 0);
    }

    #[test]
    fn unsupported_clock_and_reset_ids_are_rejected() {
        let (_regs, base) = fake_syscrg();
        let mut clock = Jh7110SysClock::new(base);
        let mut reset = Jh7110SysReset::new(base);

        assert!(matches!(
            clock.enable(rdif_clk::ClockId::from(1_usize)),
            Err(KError::InvalidArg { name: "clock_id" })
        ));
        assert_eq!(
            reset.deassert(rdif_reset::ResetId::new(1)),
            Err(rdif_reset::ResetError::InvalidId)
        );
    }
}
