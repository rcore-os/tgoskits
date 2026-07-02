use alloc::vec::Vec;

use axfs_ng_vfs::{VfsError, VfsResult};
use rdif_pwm::{Pwm, PwmError, PwmState};
use rdrive::{
    Device, DeviceId,
    probe::fdt::{NodeType, Status},
};
use spin::LazyLock;

static PWM_CHIPS: LazyLock<Vec<PwmChipDesc>> = LazyLock::new(discover_pwm_chips);

#[derive(Clone)]
struct PwmChipDesc {
    sysfs_number: u8,
    channels: u8,
    device_id: DeviceId,
}

pub(in crate::pseudofs::dev::pwm) struct PwmHardware {
    device_id: DeviceId,
}

pub(in crate::pseudofs::dev::pwm) fn pwm_chip_count() -> u8 {
    PWM_CHIPS.len() as u8
}

pub(in crate::pseudofs::dev::pwm) fn pwm_channels_per_chip(index: u8) -> u8 {
    PWM_CHIPS[index as usize].channels
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
            device_id: PWM_CHIPS[index as usize].device_id,
        }
    }
}

pub(in crate::pseudofs::dev::pwm) fn apply_channel(
    hw: &mut PwmHardware,
    channel_index: u8,
    period_ns: u64,
    duty_ns: u64,
    running: bool,
) -> VfsResult<()> {
    let mut device = get_pwm_device(hw.device_id)?;
    device
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
    let mut device = get_pwm_device(hw.device_id)?;
    device
        .disable(channel_index as usize)
        .map_err(map_pwm_error)
}

fn discover_pwm_chips() -> Vec<PwmChipDesc> {
    rdrive::with_fdt(|fdt| {
        let mut chips = Vec::new();
        for node in fdt.all_nodes() {
            if !is_enabled_rockchip_pwm(node) {
                continue;
            }
            let path = node.path();
            // Match Linux sysfs numbering: every available PWM controller is
            // registered in FDT order, while consumers choose the chip they use.
            let Some(desc) = pwm_desc_from_rdrive(chips.len() as u8, &path) else {
                warn!("RK3588 PWM {path} skipped: rdrive device not found");
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

fn is_enabled_rockchip_pwm(node: NodeType<'_>) -> bool {
    if node.as_node().status() == Some(Status::Disabled) {
        return false;
    }
    node.as_node().compatibles().any(|compatible| {
        compatible == "rockchip,rk3588-pwm" || compatible == "rockchip,rk3328-pwm"
    })
}

fn pwm_desc_from_rdrive(sysfs_number: u8, path: &str) -> Option<PwmChipDesc> {
    let device_id = rdrive::fdt_path_to_device_id(path)?;
    let device = rdrive::get::<Pwm>(device_id).ok()?;
    let channels = lock_pwm_device(&device)
        .ok()
        .and_then(|device| u8::try_from(device.channel_count()).ok())?;
    if channels == 0 {
        warn!("RK3588 PWM {path} skipped: device has no channel");
        return None;
    }
    Some(PwmChipDesc {
        sysfs_number,
        channels,
        device_id,
    })
}

fn get_pwm_device(device_id: DeviceId) -> VfsResult<rdrive::DeviceGuard<Pwm>> {
    let device = rdrive::get::<Pwm>(device_id).map_err(|err| {
        warn!("failed to get RK3588 PWM device {device_id:?}: {err}");
        VfsError::InvalidInput
    })?;
    lock_pwm_device(&device)
}

fn lock_pwm_device(device: &Device<Pwm>) -> VfsResult<rdrive::DeviceGuard<Pwm>> {
    device.lock().map_err(|err| {
        warn!("failed to lock RK3588 PWM device: {err}");
        VfsError::InvalidInput
    })
}

fn map_pwm_error(err: PwmError) -> VfsError {
    match err {
        PwmError::InvalidChannel
        | PwmError::InvalidPeriod
        | PwmError::InvalidDuty
        | PwmError::UnsupportedPolarity => VfsError::InvalidInput,
    }
}
