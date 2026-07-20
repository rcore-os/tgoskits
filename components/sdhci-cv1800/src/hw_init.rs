//! Bounded CV1800/SG2002 SDIO1 clock, reset, and pinmux preparation.

use core::num::NonZeroU32;

use mmio_api::Mmio;
use sdhci_host::{HostResetHook, ResetHookPoll, ResetHookRecoveryMode, Sdhci};
use sdmmc_protocol::Error;

const DEFAULT_CLOCK_SETTLE_NS: u64 = 1_000_000;
const DEFAULT_BASE_CLOCK_HZ: u32 = 375_000_000;

const CLK_EN_0: usize = 0x000;
const CLK_EN_0_SD1_ALL: u32 = (1 << 21) | (1 << 22) | (1 << 23);
const CLK_BYP_0: usize = 0x030;
const CLK_BYP_0_SD1: u32 = 1 << 7;
const DIV_CLK_SD1: usize = 0x07c;
const DIV_CLK_100K_SD1: usize = 0x084;
const DIV_RESET_DEASSERT: u32 = 1;
const SD_CTRL_OPT: usize = 0x294;
const SD1_CARDDET_OW: u32 = 1 << 8;
const SD1_CARDDET_SW: u32 = 1 << 9;
const RTCSYS_RST_CTRL: usize = 0x018;
const RTCSYS_RST_SDIO: u32 = 1 << 2;
const RTCSYS_CLKMUX: usize = 0x01c;
const RTCSYS_CLKMUX_MASK: u32 = 0xf;
const RTCSYS_CLKBYP: usize = 0x030;
const RTCSYS_CLKBYP_SDIO: u32 = 1 << 1;
const RTCSYS_CLK_EN: usize = 0x034;
const RTCSYS_CLK_EN_SD1_ALL: u32 = (1 << 1) | (1 << 2);
const FMUX_SD1_VO: usize = 0x0e4;
const FMUX_SEL_SD1: u32 = 0;
const FMUX_WINDOW: usize = 0x1000;

/// Immutable CV1800 SDIO1 board policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Sdio1Policy {
    base_clock_hz: NonZeroU32,
    clock_settle_ns: u64,
}

impl Sdio1Policy {
    /// Creates a validated board policy.
    pub const fn new(base_clock_hz: NonZeroU32, clock_settle_ns: u64) -> Result<Self, Error> {
        if clock_settle_ns == 0 {
            return Err(Error::InvalidArgument);
        }
        Ok(Self {
            base_clock_hz,
            clock_settle_ns,
        })
    }

    pub(crate) const fn base_clock_hz(self) -> NonZeroU32 {
        self.base_clock_hz
    }

    pub(crate) const fn clock_settle_ns(self) -> u64 {
        self.clock_settle_ns
    }
}

impl Default for Sdio1Policy {
    fn default() -> Self {
        Self {
            base_clock_hz: NonZeroU32::new(DEFAULT_BASE_CLOCK_HZ)
                .expect("the CV1800 SDIO1 clock is nonzero"),
            clock_settle_ns: DEFAULT_CLOCK_SETTLE_NS,
        }
    }
}

/// Move-only ownership of every mapping used by the SDIO1 controller.
///
/// The mappings are consumed by discovery and remain alive through IRQ-source
/// retirement. There is deliberately no physical-address-plus-offset or raw
/// virtual-address constructor in the portable driver boundary.
///
/// ```compile_fail
/// use sdhci_cv1800::{CviSdhci, hw_init::{Sdio1MappedResources, Sdio1Policy}};
///
/// fn duplicate(resources: Sdio1MappedResources) {
///     let _first = CviSdhci::discover(resources, Sdio1Policy::default());
///     let _second = CviSdhci::discover(resources, Sdio1Policy::default());
/// }
/// ```
pub struct Sdio1MappedResources {
    crg: Mmio,
    sysctrl: Mmio,
    rtcsys_ctrl: Mmio,
    rtcsys_io: Mmio,
    sdio1: Mmio,
}

