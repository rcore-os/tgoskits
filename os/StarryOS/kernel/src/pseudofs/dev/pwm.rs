use alloc::{borrow::Cow, boxed::Box, format, sync::Arc, vec::Vec};

use ax_sync::Mutex;
use axfs_ng_vfs::{VfsError, VfsResult};
use sg200x_bsp::{
    pwm::{Pwm, PwmChannel, PwmMode, PwmPolarity},
    soc::PWM0_BASE,
};
use spin::LazyLock;

use crate::pseudofs::{
    DirMaker, NodeOpsMux, RwFile, SimpleDir, SimpleDirOps, SimpleFile, SimpleFileOperation,
    SimpleFs,
};

const PWM_SYSFS_CHIPS: u8 = 4;
const PWM_SYSFS_CHANNELS_PER_CHIP: u8 = 4;
const PWM_PERIOD_NS: u64 = 1_000_000_000;

/// Returns a [`DirMaker`] for `/sys/class/pwm`, to be embedded into the
/// kernel-wide sysfs tree by [`crate::pseudofs::sysfs`]. The pwm subsystem
/// shares the sysfs superblock so that `realpath()` on subordinate symlinks
/// keeps resolving inside `/sys`.
pub(crate) fn pwm_class_dir_maker(fs: Arc<SimpleFs>) -> DirMaker {
    SimpleDir::new_maker(fs.clone(), Arc::new(PwmClassDir { fs }))
}

#[derive(Clone, Copy, Default)]
struct PwmChannelState {
    exported: bool,
    enabled: bool,
    period_ns: u64,
    duty_ns: u64,
}

struct PwmChipState {
    pwm: Pwm,
    channels: [PwmChannelState; PWM_SYSFS_CHANNELS_PER_CHIP as usize],
}

struct PwmSysfsState {
    chips: Vec<PwmChipState>,
}

unsafe impl Send for PwmSysfsState {}
unsafe impl Sync for PwmSysfsState {}

impl PwmSysfsState {
    fn new() -> Self {
        let mut chips = Vec::with_capacity(PWM_SYSFS_CHIPS as usize);
        for index in 0..PWM_SYSFS_CHIPS {
            let pwm_addr = PWM0_BASE + index as usize * 0x1000 + ax_config::plat::PHYS_VIRT_OFFSET;
            let pwm = unsafe { Pwm::new(pwm_addr) };
            chips.push(PwmChipState {
                pwm,
                channels: [PwmChannelState::default(); PWM_SYSFS_CHANNELS_PER_CHIP as usize],
            });
        }
        Self { chips }
    }
}

static PWM_SYSFS_STATE: LazyLock<Mutex<PwmSysfsState>> =
    LazyLock::new(|| Mutex::new(PwmSysfsState::new()));

struct PwmClassDir {
    fs: Arc<SimpleFs>,
}

impl SimpleDirOps for PwmClassDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(
            (0..PWM_SYSFS_CHIPS)
                .map(|index| Cow::Owned(format!("pwmchip{}", index * PWM_SYSFS_CHANNELS_PER_CHIP))),
        )
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let chip_index = parse_pwmchip_index(name).ok_or(VfsError::NotFound)?;
        Ok(NodeOpsMux::Dir(SimpleDir::new_maker(
            self.fs.clone(),
            Arc::new(PwmChipDir {
                fs: self.fs.clone(),
                chip_index,
            }),
        )))
    }

    fn is_cacheable(&self) -> bool {
        false
    }
}

struct PwmChipDir {
    fs: Arc<SimpleFs>,
    chip_index: u8,
}

