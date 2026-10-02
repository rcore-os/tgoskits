//! The single SG2002 mixer element and capture-device discovery.
//! See Linux v6.6 sound/core/control.c and sound/core/pcm.c for ioctl semantics.

use core::sync::atomic::Ordering;

use alsa_pcm_uapi::{Info, control as ctl};
use bytemuck::Zeroable;

use super::{Card, copy_name, map_error, pcm_info};
use crate::{StarryError, StarryResult, mm::UserPtr, task::UserTaskRef};

const CONTROL_VERSION: i32 = 0x02_00_09; // Linux v6.6 SNDRV_CTL_VERSION.
const VOLUME_NAME: &[u8] = b"ADC Capture Volume";

/// Handles control requests without retaining user pointers or holding the
/// stream lock across faultable user copies. Event subscriptions are unsupported.
pub(super) fn ioctl(
    card: &Card,
    current: &UserTaskRef,
    cmd: u32,
    arg: usize,
) -> StarryResult<usize> {
    match cmd {
        ctl::PVERSION => UserPtr::<i32>::from(arg).write(current, CONTROL_VERSION)?,
        ctl::CARD_INFO => {
            let mut info = ctl::CardInfo::zeroed();
            copy_name(&mut info.id, b"SG2002");
            copy_name(&mut info.driver, b"sg200x-audio");
            copy_name(&mut info.name, b"SG2002 Audio");
            copy_name(&mut info.longname, b"SG2002 onboard microphone");
            copy_name(&mut info.mixername, b"SG2002 ADC");
            UserPtr::from(arg).write(current, info)?;
        }
        ctl::PCM_NEXT_DEVICE => {
            let user = UserPtr::<i32>::from(arg);
            let device = user.read(current)?;
            user.write(current, if device < 0 { 0 } else { -1 })?;
        }
        ctl::PCM_INFO => {
            let user = UserPtr::<Info>::from(arg);
            let request = user.read(current)?;
            if !(0..=1).contains(&request.stream) {
                return Err(StarryError::InvalidInput);
            }
            if request.device != 0 {
                return Err(StarryError::NoSuchDeviceOrAddress);
            }
            if request.stream == 0 {
                return Err(StarryError::NotFound);
            }
            if request.subdevice != 0 {
                return Err(StarryError::NoSuchDeviceOrAddress);
            }
            user.write(current, pcm_info(!card.busy.load(Ordering::Acquire)))?;
        }
        ctl::PCM_PREFER_SUBDEVICE => {
            if !matches!(UserPtr::<i32>::from(arg).read(current)?, -1 | 0) {
                return Err(StarryError::NoSuchDevice);
            }
            // Both choices identify the only subdevice; no per-open state needed.
        }
        ctl::SUBSCRIBE_EVENTS => {
            let user = UserPtr::<i32>::from(arg);
            match user.read(current)? {
                -1 => user.write(current, 0)?,
                0 => {}
                1 => return Err(StarryError::OperationNotSupported),
                _ => return Err(StarryError::InvalidInput),
            }
        }
        ctl::ELEM_LIST => {
            let user = UserPtr::<ctl::ElemList>::from(arg);
            let request = user.read(current)?;
            let mut response = ctl::ElemList {
                offset: request.offset,
                space: request.space,
                count: 1,
                ids: request.ids,
                ..ctl::ElemList::zeroed()
            };
            if request.offset == 0 && request.space != 0 {
                let ids = usize::try_from(request.ids).map_err(|_| StarryError::BadAddress)?;
                UserPtr::<ctl::ElemId>::from(ids).write_slice(current, &[volume_id()])?;
                response.used = 1;
            }
            user.write(current, response)?;
        }
        ctl::ELEM_INFO => {
            let user = UserPtr::<ctl::ElemInfo>::from(arg);
            let request = user.read(current)?;
            let mut response = ctl::ElemInfo {
                id: resolve_id(&request.id)?,
                kind: 2,   // INTEGER
                access: 3, // READWRITE; no TLV support.
                count: 1,
                owner: -1, // The element is not locked by a control file.
                ..ctl::ElemInfo::zeroed()
            };
            response.value[1] = 24; // Signed LP64 integer maximum; minimum is zero.
            response.value[2] = 1; // Step in hardware gain units, not dB.
            user.write(current, response)?;
        }
        ctl::ELEM_READ | ctl::ELEM_WRITE => {
            let user = UserPtr::<ctl::ElemValue>::from(arg);
            let request = user.read(current)?;
            let id = resolve_id(&request.id)?;
            if request.indirect != 0 {
                return Err(StarryError::InvalidInput);
            }
            if cmd == ctl::ELEM_WRITE && !(0..=24).contains(&request.value[0]) {
                return Err(StarryError::InvalidInput);
            }
            let gain = {
                let mut stream = card.inner.lock();
                if cmd == ctl::ELEM_WRITE {
                    stream
                        .capture
                        .set_gain(request.value[0] as u8)
                        .map_err(map_error)?;
                }
                stream.capture.gain()
            };
            let mut response = ctl::ElemValue {
                id,
                ..ctl::ElemValue::zeroed()
            };
            response.value[0] = i64::from(gain);
            // As in snd_ctl_elem_write_user, a copy-out fault does not undo a
            // successful hardware write. The stream lock is already released.
            user.write(current, response)?;
        }
        _ => return Err(StarryError::NotATty),
    }
    Ok(0)
}

fn volume_id() -> ctl::ElemId {
    let mut id = ctl::ElemId {
        numid: 1,
        iface: 2, // MIXER
        ..ctl::ElemId::zeroed()
    };
    copy_name(&mut id.name, VOLUME_NAME);
    id
}

fn resolve_id(id: &ctl::ElemId) -> StarryResult<ctl::ElemId> {
    // Linux ignores all other selector fields when numid is nonzero. Name
    // lookup compares a C string, so bytes following its NUL are insignificant.
    let found = if id.numid != 0 {
        id.numid == 1
    } else {
        id.iface == 2
            && id.device == 0
            && id.subdevice == 0
            && id.index == 0
            && id.name.split(|byte| *byte == 0).next() == Some(VOLUME_NAME)
    };
    if found {
        Ok(volume_id())
    } else {
        Err(StarryError::NotFound)
    }
}