impl Sdio1MappedResources {
    /// Validates and groups already-owned mappings without touching hardware.
    pub fn new(
        crg: Mmio,
        sysctrl: Mmio,
        rtcsys_ctrl: Mmio,
        rtcsys_io: Mmio,
        sdio1: Mmio,
    ) -> Result<Self, Error> {
        let resources = Self {
            crg,
            sysctrl,
            rtcsys_ctrl,
            rtcsys_io,
            sdio1,
        };
        if resources.crg.size() < DIV_CLK_100K_SD1 + size_of::<u32>()
            || resources.sysctrl.size() < FMUX_WINDOW + FMUX_SD1_VO + size_of::<u32>()
            || resources.rtcsys_ctrl.size() < RTCSYS_CLK_EN + size_of::<u32>()
            || resources.rtcsys_io.size() < 0x88 + 20 * size_of::<u32>()
            || resources.sdio1.size() < 0x100
        {
            return Err(Error::InvalidArgument);
        }
        Ok(resources)
    }

    pub(crate) fn controller_base(&self) -> core::ptr::NonNull<u8> {
        self.sdio1.as_nonnull_ptr()
    }

    pub(crate) fn register_views(&self) -> Sdio1RegisterViews {
        Sdio1RegisterViews {
            crg: self.crg.as_ptr() as usize,
            sysctrl: self.sysctrl.as_ptr() as usize,
            rtcsys_ctrl: self.rtcsys_ctrl.as_ptr() as usize,
            rtcsys_io: self.rtcsys_io.as_ptr() as usize,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Sdio1RegisterViews {
    crg: usize,
    sysctrl: usize,
    rtcsys_ctrl: usize,
    rtcsys_io: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PrepareState {
    Idle,
    Waiting { wake_at_ns: u64 },
    Ready,
}

/// Absolute-time platform preparation used by generic SDHCI ResetAll.
pub(crate) struct Sdio1PlatformInit {
    registers: Sdio1RegisterViews,
    policy: Sdio1Policy,
    state: PrepareState,
}

impl Sdio1PlatformInit {
    pub(crate) const fn new(registers: Sdio1RegisterViews, policy: Sdio1Policy) -> Self {
        Self {
            registers,
            policy,
            state: PrepareState::Idle,
        }
    }

    fn program_soc(&self) {
        let fmux_base = self.registers.sysctrl + FMUX_WINDOW;
        for offset in [0xd0usize, 0xd4, 0xd8, 0xdc, 0xe0, 0xe4] {
            update32(fmux_base + offset, |value| value & !0x7);
        }
        for index in 0..20usize {
            write32(self.registers.rtcsys_io + 0x88 + index * 4, 0x1111_1111);
        }
        write32(self.registers.rtcsys_io + FMUX_SD1_VO, FMUX_SEL_SD1);
        set32(self.registers.crg + CLK_EN_0, CLK_EN_0_SD1_ALL);
        clear32(self.registers.crg + CLK_BYP_0, CLK_BYP_0_SD1);
        set32(self.registers.crg + DIV_CLK_SD1, DIV_RESET_DEASSERT);
        set32(self.registers.crg + DIV_CLK_100K_SD1, DIV_RESET_DEASSERT);
        clear32(
            self.registers.rtcsys_ctrl + RTCSYS_CLKMUX,
            RTCSYS_CLKMUX_MASK,
        );
        set32(
            self.registers.rtcsys_ctrl + RTCSYS_CLK_EN,
            RTCSYS_CLK_EN_SD1_ALL,
        );
        clear32(
            self.registers.rtcsys_ctrl + RTCSYS_CLKBYP,
            RTCSYS_CLKBYP_SDIO,
        );
        set32(
            self.registers.rtcsys_ctrl + RTCSYS_RST_CTRL,
            RTCSYS_RST_SDIO,
        );
        set32(
            self.registers.sysctrl + SD_CTRL_OPT,
            SD1_CARDDET_OW | SD1_CARDDET_SW,
        );
    }
}

impl HostResetHook for Sdio1PlatformInit {
    fn recovery_mode(&self) -> ResetHookRecoveryMode {
        ResetHookRecoveryMode::Scheduled
    }

    fn begin_before_reset_all(
        &mut self,
        _host: &mut Sdhci,
        now_ns: u64,
    ) -> Result<ResetHookPoll, Error> {
        match self.state {
            PrepareState::Idle => {
                self.program_soc();
                let wake_at_ns = now_ns
                    .checked_add(self.policy.clock_settle_ns())
                    .ok_or(Error::InvalidArgument)?;
                self.state = PrepareState::Waiting { wake_at_ns };
                Ok(ResetHookPoll::Pending { wake_at_ns })
            }
            PrepareState::Waiting { wake_at_ns } => Ok(ResetHookPoll::Pending { wake_at_ns }),
            PrepareState::Ready => Ok(ResetHookPoll::Ready),
        }
    }

    fn poll_before_reset_all(
        &mut self,
        _host: &mut Sdhci,
        now_ns: u64,
    ) -> Result<ResetHookPoll, Error> {
        match self.state {
            PrepareState::Waiting { wake_at_ns } if now_ns < wake_at_ns => {
                Ok(ResetHookPoll::Pending { wake_at_ns })
            }
            PrepareState::Waiting { .. } | PrepareState::Ready => {
                self.state = PrepareState::Ready;
                Ok(ResetHookPoll::Ready)
            }
            PrepareState::Idle => Err(Error::InvalidArgument),
        }
    }

    fn cancel_before_reset_all(&mut self, _host: &mut Sdhci) -> Result<(), Error> {
        self.state = PrepareState::Idle;
        Ok(())
    }

    fn after_reset(&self, _host: &mut Sdhci) -> Result<(), Error> {
        Ok(())
    }
}

fn update32(address: usize, update: impl FnOnce(u32) -> u32) {
    write32(address, update(read32(address)));
}

fn set32(address: usize, bits: u32) {
    update32(address, |value| value | bits);
}

fn clear32(address: usize, bits: u32) {
    update32(address, |value| value & !bits);
}

fn read32(address: usize) -> u32 {
    // SAFETY: `Sdio1MappedResources` remains owned by the controller while it
    // exclusively accesses each documented SoC register window.
    unsafe { core::ptr::read_volatile(address as *const u32) }
}

fn write32(address: usize, value: u32) {
    // SAFETY: same exclusive mapped-register ownership as `read32`.
    unsafe { core::ptr::write_volatile(address as *mut u32, value) }
}

#[cfg(test)]
mod tests {
    use core::ptr::NonNull;

    use super::*;

    #[test]
    fn platform_prepare_uses_an_absolute_settle_deadline() {
        let mut crg = [0u32; 0x1000 / 4];
        let mut sysctrl = [0u32; 0x2000 / 4];
        let mut rtc_ctrl = [0u32; 0x1000 / 4];
        let mut rtc_io = [0u32; 0x1000 / 4];
        let mut controller = [0u32; 0x1000 / 4];
        let registers = Sdio1RegisterViews {
            crg: crg.as_mut_ptr() as usize,
            sysctrl: sysctrl.as_mut_ptr() as usize,
            rtcsys_ctrl: rtc_ctrl.as_mut_ptr() as usize,
            rtcsys_io: rtc_io.as_mut_ptr() as usize,
        };
        let mut hook = Sdio1PlatformInit::new(registers, Sdio1Policy::default());
        // SAFETY: the local aligned register backing lives through the test.
        let mut host = unsafe {
            Sdhci::new(NonNull::new(controller.as_mut_ptr().cast()).expect("non-null backing"))
        };

        assert_eq!(
            hook.begin_before_reset_all(&mut host, 10).unwrap(),
            ResetHookPoll::Pending {
                wake_at_ns: 1_000_010
            }
        );
        assert_eq!(
            hook.poll_before_reset_all(&mut host, 999_999).unwrap(),
            ResetHookPoll::Pending {
                wake_at_ns: 1_000_010
            }
        );
        assert_eq!(
            hook.poll_before_reset_all(&mut host, 1_000_010).unwrap(),
            ResetHookPoll::Ready
        );
        assert_eq!(
            rtc_ctrl[RTCSYS_RST_CTRL / 4] & RTCSYS_RST_SDIO,
            RTCSYS_RST_SDIO
        );
        assert_eq!(
            sysctrl[SD_CTRL_OPT / 4] & (SD1_CARDDET_OW | SD1_CARDDET_SW),
            SD1_CARDDET_OW | SD1_CARDDET_SW
        );
    }
}