impl SimpleDirOps for PwmChipDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        let mut names = Vec::new();
        names.push(Cow::Borrowed("export"));
        names.push(Cow::Borrowed("unexport"));
        names.push(Cow::Borrowed("npwm"));
        let state = PWM_SYSFS_STATE.lock();
        if let Some(chip) = state.chips.get(self.chip_index as usize) {
            for (index, channel) in chip.channels.iter().enumerate() {
                if channel.exported {
                    names.push(Cow::Owned(format!("pwm{}", index)));
                }
            }
        }
        Box::new(names.into_iter())
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        match name {
            "export" => Ok(SimpleFile::new_regular(
                self.fs.clone(),
                RwFile::new({
                    let chip_index = self.chip_index;
                    move |req| match req {
                        SimpleFileOperation::Read => Ok(Some(Vec::new())),
                        SimpleFileOperation::Write(data) => {
                            if data.is_empty() || data.iter().all(|b| b.is_ascii_whitespace()) {
                                return Ok(None);
                            }
                            let channel = parse_u8(data)?;
                            export_pwm_channel(chip_index, channel)?;
                            Ok(None)
                        }
                    }
                }),
            )
            .into()),
            "unexport" => Ok(SimpleFile::new_regular(
                self.fs.clone(),
                RwFile::new({
                    let chip_index = self.chip_index;
                    move |req| match req {
                        SimpleFileOperation::Read => Ok(Some(Vec::new())),
                        SimpleFileOperation::Write(data) => {
                            if data.is_empty() || data.iter().all(|b| b.is_ascii_whitespace()) {
                                return Ok(None);
                            }
                            let channel = parse_u8(data)?;
                            unexport_pwm_channel(chip_index, channel)?;
                            Ok(None)
                        }
                    }
                }),
            )
            .into()),
            "npwm" => Ok(SimpleFile::new_regular(self.fs.clone(), || {
                Ok(format!("{}\n", PWM_SYSFS_CHANNELS_PER_CHIP))
            })
            .into()),
            _ => {
                let local_index = parse_pwm_local_index(name).ok_or(VfsError::NotFound)?;
                let state = PWM_SYSFS_STATE.lock();
                let exported = state
                    .chips
                    .get(self.chip_index as usize)
                    .and_then(|chip| chip.channels.get(local_index as usize))
                    .map(|ch| ch.exported)
                    .unwrap_or(false);
                if !exported {
                    return Err(VfsError::NotFound);
                }
                Ok(NodeOpsMux::Dir(SimpleDir::new_maker(
                    self.fs.clone(),
                    Arc::new(PwmChannelDir {
                        fs: self.fs.clone(),
                        chip_index: self.chip_index,
                        channel_index: local_index,
                    }),
                )))
            }
        }
    }

    fn is_cacheable(&self) -> bool {
        false
    }
}

struct PwmChannelDir {
    fs: Arc<SimpleFs>,
    chip_index: u8,
    channel_index: u8,
}

impl SimpleDirOps for PwmChannelDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(
            ["period", "duty_cycle", "enable"]
                .iter()
                .map(|s| Cow::Borrowed(*s)),
        )
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let chip_index = self.chip_index;
        let channel_index = self.channel_index;
        let file = match name {
            "period" => SimpleFile::new_regular(
                self.fs.clone(),
                RwFile::new(move |req| match req {
                    SimpleFileOperation::Read => Ok(Some(
                        format!("{}\n", pwm_read_period(chip_index, channel_index)?).into_bytes(),
                    )),
                    SimpleFileOperation::Write(data) => {
                        if data.is_empty() || data.iter().all(|b| b.is_ascii_whitespace()) {
                            return Ok(None);
                        }
                        pwm_write_period(chip_index, channel_index, data)?;
                        Ok(None)
                    }
                }),
            ),
            "duty_cycle" => SimpleFile::new_regular(
                self.fs.clone(),
                RwFile::new(move |req| match req {
                    SimpleFileOperation::Read => Ok(Some(
                        format!("{}\n", pwm_read_duty(chip_index, channel_index)?).into_bytes(),
                    )),
                    SimpleFileOperation::Write(data) => {
                        if data.is_empty() || data.iter().all(|b| b.is_ascii_whitespace()) {
                            return Ok(None);
                        }
                        pwm_write_duty(chip_index, channel_index, data)?;
                        Ok(None)
                    }
                }),
            ),
            "enable" => SimpleFile::new_regular(
                self.fs.clone(),
                RwFile::new(move |req| match req {
                    SimpleFileOperation::Read => Ok(Some(
                        format!("{}\n", pwm_read_enable(chip_index, channel_index)?).into_bytes(),
                    )),
                    SimpleFileOperation::Write(data) => {
                        if data.is_empty() || data.iter().all(|b| b.is_ascii_whitespace()) {
                            return Ok(None);
                        }
                        pwm_write_enable(chip_index, channel_index, data)?;
                        Ok(None)
                    }
                }),
            ),
            _ => return Err(VfsError::NotFound),
        };
        Ok(file.into())
    }

    fn is_cacheable(&self) -> bool {
        false
    }
}

