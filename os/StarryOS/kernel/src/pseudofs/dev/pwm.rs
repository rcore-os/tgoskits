use alloc::{borrow::Cow, boxed::Box, format, sync::Arc, vec::Vec};

use ax_lazyinit::LazyLock;
use axfs_ng_vfs::{NodePermission, VfsError, VfsResult};
use rdif_pwm::{Pwm, PwmError, PwmPolarity, PwmState};

use crate::{
    pseudofs::{
        DirMaker, DirectRwFsFileOps, NodeOpsMux, RwFile, SimpleDir, SimpleDirOps, SimpleFile,
        SimpleFileOperation, SimpleFileOps, SimpleFs, SpecialFsFile,
    },
    sync::Mutex,
};

/// The class exists even on platforms without a PWM controller.
pub(crate) fn pwm_class_dir_maker(fs: Arc<SimpleFs>) -> DirMaker {
    SimpleDir::new_maker(fs.clone(), Arc::new(PwmClassDir { fs }))
}

struct PwmChannel {
    exported: bool,
    generation: usize,
    requested: PwmState,
}
impl PwmChannel {
    fn export(&mut self) -> VfsResult<()> {
        if self.exported {
            return Err(VfsError::ResourceBusy);
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(VfsError::ValueOverflow)?;
        self.exported = true;
        Ok(())
    }

    fn unexport(&mut self, controller: &mut Pwm, channel: usize) -> VfsResult<()> {
        if !self.exported {
            return Err(VfsError::NoSuchDevice);
        }
        controller.disable(channel).map_err(map_error)?;
        self.requested.enabled = false;
        self.exported = false;
        Ok(())
    }

    fn apply(&mut self, controller: &mut Pwm, channel: usize, next: PwmState) -> VfsResult<()> {
        controller.apply(channel, next).map_err(map_error)?;
        self.requested = next;
        Ok(())
    }
}

struct PwmChip {
    number: usize,
    device: rdrive::Device<Pwm>,
    channels: Mutex<Vec<PwmChannel>>,
}

// Device IDs are allocated in firmware enumeration order. The registry is the
// only discovery source; sysfs never matches vendor compatibles or maps MMIO.
static CHIPS: LazyLock<Vec<PwmChip>> = LazyLock::new(|| {
    let mut chips = Vec::new();
    let mut number = 0;
    for device in rdrive::get_list::<Pwm>() {
        let result = (|| {
            let mut controller = device.lock().map_err(|_| VfsError::Io)?;
            (0..controller.channel_count())
                .map(|channel| {
                    controller
                        .get_state(channel)
                        .map(|requested| PwmChannel {
                            exported: false,
                            generation: 0,
                            requested,
                        })
                        .map_err(map_error)
                })
                .collect::<VfsResult<Vec<_>>>()
        })();
        match result {
            Ok(channels) if !channels.is_empty() => {
                let count = channels.len();
                chips.push(PwmChip {
                    number,
                    device,
                    channels: Mutex::new(channels),
                });
                number += count;
            }
            Ok(_) => warn!("PWM controller has no channels"),
            Err(err) => warn!(
                "PWM controller {:?} state unavailable: {err:?}",
                device.descriptor().device_id()
            ),
        }
    }
    chips
});

struct PwmAttrFile {
    ops: Arc<dyn SimpleFileOps>,
}
impl DirectRwFsFileOps for PwmAttrFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let data = self.ops.read_all()?;
        if offset >= data.len() as u64 {
            return Ok(0);
        }
        let data = &data[offset as usize..];
        let len = buf.len().min(data.len());
        buf[..len].copy_from_slice(&data[..len]);
        Ok(len)
    }
    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize> {
        if offset != 0 {
            return Err(VfsError::InvalidInput);
        }
        self.ops.write_all(buf)?;
        Ok(buf.len())
    }
}
fn attribute(
    fs: Arc<SimpleFs>,
    permissions: NodePermission,
    ops: impl SimpleFileOps,
) -> NodeOpsMux {
    SpecialFsFile::new_regular_with_perm(fs, PwmAttrFile { ops: Arc::new(ops) }, permissions).into()
}
struct PwmClassDir {
    fs: Arc<SimpleFs>,
}
impl SimpleDirOps for PwmClassDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(
            CHIPS
                .iter()
                .map(|chip| Cow::Owned(format!("pwmchip{}", chip.number))),
        )
    }
    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let number = name
            .strip_prefix("pwmchip")
            .and_then(|v| v.parse::<usize>().ok())
            .ok_or(VfsError::NotFound)?;
        let index = CHIPS
            .iter()
            .position(|chip| chip.number == number)
            .ok_or(VfsError::NotFound)?;
        Ok(NodeOpsMux::Dir(SimpleDir::new_maker(
            self.fs.clone(),
            Arc::new(PwmChipDir {
                fs: self.fs.clone(),
                index,
            }),
        )))
    }
}
struct PwmChipDir {
    fs: Arc<SimpleFs>,
    index: usize,
}
impl SimpleDirOps for PwmChipDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        let mut names = alloc::vec![
            Cow::Borrowed("export"),
            Cow::Borrowed("unexport"),
            Cow::Borrowed("npwm")
        ];
        for (index, channel) in CHIPS[self.index].channels.lock().iter().enumerate() {
            if channel.exported {
                names.push(Cow::Owned(format!("pwm{index}")));
            }
        }
        Box::new(names.into_iter())
    }
    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let index = self.index;
        match name {
            "export" | "unexport" => {
                let export = name == "export";
                Ok(attribute(
                    self.fs.clone(),
                    NodePermission::OWNER_WRITE,
                    RwFile::new(move |request| match request {
                        SimpleFileOperation::Read => Err(VfsError::PermissionDenied),
                        SimpleFileOperation::Write(data) => {
                            let channel = usize::try_from(parse_number(data)?)
                                .map_err(|_| VfsError::InvalidInput)?;
                            set_exported(index, channel, export)?;
                            Ok(None::<Vec<u8>>)
                        }
                    }),
                ))
            }
            "npwm" => Ok(SimpleFile::new_regular(self.fs.clone(), move || {
                Ok(format!("{}\n", CHIPS[index].channels.lock().len()))
            })
            .into()),
            _ => {
                let channel = name
                    .strip_prefix("pwm")
                    .and_then(|v| v.parse::<usize>().ok())
                    .ok_or(VfsError::NotFound)?;
                let generation =
                    exported_channel(&CHIPS[index].channels.lock(), channel, None)?.generation;
                Ok(NodeOpsMux::Dir(SimpleDir::new_maker(
                    self.fs.clone(),
                    Arc::new(PwmChannelDir {
                        fs: self.fs.clone(),
                        index,
                        channel,
                        generation,
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
    index: usize,
    channel: usize,
    generation: usize,
}
#[derive(Clone, Copy)]
enum Attribute {
    Period,
    Duty,
    Enable,
    Polarity,
}
impl SimpleDirOps for PwmChannelDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(
            ["period", "duty_cycle", "enable", "polarity"]
                .into_iter()
                .map(Cow::Borrowed),
        )
    }
    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let attr = match name {
            "period" => Attribute::Period,
            "duty_cycle" => Attribute::Duty,
            "enable" => Attribute::Enable,
            "polarity" => Attribute::Polarity,
            _ => return Err(VfsError::NotFound),
        };
        let index = self.index;
        let channel = self.channel;
        let generation = self.generation;
        exported_channel(&CHIPS[index].channels.lock(), channel, Some(generation))?;
        Ok(attribute(
            self.fs.clone(),
            NodePermission::from_bits_truncate(0o644),
            RwFile::new(move |request| match request {
                SimpleFileOperation::Read => {
                    let channels = CHIPS[index].channels.lock();
                    let state = exported_channel(&channels, channel, Some(generation))?.requested;
                    let value = match attr {
                        Attribute::Period => format!("{}\n", state.period_ns),
                        Attribute::Duty => format!("{}\n", state.duty_ns),
                        Attribute::Enable => format!("{}\n", u8::from(state.enabled)),
                        Attribute::Polarity => match state.polarity {
                            PwmPolarity::Normal => "normal\n".into(),
                            PwmPolarity::Inversed => "inversed\n".into(),
                        },
                    };
                    Ok(Some(value.into_bytes()))
                }
                SimpleFileOperation::Write(data) => {
                    write_attribute(index, channel, generation, attr, data)?;
                    Ok(None)
                }
            }),
        ))
    }
    fn is_cacheable(&self) -> bool {
        false
    }
}
fn parse_number(data: &[u8]) -> VfsResult<u64> {
    core::str::from_utf8(data)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .ok_or(VfsError::InvalidInput)
}
fn exported_channel(
    channels: &[PwmChannel],
    channel: usize,
    generation: Option<usize>,
) -> VfsResult<&PwmChannel> {
    let entry = channels.get(channel).ok_or(VfsError::NotFound)?;
    // An open attribute belongs to one export lifetime. Re-exporting the same
    // hardware channel must not revive descriptors from the removed directory.
    if generation.is_some_and(|value| value != entry.generation || !entry.exported) {
        return Err(VfsError::NoSuchDevice);
    }
    if !entry.exported {
        return Err(VfsError::NotFound);
    }
    Ok(entry)
}
fn set_exported(index: usize, channel: usize, export: bool) -> VfsResult<()> {
    let chip = &CHIPS[index];
    let mut channels = chip.channels.lock();
    let entry = channels.get_mut(channel).ok_or(VfsError::NoSuchDevice)?;
    if export {
        entry.export()
    } else {
        let mut controller = chip.device.lock().map_err(|_| VfsError::Io)?;
        entry.unexport(&mut controller, channel)
    }
}
fn write_attribute(
    index: usize,
    channel: usize,
    generation: usize,
    attr: Attribute,
    data: &[u8],
) -> VfsResult<()> {
    let chip = &CHIPS[index];
    // Lock order: per-chip sysfs state -> rdrive controller -> clock provider.
    // No driver callback enters sysfs. The candidate is published only on success.
    let mut channels = chip.channels.lock();
    let mut next = exported_channel(&channels, channel, Some(generation))?.requested;
    match attr {
        Attribute::Period => next.period_ns = parse_number(data)?,
        Attribute::Duty => next.duty_ns = parse_number(data)?,
        Attribute::Enable => {
            next.enabled = match parse_number(data)? {
                0 => false,
                1 => true,
                _ => return Err(VfsError::InvalidInput),
            }
        }
        Attribute::Polarity => {
            next.polarity = match core::str::from_utf8(data)
                .map_err(|_| VfsError::InvalidInput)?
                .trim()
            {
                "normal" => PwmPolarity::Normal,
                "inversed" => PwmPolarity::Inversed,
                _ => return Err(VfsError::InvalidInput),
            }
        }
    }
    let mut controller = chip.device.lock().map_err(|_| VfsError::Io)?;
    channels[channel].apply(&mut controller, channel, next)
}
fn map_error(error: PwmError) -> VfsError {
    match error {
        PwmError::InvalidChannel | PwmError::InvalidPeriod | PwmError::InvalidDuty => {
            VfsError::InvalidInput
        }
        PwmError::UnsupportedPolarity => VfsError::OperationNotSupported,
        PwmError::Clock | PwmError::InvalidMapping => VfsError::Io,
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use rdif_pwm::{DriverGeneric, Interface};

    use super::*;

    // The failure is at the hardware boundary; no OS runtime is replaced.
    struct UnavailableOutput;
    impl DriverGeneric for UnavailableOutput {
        fn name(&self) -> &str {
            "unavailable-output"
        }
    }
    impl Interface for UnavailableOutput {
        fn channel_count(&self) -> usize {
            1
        }
        fn get_state(&mut self, _channel: usize) -> Result<PwmState, PwmError> {
            Ok(PwmState::normal(0, 0, false))
        }
        fn apply(&mut self, _channel: usize, state: PwmState) -> Result<(), PwmError> {
            if state.enabled {
                Err(PwmError::Clock)
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn failed_submission_preserves_requested_state() {
        let mut controller = Pwm::new(UnavailableOutput);
        let initial = PwmState::normal(1_000_000, 250_000, false);
        let mut channel = PwmChannel {
            exported: false,
            generation: 0,
            requested: initial,
        };
        channel.export().unwrap();
        assert_eq!(channel.export(), Err(VfsError::ResourceBusy));
        let rejected = PwmState {
            duty_ns: 300_000,
            enabled: true,
            polarity: PwmPolarity::Inversed,
            ..initial
        };
        assert_eq!(
            channel.apply(&mut controller, 0, rejected),
            Err(VfsError::Io)
        );
        assert_eq!(channel.requested, initial);
        assert!(channel.exported);
        let accepted = PwmState {
            enabled: false,
            ..rejected
        };
        channel.apply(&mut controller, 0, accepted).unwrap();
        assert_eq!(channel.requested, accepted);
        let mut channels = [channel];
        let generation = channels[0].generation;
        channels[0].unexport(&mut controller, 0).unwrap();
        assert!(matches!(
            exported_channel(&channels, 0, Some(generation)),
            Err(VfsError::NoSuchDevice)
        ));
        channels[0].export().unwrap();
        assert!(matches!(
            exported_channel(&channels, 0, Some(generation)),
            Err(VfsError::NoSuchDevice)
        ));
        assert!(exported_channel(&channels, 0, Some(channels[0].generation)).is_ok());
        channels[0].unexport(&mut controller, 0).unwrap();
        assert_eq!(
            channels[0].unexport(&mut controller, 0),
            Err(VfsError::NoSuchDevice)
        );
    }
}
