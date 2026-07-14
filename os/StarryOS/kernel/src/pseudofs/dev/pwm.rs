use alloc::{borrow::Cow, boxed::Box, format, sync::Arc, vec, vec::Vec};

use ax_sync::Mutex;
use axfs_ng_vfs::{NodePermission, VfsError, VfsResult};
use spin::LazyLock;

use crate::pseudofs::{
    DirMaker, DirectRwFsFileOps, NodeOpsMux, RwFile, SimpleDir, SimpleDirOps, SimpleFile,
    SimpleFileOperation, SimpleFileOps, SimpleFs, SpecialFsFile,
};

mod platform;

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
    hw: platform::PwmHardware,
    channels: Vec<PwmChannelState>,
}

struct PwmSysfsState {
    chips: Vec<PwmChipState>,
}

struct PwmAttrFile {
    ops: Arc<dyn SimpleFileOps>,
}

// PWM hardware state is only accessed while holding `PWM_SYSFS_STATE`.
unsafe impl Send for PwmSysfsState {}

impl PwmSysfsState {
    fn new() -> Self {
        let mut chips = Vec::with_capacity(platform::pwm_chip_count() as usize);
        for index in 0..platform::pwm_chip_count() {
            chips.push(PwmChipState {
                hw: platform::PwmHardware::new(index),
                channels: vec![
                    PwmChannelState::default();
                    platform::pwm_channels_per_chip(index) as usize
                ],
            });
        }
        Self { chips }
    }
}

static PWM_SYSFS_STATE: LazyLock<Mutex<PwmSysfsState>> =
    LazyLock::new(|| Mutex::new(PwmSysfsState::new()));

impl PwmAttrFile {
    fn new(ops: impl SimpleFileOps) -> Self {
        Self { ops: Arc::new(ops) }
    }
}

impl DirectRwFsFileOps for PwmAttrFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let data = self.ops.read_all()?;
        if offset >= data.len() as u64 {
            return Ok(0);
        }
        let data = &data[offset as usize..];
        let read = data.len().min(buf.len());
        buf[..read].copy_from_slice(&data[..read]);
        Ok(read)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize> {
        if offset != 0 {
            return Err(VfsError::InvalidInput);
        }
        // Sysfs attribute writes replace the attribute value. Do this locally
        // for PWM instead of changing the generic SimpleFile write contract.
        self.ops.write_all(buf)?;
        Ok(buf.len())
    }
}

fn pwm_attr_file(fs: Arc<SimpleFs>, ops: impl SimpleFileOps) -> Arc<SpecialFsFile<PwmAttrFile>> {
    SpecialFsFile::new_regular_with_perm(fs, PwmAttrFile::new(ops), NodePermission::default())
}

struct PwmClassDir {
    fs: Arc<SimpleFs>,
}

impl SimpleDirOps for PwmClassDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(
            (0..platform::pwm_chip_count())
                .map(|index| Cow::Owned(format!("pwmchip{}", platform::pwmchip_number(index)))),
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
        let mut names = vec![
            Cow::Borrowed("export"),
            Cow::Borrowed("unexport"),
            Cow::Borrowed("npwm"),
        ];
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
            "export" => Ok(pwm_attr_file(
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
            "unexport" => Ok(pwm_attr_file(
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
            "npwm" => Ok(SimpleFile::new_regular(self.fs.clone(), {
                let chip_index = self.chip_index;
                move || {
                    let state = PWM_SYSFS_STATE.lock();
                    let channels = state
                        .chips
                        .get(chip_index as usize)
                        .map(|chip| chip.channels.len())
                        .unwrap_or(0);
                    Ok(format!("{channels}\n"))
                }
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
            "period" => pwm_attr_file(
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
            "duty_cycle" => pwm_attr_file(
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
            "enable" => pwm_attr_file(
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
    platform::pwmchip_index(value)
}

fn parse_pwm_local_index(name: &str) -> Option<u8> {
    name.strip_prefix("pwm")?.parse::<u8>().ok()
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
    let mut state = PWM_SYSFS_STATE.lock();
    let chip = state
        .chips
        .get_mut(chip_index as usize)
        .ok_or(VfsError::InvalidInput)?;
    let entry = chip
        .channels
        .get_mut(channel as usize)
        .ok_or(VfsError::InvalidInput)?;
    if !entry.exported {
        *entry = PwmChannelState {
            exported: true,
            ..Default::default()
        };
    }
    Ok(())
}

fn unexport_pwm_channel(chip_index: u8, channel: u8) -> VfsResult<()> {
    let mut state = PWM_SYSFS_STATE.lock();
    let chip = state
        .chips
        .get_mut(chip_index as usize)
        .ok_or(VfsError::InvalidInput)?;
    {
        let entry = chip
            .channels
            .get(channel as usize)
            .ok_or(VfsError::InvalidInput)?;
        if entry.enabled {
            platform::disable_channel(&mut chip.hw, channel)?;
        }
    }
    chip.channels[channel as usize] = PwmChannelState::default();
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
    if let Err(err) = pwm_apply_channel(chip, channel_index, enabled) {
        warn!("pwmchip{chip_index}/pwm{channel_index}: apply failed: {err:?}");
        return Err(err);
    }
    Ok(())
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
        chip.channels[channel_index as usize].enabled = true;
    } else {
        chip.channels
            .get(channel_index as usize)
            .ok_or(VfsError::InvalidInput)?;
        platform::disable_channel(&mut chip.hw, channel_index)?;
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
    platform::apply_channel(
        &mut chip.hw,
        channel_index,
        entry.period_ns,
        entry.duty_ns,
        running,
    )
}
