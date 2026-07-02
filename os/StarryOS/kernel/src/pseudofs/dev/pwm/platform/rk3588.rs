use alloc::{
    string::{String, ToString},
    vec::Vec,
};
use core::ptr::NonNull;

use ax_memory_addr::PhysAddr;
use axfs_ng_vfs::{VfsError, VfsResult};
use rdif_pinctrl::PinctrlDevice;
use rdif_pwm::{Interface as PwmInterface, PwmError, PwmPolarity, PwmState};
use rdrive::probe::fdt::{Fdt, NodeType, Status};
use rockchip_pwm::{RK_PWM_CLOCK_HZ, RK_PWM_MMIO_SIZE, RockchipPwm};
use spin::LazyLock;

const PWM_CHANNELS_PER_CHIP: u8 = 1;
// Board FDT decides which hardware PWM controllers are exposed through the
// Linux-compatible sysfs numbering. This keeps board-specific pin choices out
// of the kernel backend.
const STARRY_PWMCHIPS_PROPERTY: &str = "starry,pwmchips";

static PWM_CHIPS: LazyLock<Vec<PwmChipDesc>> = LazyLock::new(discover_pwm_chips);

#[derive(Clone)]
struct PwmChipDesc {
    // Sysfs chip number, for example pwmchip4. This does not have to match the
    // discovered FDT order.
    sysfs_number: u8,
    path: String,
    base: usize,
    pwm_clock: u32,
    pclk: u32,
}

pub(in crate::pseudofs::dev::pwm) struct PwmHardware {
    desc: PwmChipDesc,
    controller: Option<RockchipPwm>,
}

pub(in crate::pseudofs::dev::pwm) fn pwm_chip_count() -> u8 {
    PWM_CHIPS.len() as u8
}

pub(in crate::pseudofs::dev::pwm) fn pwm_channels_per_chip(_index: u8) -> u8 {
    PWM_CHANNELS_PER_CHIP
}

pub(in crate::pseudofs::dev::pwm) fn pwmchip_number(index: u8) -> u8 {
    PWM_CHIPS[index as usize].sysfs_number
}

pub(in crate::pseudofs::dev::pwm) fn pwmchip_index(chip_number: u8) -> Option<u8> {
    PWM_CHIPS
        .iter()
        .position(|chip| chip.sysfs_number == chip_number)
        .map(|index| index as u8)
}

impl PwmHardware {
    pub(in crate::pseudofs::dev::pwm) fn new(index: u8) -> Self {
        Self {
            desc: PWM_CHIPS[index as usize].clone(),
            controller: None,
        }
    }

    fn ensure_initialized(&mut self) -> VfsResult<&mut RockchipPwm> {
        if self.controller.is_some() {
            return Ok(self.controller.as_mut().unwrap());
        }

        configure_pinctrl(&self.desc)?;
        configure_clocks(&self.desc)?;

        let base = NonNull::new(
            ax_mm::iomap(PhysAddr::from_usize(self.desc.base), RK_PWM_MMIO_SIZE)
                .map_err(|err| {
                    warn!("failed to map RK3588 PWM MMIO {:#x}: {err}", self.desc.base);
                    VfsError::NoMemory
                })?
                .as_mut_ptr(),
        )
        .ok_or(VfsError::NoMemory)?;
        // The MMIO mapping is kept for the lifetime of this sysfs PWM object.
        // Access serialization is provided by the global PWM sysfs mutex.
        let mut controller =
            unsafe { RockchipPwm::new(base, RK_PWM_CLOCK_HZ, PWM_CHANNELS_PER_CHIP as usize) };
        controller
            .init(PwmPolarity::Normal)
            .map_err(map_pwm_error)?;
        self.controller = Some(controller);
        Ok(self.controller.as_mut().unwrap())
    }
}

pub(in crate::pseudofs::dev::pwm) fn apply_channel(
    hw: &mut PwmHardware,
    channel_index: u8,
    period_ns: u64,
    duty_ns: u64,
    running: bool,
) -> VfsResult<()> {
    if channel_index >= PWM_CHANNELS_PER_CHIP {
        return Err(VfsError::InvalidInput);
    }
    let desc = hw.desc.clone();
    let controller = hw.ensure_initialized()?;
    configure_clocks(&desc)?;
    controller
        .apply(
            channel_index as usize,
            PwmState::normal(period_ns, duty_ns, running),
        )
        .map_err(map_pwm_error)
}

pub(in crate::pseudofs::dev::pwm) fn disable_channel(
    hw: &mut PwmHardware,
    channel_index: u8,
) -> VfsResult<()> {
    if channel_index >= PWM_CHANNELS_PER_CHIP {
        return Err(VfsError::InvalidInput);
    }
    let desc = hw.desc.clone();
    let controller = hw.ensure_initialized()?;
    configure_clocks(&desc)?;
    controller
        .disable(channel_index as usize)
        .map_err(map_pwm_error)
}