fn parse_pwmchip_index(name: &str) -> Option<u8> {
    let value = name.strip_prefix("pwmchip")?.parse::<u8>().ok()?;
    if value % PWM_SYSFS_CHANNELS_PER_CHIP != 0 {
        return None;
    }
    let index = value / PWM_SYSFS_CHANNELS_PER_CHIP;
    if index < PWM_SYSFS_CHIPS {
        Some(index)
    } else {
        None
    }
}

fn parse_pwm_local_index(name: &str) -> Option<u8> {
    let value = name.strip_prefix("pwm")?.parse::<u8>().ok()?;
    if value < PWM_SYSFS_CHANNELS_PER_CHIP {
        Some(value)
    } else {
        None
    }
}

fn parse_u64(data: &[u8]) -> VfsResult<u64> {
    core::str::from_utf8(data)
        .ok()
        .and_then(|t| t.trim().parse::<u64>().ok())
        .ok_or(VfsError::InvalidInput)
}

fn parse_u8(data: &[u8]) -> VfsResult<u8> {
    core::str::from_utf8(data)
        .ok()
        .and_then(|t| t.trim().parse::<u8>().ok())
        .ok_or(VfsError::InvalidInput)
}

fn export_pwm_channel(chip_index: u8, channel: u8) -> VfsResult<()> {
    if channel >= PWM_SYSFS_CHANNELS_PER_CHIP {
        return Err(VfsError::InvalidInput);
    }
    let mut state = PWM_SYSFS_STATE.lock();
    let chip = state
        .chips
        .get_mut(chip_index as usize)
        .ok_or(VfsError::InvalidInput)?;
    let entry = &mut chip.channels[channel as usize];
    if !entry.exported {
        *entry = PwmChannelState {
            exported: true,
            ..Default::default()
        };
    }
    Ok(())
}

fn unexport_pwm_channel(chip_index: u8, channel: u8) -> VfsResult<()> {
    if channel >= PWM_SYSFS_CHANNELS_PER_CHIP {
        return Err(VfsError::InvalidInput);
    }
    let mut state = PWM_SYSFS_STATE.lock();
    let chip = state
        .chips
        .get_mut(chip_index as usize)
        .ok_or(VfsError::InvalidInput)?;
    let entry = &mut chip.channels[channel as usize];
    if entry.enabled {
        let ch = PwmChannel::from_u8(channel).ok_or(VfsError::InvalidInput)?;
        chip.pwm.stop(ch);
        chip.pwm.disable_output(ch);
    }
    *entry = PwmChannelState::default();
    Ok(())
}

fn pwm_read_period(chip_index: u8, channel_index: u8) -> VfsResult<u64> {
    let state = PWM_SYSFS_STATE.lock();
    Ok(state
        .chips
        .get(chip_index as usize)
        .and_then(|c| c.channels.get(channel_index as usize))
        .ok_or(VfsError::InvalidInput)?
        .period_ns)
}

fn pwm_read_duty(chip_index: u8, channel_index: u8) -> VfsResult<u64> {
    let state = PWM_SYSFS_STATE.lock();
    Ok(state
        .chips
        .get(chip_index as usize)
        .and_then(|c| c.channels.get(channel_index as usize))
        .ok_or(VfsError::InvalidInput)?
        .duty_ns)
}

