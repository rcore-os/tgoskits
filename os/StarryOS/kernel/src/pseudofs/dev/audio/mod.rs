//! ALSA card-control and PCM character devices.

mod control;
mod pcm;

use alloc::sync::Arc;
use core::any::Any;

use axfs_ng_vfs::{
    DeviceId, MetadataUpdate, NodeFlags, NodeOps, NodePermission, NodeType, VfsError, VfsResult,
};
use linux_raw_sys::general::O_NONBLOCK;
use rdif_audio::PcmDirection;

pub(crate) use self::pcm::PcmFile;
use crate::{
    StarryResult,
    file::{File as KernelFile, FileLike},
    pseudofs::{Device, DirMapping, SimpleDir, SimpleFs},
    sound::SoundService,
};

pub(crate) const SND_DEVICE_ID: DeviceId = DeviceId::new(116, 0);
pub(crate) const PCM_PLAYBACK_DEVICE_ID: DeviceId = DeviceId::new(116, 16);
pub(crate) const PCM_CAPTURE_DEVICE_ID: DeviceId = DeviceId::new(116, 24);

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum AudioNodeKind {
    Control,
    Pcm(PcmDirection),
}

pub(crate) struct AudioDevNode {
    service: Arc<SoundService>,
    kind: AudioNodeKind,
}

impl AudioDevNode {
    fn new(service: Arc<SoundService>, kind: AudioNodeKind) -> Self {
        Self { service, kind }
    }
}

impl crate::pseudofs::DeviceOps for AudioDevNode {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        Err(VfsError::InvalidInput)
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Err(VfsError::InvalidInput)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM
    }
}

pub(crate) fn is_audio_device(inner: &dyn Any) -> bool {
    inner.is::<AudioDevNode>()
}

pub(crate) fn open_audio_file(
    inner: &dyn Any,
    file: ax_fs_ng::File,
    flags: u32,
) -> StarryResult<Arc<dyn FileLike>> {
    let node = inner
        .downcast_ref::<AudioDevNode>()
        .ok_or(crate::StarryError::InvalidInput)?;
    // O_NONBLOCK is mutable open-file-description state owned by
    // `FileLike::nonblocking()`. Keep it out of `KernelFile`'s immutable
    // open flags so F_GETFL cannot report a stale bit after F_SETFL clears it.
    let base = KernelFile::new(file, flags & !O_NONBLOCK);
    match node.kind {
        AudioNodeKind::Control => Ok(Arc::new(control::ControlFile::new(
            base,
            node.service.clone(),
        ))),
        AudioNodeKind::Pcm(direction) => Ok(Arc::new(PcmFile::open(
            base,
            node.service.open_session(direction)?,
        ))),
    }
}

/// Publishes `/dev/snd` only after discovery produced real direction
/// capabilities. The parent devfs builder treats a metadata failure as a
/// broken devfs invariant rather than silently exposing a permissive node.
pub(crate) fn register_devices(root: &mut DirMapping, fs: Arc<SimpleFs>) -> StarryResult<()> {
    let Some(service) = crate::sound::service() else {
        return Ok(());
    };
    let Some(card) = service.publication() else {
        return Ok(());
    };
    if card.playback.is_none() && card.capture.is_none() {
        return Ok(());
    }

    let mut snd = DirMapping::new();
    let dir_fs = fs.clone();
    add_device(
        &mut snd,
        fs.clone(),
        "controlC0",
        SND_DEVICE_ID,
        service.clone(),
        AudioNodeKind::Control,
    )?;
    if card.playback.is_some() {
        add_pcm_device(
            &mut snd,
            fs.clone(),
            "pcmC0D0p",
            PCM_PLAYBACK_DEVICE_ID,
            service.clone(),
            PcmDirection::Playback,
        )?;
    }
    if card.capture.is_some() {
        add_pcm_device(
            &mut snd,
            fs,
            "pcmC0D0c",
            PCM_CAPTURE_DEVICE_ID,
            service,
            PcmDirection::Capture,
        )?;
    }
    root.add("snd", SimpleDir::new_maker(dir_fs, Arc::new(snd)));
    Ok(())
}

fn add_pcm_device(
    root: &mut DirMapping,
    fs: Arc<SimpleFs>,
    name: &str,
    device_id: DeviceId,
    service: Arc<SoundService>,
    direction: PcmDirection,
) -> StarryResult<()> {
    add_device(
        root,
        fs,
        name,
        device_id,
        service,
        AudioNodeKind::Pcm(direction),
    )
}

fn add_device(
    root: &mut DirMapping,
    fs: Arc<SimpleFs>,
    name: &str,
    device_id: DeviceId,
    service: Arc<SoundService>,
    kind: AudioNodeKind,
) -> StarryResult<()> {
    let device = Device::new(
        fs,
        NodeType::CharacterDevice,
        device_id,
        Arc::new(AudioDevNode::new(service, kind)),
    );
    device.update_metadata(MetadataUpdate {
        mode: Some(NodePermission::from_bits_truncate(0o600)),
        owner: Some((0, 0)),
        ..MetadataUpdate::default()
    })?;
    root.add(name, device);
    Ok(())
}