fn discover_pwm_chips() -> Vec<PwmChipDesc> {
    rdrive::with_fdt(|fdt| {
        let Some(chosen) = fdt.get_by_path("/chosen") else {
            warn!("RK3588 PWM disabled: /chosen node not found");
            return Vec::new();
        };
        let Some(prop) = chosen.as_node().get_property(STARRY_PWMCHIPS_PROPERTY) else {
            warn!("RK3588 PWM disabled: /chosen/{STARRY_PWMCHIPS_PROPERTY} not configured");
            return Vec::new();
        };

        let mut chips = Vec::new();
        for entry in prop.as_str_iter() {
            let Some((sysfs_number, path)) = parse_pwmchip_entry(entry) else {
                warn!("invalid RK3588 PWM chip entry '{entry}'");
                continue;
            };
            let Some(desc) = pwm_desc_from_fdt(fdt, sysfs_number, path) else {
                warn!("RK3588 PWM chip entry '{entry}' cannot be resolved from FDT");
                continue;
            };
            chips.push(desc);
        }
        chips
    })
    .unwrap_or_else(|| {
        warn!("RK3588 PWM disabled: live FDT not found");
        Vec::new()
    })
}

fn parse_pwmchip_entry(entry: &str) -> Option<(u8, &str)> {
    let (number, path) = entry.split_once(':')?;
    let sysfs_number = number.parse().ok()?;
    path.starts_with('/').then_some((sysfs_number, path))
}

fn pwm_desc_from_fdt(fdt: &Fdt, sysfs_number: u8, path: &str) -> Option<PwmChipDesc> {
    let node = fdt.get_by_path(path)?;
    if node.as_node().status() == Some(Status::Disabled) {
        warn!("RK3588 PWM {path} skipped: FDT node is disabled");
        return None;
    }
    if !node.as_node().compatibles().any(|compatible| {
        compatible == "rockchip,rk3588-pwm" || compatible == "rockchip,rk3328-pwm"
    }) {
        warn!("RK3588 PWM {path} skipped: incompatible FDT node");
        return None;
    }

    let reg = node.regs().into_iter().next()?;
    let (pwm_clock, pclk) = pwm_clock_ids(&node)?;
    Some(PwmChipDesc {
        sysfs_number,
        path: path.to_string(),
        base: reg.address as usize,
        pwm_clock,
        pclk,
    })
}

fn pwm_clock_ids(node: &NodeType<'_>) -> Option<(u32, u32)> {
    let mut pwm_clock = None;
    let mut pclk = None;
    for clock in node.clocks() {
        match clock.name.as_deref() {
            Some("pwm") => pwm_clock = clock.select(),
            Some("pclk") => pclk = clock.select(),
            _ => {}
        }
    }
    Some((pwm_clock?, pclk?))
}

fn configure_clocks(desc: &PwmChipDesc) -> VfsResult<()> {
    ax_driver::soc::rk3588_set_clock_rate(desc.pwm_clock, RK_PWM_CLOCK_HZ).map_err(|err| {
        warn!(
            "failed to set RK3588 PWM clock {} to {} Hz: {err}",
            desc.pwm_clock, RK_PWM_CLOCK_HZ
        );
        VfsError::InvalidInput
    })?;
    ax_driver::soc::rk3588_enable_clock(desc.pwm_clock).map_err(|err| {
        warn!(
            "failed to enable RK3588 PWM clock {}: {err}",
            desc.pwm_clock
        );
        VfsError::InvalidInput
    })?;
    ax_driver::soc::rk3588_enable_clock(desc.pclk).map_err(|err| {
        warn!("failed to enable RK3588 PWM pclk {}: {err}", desc.pclk);
        VfsError::InvalidInput
    })?;
    Ok(())
}

fn configure_pinctrl(desc: &PwmChipDesc) -> VfsResult<()> {
    // rdrive applies default pinctrl before normal FDT device probe, but this
    // sysfs backend is created lazily and is not itself an rdrive-probed device.
    // Apply the PWM node's default state here before touching the controller.
    apply_default_pinctrl(desc)
}

fn apply_default_pinctrl(desc: &PwmChipDesc) -> VfsResult<()> {
    let Some(pinctrl) = rdrive::get_one::<PinctrlDevice>() else {
        warn!("RK3588 PWM default pinctrl failed: no pinctrl device");
        return Err(VfsError::InvalidInput);
    };
    let Ok(mut pinctrl) = pinctrl.lock() else {
        warn!("RK3588 PWM default pinctrl failed: pinctrl device is busy");
        return Err(VfsError::InvalidInput);
    };
    let Some(result) = rdrive::with_fdt(|fdt| {
        let node = fdt.get_by_path(&desc.path).ok_or_else(|| {
            warn!("RK3588 PWM node {} not found", desc.path);
            VfsError::NotFound
        })?;
        pinctrl
            .apply_fdt_default_state(fdt, node.as_node())
            .map_err(|err| {
                warn!(
                    "failed to apply RK3588 PWM default pinctrl {}: {err}",
                    desc.path
                );
                VfsError::InvalidInput
            })
    }) else {
        warn!("RK3588 PWM default pinctrl failed: live FDT not found");
        return Err(VfsError::InvalidInput);
    };
    result.map(|_| ())
}

fn map_pwm_error(err: PwmError) -> VfsError {
    match err {
        PwmError::InvalidChannel
        | PwmError::InvalidPeriod
        | PwmError::InvalidDuty
        | PwmError::UnsupportedPolarity => VfsError::InvalidInput,
    }
}