fn pwm_read_enable(chip_index: u8, channel_index: u8) -> VfsResult<u8> {
    let state = PWM_SYSFS_STATE.lock();
    Ok(state
        .chips
        .get(chip_index as usize)
        .and_then(|c| c.channels.get(channel_index as usize))
        .ok_or(VfsError::InvalidInput)?
        .enabled as u8)
}

fn pwm_write_period(chip_index: u8, channel_index: u8, data: &[u8]) -> VfsResult<()> {
    let value = parse_u64(data)?;
    if value == 0 {
        return Err(VfsError::InvalidInput);
    }
    let mut state = PWM_SYSFS_STATE.lock();
    let chip = state
        .chips
        .get_mut(chip_index as usize)
        .ok_or(VfsError::InvalidInput)?;
    let enabled = {
        let e = chip
            .channels
            .get_mut(channel_index as usize)
            .ok_or(VfsError::InvalidInput)?;
        e.period_ns = value;
        e.enabled
    };
    pwm_apply_channel(chip, channel_index, enabled)
}

fn pwm_write_duty(chip_index: u8, channel_index: u8, data: &[u8]) -> VfsResult<()> {
    let value = parse_u64(data)?;
    let mut state = PWM_SYSFS_STATE.lock();
    let chip = state
        .chips
        .get_mut(chip_index as usize)
        .ok_or(VfsError::InvalidInput)?;
    let enabled = {
        let e = chip
            .channels
            .get_mut(channel_index as usize)
            .ok_or(VfsError::InvalidInput)?;
        e.duty_ns = value;
        e.enabled
    };
    pwm_apply_channel(chip, channel_index, enabled)
}

fn pwm_write_enable(chip_index: u8, channel_index: u8, data: &[u8]) -> VfsResult<()> {
    let value = parse_u8(data)?;
    if value > 1 {
        return Err(VfsError::InvalidInput);
    }
    let mut state = PWM_SYSFS_STATE.lock();
    let chip = state
        .chips
        .get_mut(chip_index as usize)
        .ok_or(VfsError::InvalidInput)?;
    let channel = PwmChannel::from_u8(channel_index).ok_or(VfsError::InvalidInput)?;
    if value == 1 {
        let period_ns = chip
            .channels
            .get(channel_index as usize)
            .ok_or(VfsError::InvalidInput)?
            .period_ns;
        if period_ns == 0 {
            return Err(VfsError::InvalidInput);
        }
        pwm_apply_channel(chip, channel_index, true)?;
        chip.pwm.set_mode(channel, PwmMode::Continuous);
        chip.pwm.enable_output(channel);
        chip.pwm.start(channel);
        chip.channels[channel_index as usize].enabled = true;
    } else {
        chip.pwm.stop(channel);
        chip.pwm.disable_output(channel);
        chip.channels[channel_index as usize].enabled = false;
    }
    Ok(())
}

fn pwm_apply_channel(chip: &mut PwmChipState, channel_index: u8, running: bool) -> VfsResult<()> {
    let entry = chip
        .channels
        .get(channel_index as usize)
        .ok_or(VfsError::InvalidInput)?;
    if entry.period_ns == 0 {
        return Ok(());
    }
    if entry.duty_ns > entry.period_ns {
        return Err(VfsError::InvalidInput);
    }
    let frequency_hz = (PWM_PERIOD_NS / entry.period_ns) as u32;
    if frequency_hz == 0 {
        return Err(VfsError::InvalidInput);
    }
    let high_percent = (entry.duty_ns * 100 / entry.period_ns) as u8;
    let low_percent = 100u8.saturating_sub(high_percent);
    let channel = PwmChannel::from_u8(channel_index).ok_or(VfsError::InvalidInput)?;
    let result = if running {
        chip.pwm
            .update_frequency_duty(channel, frequency_hz, low_percent)
    } else {
        chip.pwm
            .configure_channel(channel, frequency_hz, low_percent, PwmPolarity::ActiveHigh)
    };
    result.map_err(|_| VfsError::InvalidInput)
}
