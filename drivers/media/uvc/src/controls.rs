//! UVC controls — Processing Unit / Camera Terminal → V4L2 (UVC 1.5 §4.2.2).

use alloc::{boxed::Box, sync::Arc, vec::Vec};

use ax_media::{
    CtrlConfig, CtrlGetFn, CtrlSetFn, CtrlType,
    class::{CameraClassCtrl, CtrlClass, UserClassCtrl},
    interface::ctrl::CtrlFlags,
    videobuffer::VbMemOps,
};
use crab_usb::usb_if::{
    host::ControlSetup,
    transfer::{Recipient, RequestType},
};

use crate::{
    UvcDevice, UvcHandle,
    descriptors::{
        ControlCapabilities, RequestCode, camera_terminal_controls, processing_unit_controls,
    },
};

/// Parsed VC units.
#[derive(Debug, Default, Clone)]
pub(crate) struct VcUnits {
    pub camera_terminal_id: Option<u8>,
    pub camera_controls: Vec<u8>,
    pub processing_unit_id: Option<u8>,
    pub processing_controls: Vec<u8>,
}

/// Power line frequency menu.
static POWER_LINE_FREQ_MENU: [&str; 4] = ["Disabled", "50 Hz", "60 Hz", "Auto"];

/// Exposure auto menu.
const EXPOSURE_AUTO_MENU: &[&str] = &[
    "Auto Mode",
    "Manual Mode",
    "Shutter Priority Mode",
    "Aperture Priority Mode",
];

/// UVC CT_AE_MODE_CONTROL bit values in V4L2_EXPOSURE_* menu order.
const EXPOSURE_AUTO_UVC_VALUES: [i64; 4] = [2, 1, 4, 8];

fn exposure_auto_index(raw: i64) -> Option<i64> {
    EXPOSURE_AUTO_UVC_VALUES
        .iter()
        .position(|&mode| mode == raw)
        .map(|index| index as i64)
}

fn menu_value_supported(mask: u64, raw: i64) -> bool {
    u32::try_from(raw)
        .ok()
        .and_then(|index| 1u64.checked_shl(index))
        .is_some_and(|bit| mask & bit != 0)
}

/// Representable range when a write-only control cannot report GET_MIN/MAX.
fn uvc_wire_range(size: usize, signed: bool) -> Option<(i64, i64)> {
    match (size, signed) {
        (1, false) => Some((0, u8::MAX as i64)),
        (1, true) => Some((i8::MIN as i64, i8::MAX as i64)),
        (2, false) => Some((0, u16::MAX as i64)),
        (2, true) => Some((i16::MIN as i64, i16::MAX as i64)),
        (4, false) => Some((0, i32::MAX as i64)),
        (4, true) => Some((i32::MIN as i64, i32::MAX as i64)),
        _ => None,
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UvcCtrlType {
    Integer,
    Boolean,
    Menu(&'static [&'static str]),
    Button,
}

struct UvcControlDef {
    cid: u32,
    name: &'static str,
    selector: u8,
    size: usize,
    signed: bool,
    ctrl_bit: u8,
    ty: UvcCtrlType,
}

const UVC_CONTROL_PU_DEFS: &[UvcControlDef] = &[
    UvcControlDef {
        cid: UserClassCtrl::Brightness as u32,
        name: "Brightness",
        selector: processing_unit_controls::BRIGHTNESS,
        size: 2,
        signed: true,
        ctrl_bit: 0,
        ty: UvcCtrlType::Integer,
    },
    UvcControlDef {
        cid: UserClassCtrl::Contrast as u32,
        name: "Contrast",
        selector: processing_unit_controls::CONTRAST,
        size: 2,
        signed: false,
        ctrl_bit: 1,
        ty: UvcCtrlType::Integer,
    },
    UvcControlDef {
        cid: UserClassCtrl::Hue as u32,
        name: "Hue",
        selector: processing_unit_controls::HUE,
        size: 2,
        signed: true,
        ctrl_bit: 2,
        ty: UvcCtrlType::Integer,
    },
    UvcControlDef {
        cid: UserClassCtrl::Saturation as u32,
        name: "Saturation",
        selector: processing_unit_controls::SATURATION,
        size: 2,
        signed: false,
        ctrl_bit: 3,
        ty: UvcCtrlType::Integer,
    },
    UvcControlDef {
        cid: UserClassCtrl::Sharpness as u32,
        name: "Sharpness",
        selector: processing_unit_controls::SHARPNESS,
        size: 2,
        signed: false,
        ctrl_bit: 4,
        ty: UvcCtrlType::Integer,
    },
    UvcControlDef {
        cid: UserClassCtrl::Gamma as u32,
        name: "Gamma",
        selector: processing_unit_controls::GAMMA,
        size: 2,
        signed: false,
        ctrl_bit: 5,
        ty: UvcCtrlType::Integer,
    },
    UvcControlDef {
        cid: UserClassCtrl::WhiteBalanceTemperature as u32,
        name: "White Balance Temperature",
        selector: processing_unit_controls::WHITE_BALANCE_TEMPERATURE,
        size: 2,
        signed: false,
        ctrl_bit: 6,
        ty: UvcCtrlType::Integer,
    },
    UvcControlDef {
        cid: UserClassCtrl::BacklightCompensation as u32,
        name: "Backlight Compensation",
        selector: processing_unit_controls::BACKLIGHT_COMPENSATION,
        size: 2,
        signed: false,
        ctrl_bit: 8,
        ty: UvcCtrlType::Integer,
    },
    UvcControlDef {
        cid: UserClassCtrl::Gain as u32,
        name: "Gain",
        selector: processing_unit_controls::GAIN,
        size: 2,
        signed: false,
        ctrl_bit: 9,
        ty: UvcCtrlType::Integer,
    },
    UvcControlDef {
        cid: UserClassCtrl::PowerLineFrequency as u32,
        name: "Power Line Frequency",
        selector: processing_unit_controls::POWER_LINE_FREQUENCY,
        size: 1,
        signed: false,
        ctrl_bit: 10,
        ty: UvcCtrlType::Menu(&POWER_LINE_FREQ_MENU),
    },
    UvcControlDef {
        cid: UserClassCtrl::HueAuto as u32,
        name: "Hue Auto",
        selector: processing_unit_controls::HUE_AUTO,
        size: 1,
        signed: false,
        ctrl_bit: 11,
        ty: UvcCtrlType::Boolean,
    },
    UvcControlDef {
        cid: UserClassCtrl::AutoWhiteBalance as u32,
        name: "Auto White Balance",
        selector: processing_unit_controls::WHITE_BALANCE_TEMPERATURE_AUTO,
        size: 1,
        signed: false,
        ctrl_bit: 12,
        ty: UvcCtrlType::Boolean,
    },
];

const UVC_CONTROL_CT_DEFS: &[UvcControlDef] = &[
    UvcControlDef {
        cid: CameraClassCtrl::ExposureAuto as u32,
        name: "Exposure, Auto",
        selector: camera_terminal_controls::AE_MODE,
        size: 1,
        signed: false,
        ctrl_bit: 1,
        ty: UvcCtrlType::Menu(EXPOSURE_AUTO_MENU),
    },
    UvcControlDef {
        cid: CameraClassCtrl::ExposureAutoPriority as u32,
        name: "Exposure, Auto Priority",
        selector: camera_terminal_controls::AE_PRIORITY,
        size: 1,
        signed: false,
        ctrl_bit: 2,
        ty: UvcCtrlType::Boolean,
    },
    UvcControlDef {
        cid: CameraClassCtrl::ExposureAbsolute as u32,
        name: "Exposure (Absolute)",
        selector: camera_terminal_controls::EXPOSURE_TIME_ABSOLUTE,
        size: 4,
        signed: false,
        ctrl_bit: 3,
        ty: UvcCtrlType::Integer,
    },
    UvcControlDef {
        cid: CameraClassCtrl::FocusAbsolute as u32,
        name: "Focus (Absolute)",
        selector: camera_terminal_controls::FOCUS_ABSOLUTE,
        size: 2,
        signed: false,
        ctrl_bit: 5,
        ty: UvcCtrlType::Integer,
    },
    UvcControlDef {
        cid: CameraClassCtrl::FocusAuto as u32,
        name: "Focus, Auto",
        selector: camera_terminal_controls::FOCUS_AUTO,
        size: 1,
        signed: false,
        ctrl_bit: 17,
        ty: UvcCtrlType::Boolean,
    },
    UvcControlDef {
        cid: CameraClassCtrl::IrisAbsolute as u32,
        name: "Iris, Absolute",
        selector: camera_terminal_controls::IRIS_ABSOLUTE,
        size: 2,
        signed: false,
        ctrl_bit: 7,
        ty: UvcCtrlType::Integer,
    },
    UvcControlDef {
        cid: CameraClassCtrl::ZoomAbsolute as u32,
        name: "Zoom, Absolute",
        selector: camera_terminal_controls::ZOOM_ABSOLUTE,
        size: 2,
        signed: false,
        ctrl_bit: 9,
        ty: UvcCtrlType::Integer,
    },
    UvcControlDef {
        cid: CameraClassCtrl::Privacy as u32,
        name: "Privacy",
        selector: camera_terminal_controls::PRIVACY,
        size: 1,
        signed: false,
        ctrl_bit: 18,
        ty: UvcCtrlType::Boolean,
    },
];

fn control_supported(bitmap: &[u8], bit: u8) -> bool {
    let byte = (bit / 8) as usize;
    let b = bit % 8;
    bitmap.get(byte).is_some_and(|v| (v >> b) & 1 == 1)
}

fn decode_uvc_value(buf: &[u8], signed: bool) -> Option<i64> {
    match buf.len() {
        1 => Some(buf[0] as i64),
        2 if signed => Some(i16::from_le_bytes([buf[0], buf[1]]) as i64),
        2 => Some(u16::from_le_bytes([buf[0], buf[1]]) as i64),
        4 if signed => Some(i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as i64),
        4 => Some(u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as i64),
        _ => None,
    }
}

fn encode_uvc_value(v: i64, size: usize) -> Option<Vec<u8>> {
    match size {
        1 => Some(vec![v as u8]),
        2 => Some((v as i16).to_le_bytes().to_vec()),
        4 => Some((v as i32).to_le_bytes().to_vec()),
        _ => None,
    }
}

/// Register a single control.
#[allow(clippy::too_many_arguments)]
fn register_control<H: UvcHandle>(
    ctrls: &mut ax_media::CtrlHandler,
    handle: &Arc<H>,
    vc_iface: u8,
    unit_id: u8,
    bitmap: &[u8],
    def: &UvcControlDef,
    uvc_version: u16,
    log_tag: &str,
) {
    let cid_raw = def.cid;
    let name = def.name;
    let sel_raw = def.selector;
    let size = def.size;
    let signed = def.signed;
    let ctrl_bit = def.ctrl_bit;
    let ty = def.ty;
    if ctrls.find(cid_raw).is_some() {
        return;
    }
    if !control_supported(bitmap, ctrl_bit) {
        return;
    }

    let info_byte = {
        let mut buf = vec![0u8; 1];
        let setup = ControlSetup {
            request_type: RequestType::Class,
            recipient: Recipient::Interface,
            request: RequestCode::GetInfo.into(),
            value: (sel_raw as u16) << 8,
            index: ((unit_id as u16) << 8) | vc_iface as u16,
        };
        match handle.control_in(setup, &mut buf) {
            Ok(1) => buf[0],
            other => {
                log::debug!("uvc: {log_tag} {name} GetInfo err sel {sel_raw:#x}: {other:?}");
                return;
            }
        }
    };
    let caps = ControlCapabilities::from_bits_truncate(info_byte);
    if caps.contains(ControlCapabilities::DISABLED) {
        log::debug!("uvc: {log_tag} {name} disabled info={info_byte:#x}");
        return;
    }
    let readable = caps.contains(ControlCapabilities::GET);
    let writable = caps.contains(ControlCapabilities::SET);
    if !readable && !writable {
        log::debug!("uvc: {log_tag} {name} no GET/SET support info={info_byte:#x}");
        return;
    }

    let read = {
        let handle = handle.clone();
        move |request: RequestCode| -> Option<i64> {
            let mut buf = vec![0u8; size];
            let setup = ControlSetup {
                request_type: RequestType::Class,
                recipient: Recipient::Interface,
                request: request.into(),
                value: (sel_raw as u16) << 8,
                index: ((unit_id as u16) << 8) | vc_iface as u16,
            };
            if handle.control_in(setup, &mut buf).ok()? != size {
                return None;
            }
            decode_uvc_value(&buf, signed)
        }
    };

    let power_line_mask = if cid_raw == UserClassCtrl::PowerLineFrequency as u32 {
        let Some(current) = read(RequestCode::GetCur).and_then(|raw| u8::try_from(raw).ok()) else {
            return;
        };
        let mut supported = 0b0111u64;
        if writable {
            let set = |value: u8| {
                let setup = ControlSetup {
                    request_type: RequestType::Class,
                    recipient: Recipient::Interface,
                    request: RequestCode::SetCur.into(),
                    value: (sel_raw as u16) << 8,
                    index: ((unit_id as u16) << 8) | vc_iface as u16,
                };
                handle.control_out(setup, &[value]).is_ok()
            };
            if set(0) {
                if uvc_version >= 0x150 && set(3) {
                    supported |= 1 << 3;
                }
            } else {
                supported &= !1;
            }
            // A failed restore does not invalidate the menu; G_CTRL reads the live value.
            if !set(current) {
                log::warn!("uvc: failed to restore {log_tag} {name} after capability probe");
            }
        } else if uvc_version >= 0x150 && (current == 3 || read(RequestCode::GetDef) == Some(3)) {
            supported |= 1 << 3;
        }
        if !menu_value_supported(supported, current.into()) {
            log::warn!("uvc: skip {log_tag} {name}: current value {current} is not settable");
            return;
        }
        Some(supported)
    } else {
        None
    };

    let h = handle.clone();
    let get_fn: CtrlGetFn = Box::new(move || {
        let mut buf = vec![0u8; size];
        let setup = ControlSetup {
            request_type: RequestType::Class,
            recipient: Recipient::Interface,
            request: RequestCode::GetCur.into(),
            value: (sel_raw as u16) << 8,
            index: ((unit_id as u16) << 8) | vc_iface as u16,
        };
        if h.control_in(setup, &mut buf)
            .map_err(|_| ax_media::V4l2Error::Io)?
            != size
        {
            return Err(ax_media::V4l2Error::Io);
        }
        let raw = decode_uvc_value(&buf, signed).ok_or(ax_media::V4l2Error::Io)?;
        if cid_raw == CameraClassCtrl::ExposureAuto as u32 {
            exposure_auto_index(raw).ok_or(ax_media::V4l2Error::Io)
        } else if let Some(mask) = power_line_mask {
            menu_value_supported(mask, raw)
                .then_some(raw)
                .ok_or(ax_media::V4l2Error::Io)
        } else {
            Ok(raw)
        }
    });

    let h = handle.clone();
    let set_fn: CtrlSetFn = Box::new(move |v| {
        let orig_v = v;
        let v = if cid_raw == CameraClassCtrl::ExposureAuto as u32 {
            *EXPOSURE_AUTO_UVC_VALUES
                .get(usize::try_from(v).map_err(|_| ax_media::V4l2Error::InvalidArgument)?)
                .ok_or(ax_media::V4l2Error::InvalidArgument)?
        } else {
            v
        };
        let buf = encode_uvc_value(v, size).ok_or(ax_media::V4l2Error::Io)?;
        let setup = ControlSetup {
            request_type: RequestType::Class,
            recipient: Recipient::Interface,
            request: RequestCode::SetCur.into(),
            value: (sel_raw as u16) << 8,
            index: ((unit_id as u16) << 8) | vc_iface as u16,
        };
        h.control_out(setup, &buf)
            .map_err(|_| ax_media::V4l2Error::Io)?;
        Ok(orig_v)
    });

    let ops = ax_media::CtrlOps {
        get: Some(get_fn),
        try_ctrl: None,
        set: set_fn,
    };

    let res = match ty {
        UvcCtrlType::Integer => {
            let (min, max) = if readable {
                let Some(min) = read(RequestCode::GetMin) else {
                    return;
                };
                let Some(max) = read(RequestCode::GetMax) else {
                    return;
                };
                (min, max)
            } else {
                let Some(range) = uvc_wire_range(size, signed) else {
                    return;
                };
                range
            };
            // V4L2 integer controls expose signed 32-bit values.
            let min = min.max(i32::MIN as i64);
            let max = max.min(i32::MAX as i64);
            if min > max {
                return;
            }
            let step = if readable {
                read(RequestCode::GetRes).unwrap_or(1).max(1)
            } else {
                1
            };
            let default = if readable {
                read(RequestCode::GetDef).unwrap_or(min)
            } else {
                0
            };
            ctrls.new_int(cid_raw, name, min, max, step, default, Some(ops))
        }
        UvcCtrlType::Boolean => {
            let default = if readable {
                read(RequestCode::GetDef).unwrap_or(0)
            } else {
                0
            };
            ctrls.new_bool(cid_raw, name, default != 0, Some(ops))
        }
        UvcCtrlType::Menu(qmenu) => {
            let (qmenu, default_idx, skipped) = if cid_raw == CameraClassCtrl::ExposureAuto as u32 {
                let supported = if readable {
                    read(RequestCode::GetRes)
                        .or_else(|| read(RequestCode::GetMax))
                        .unwrap_or(0x0f)
                } else {
                    0x0f
                };
                let skipped = EXPOSURE_AUTO_UVC_VALUES.iter().enumerate().fold(
                    0u64,
                    |mask, (index, mode)| {
                        if (supported & *mode) == 0 {
                            mask | (1u64 << index)
                        } else {
                            mask
                        }
                    },
                );
                let Some(first_supported) = (0..EXPOSURE_AUTO_UVC_VALUES.len())
                    .find(|index| (skipped & (1u64 << index)) == 0)
                else {
                    return;
                };
                let default_idx = if readable {
                    read(RequestCode::GetDef)
                        .and_then(exposure_auto_index)
                        .and_then(|index| u32::try_from(index).ok())
                        .filter(|index| (skipped & (1u64 << index)) == 0)
                        .unwrap_or(first_supported as u32)
                } else {
                    first_supported as u32
                };
                (qmenu, default_idx, skipped)
            } else if let Some(mask) = power_line_mask {
                let qmenu: &'static [&'static str] = if menu_value_supported(mask, 3) {
                    &POWER_LINE_FREQ_MENU
                } else {
                    &POWER_LINE_FREQ_MENU[..3]
                };
                let first_supported = (0..qmenu.len())
                    .find(|index| menu_value_supported(mask, *index as i64))
                    .unwrap_or(0);
                let default = match read(RequestCode::GetDef) {
                    Some(value) if menu_value_supported(mask, value) => value as u32,
                    Some(value) => {
                        log::warn!(
                            "uvc: skip {log_tag} {name}: default value {value} is not settable"
                        );
                        return;
                    }
                    None => first_supported as u32,
                };
                (qmenu, default, !mask & ((1u64 << qmenu.len()) - 1))
            } else {
                let default = if readable {
                    read(RequestCode::GetDef).unwrap_or(0)
                } else {
                    0
                };
                (qmenu, (default as u32).min(qmenu.len() as u32 - 1), 0)
            };
            let res = ctrls.new_menu(
                cid_raw,
                name,
                qmenu.len() as u32,
                default_idx,
                qmenu,
                Some(ops),
            );
            if res.is_ok() && skipped != 0 {
                ctrls.set_step(cid_raw, skipped);
            }
            res
        }
        UvcCtrlType::Button => ctrls.new_button(cid_raw, name, Some(ops)),
    };
    if let Err(e) = res {
        log::warn!("uvc: skip {log_tag} {name} (0x{cid_raw:08x}): {e:?}");
    } else {
        let mut access = CtrlFlags::empty();
        if !readable {
            access.insert(CtrlFlags::WRITE_ONLY);
        }
        if !writable {
            access.insert(CtrlFlags::READ_ONLY);
        }
        ctrls.restrict_access(cid_raw, access);
    }
}

impl<H: UvcHandle, M: VbMemOps + 'static> UvcDevice<H, M> {
    pub(crate) fn register_controls(&self, units: &VcUnits) {
        let mut ctrls = self.ctrls.lock();
        let _ = ctrls.new_ctrl(CtrlConfig {
            id: (CtrlClass::User as u32) | 1,
            name: "User Controls",
            ctrl_type: CtrlType::CtrlClass,
            minimum: 0,
            maximum: 0,
            step: 0,
            default_value: 0,
            flags: CtrlFlags::empty(),
            qmenu: None,
            ops: None,
        });
        let _ = ctrls.new_ctrl(CtrlConfig {
            id: (CtrlClass::Camera as u32) | 1,
            name: "Camera Controls",
            ctrl_type: CtrlType::CtrlClass,
            minimum: 0,
            maximum: 0,
            step: 0,
            default_value: 0,
            flags: CtrlFlags::empty(),
            qmenu: None,
            ops: None,
        });

        let vc_iface = self.vc_iface_num;
        if let Some(unit_id) = units.processing_unit_id {
            for def in UVC_CONTROL_PU_DEFS {
                register_control(
                    &mut ctrls,
                    &self.handle,
                    vc_iface,
                    unit_id,
                    &units.processing_controls,
                    def,
                    self.uvc_version,
                    "PU",
                );
            }
        }
        if let Some(unit_id) = units.camera_terminal_id {
            for def in UVC_CONTROL_CT_DEFS {
                register_control(
                    &mut ctrls,
                    &self.handle,
                    vc_iface,
                    unit_id,
                    &units.camera_controls,
                    def,
                    self.uvc_version,
                    "CT",
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicU8, Ordering};

    use ax_media::interface::ctrl::{Control, QueryCtrl, Querymenu};
    use crab_usb::{err::USBError, usb_if::endpoint::TransferRequest};

    use super::*;

    #[test]
    fn control_signedness_preserves_high_unsigned_values() {
        assert_eq!(decode_uvc_value(&[0x00, 0x80], false), Some(32768));
        assert_eq!(decode_uvc_value(&[0x00, 0x80], true), Some(-32768));
        assert_eq!(
            decode_uvc_value(&[0x00, 0x00, 0x00, 0x80], false),
            Some(2147483648)
        );
    }

    struct ExposureHandle {
        current: AtomicU8,
        supported: u8,
        caps: u8,
    }

    impl UvcHandle for ExposureHandle {
        fn claim_interface(&self, _: u8, _: u8) -> Result<(), USBError> {
            Ok(())
        }

        fn release_interface(&self, _: u8) -> Result<(), USBError> {
            Ok(())
        }

        fn control_in(&self, setup: ControlSetup, data: &mut [u8]) -> Result<usize, USBError> {
            data[0] = match setup.request {
                crab_usb::usb_if::transfer::Request::Other(0x86) => self.caps,
                crab_usb::usb_if::transfer::Request::Other(0x84) => self.supported,
                crab_usb::usb_if::transfer::Request::Other(0x87) => 2,
                crab_usb::usb_if::transfer::Request::Other(0x81) => {
                    self.current.load(Ordering::SeqCst)
                }
                _ => return Err(USBError::NotSupported),
            };
            Ok(1)
        }

        fn control_out(&self, setup: ControlSetup, data: &[u8]) -> Result<(), USBError> {
            if !matches!(
                setup.request,
                crab_usb::usb_if::transfer::Request::ClearFeature
            ) {
                return Err(USBError::NotSupported);
            }
            self.current.store(data[0], Ordering::SeqCst);
            Ok(())
        }

        fn submit_endpoint_transfer(
            &self,
            _: u8,
            _: TransferRequest,
        ) -> Result<crate::IsoPending, USBError> {
            Err(USBError::NotSupported)
        }
    }

    #[test]
    fn exposure_auto_uses_v4l2_menu_order_and_uvc_mode_bits() {
        let id = CameraClassCtrl::ExposureAuto as u32;
        let handle = Arc::new(ExposureHandle {
            current: AtomicU8::new(2),
            supported: 0x0f,
            caps: 3,
        });
        let mut ctrls = ax_media::CtrlHandler::new();
        register_control(
            &mut ctrls,
            &handle,
            0,
            1,
            &[0x02],
            &UVC_CONTROL_CT_DEFS[0],
            0x110,
            "CT",
        );

        let mut query = QueryCtrl {
            id,
            ty: 0,
            name: [0; 32],
            minimum: 0,
            maximum: 0,
            step: 0,
            default_value: -1,
            flags: CtrlFlags::empty(),
            reserved: [0; 2],
        };
        ctrls.queryctrl(&mut query).unwrap();
        assert_eq!(query.default_value, 0);
        let mut initial = Control { id, value: -1 };
        ctrls.g_ctrl(&mut initial).unwrap();
        assert_eq!(initial.value, 0);

        for (index, name, raw) in [
            (0, "Auto Mode", 2),
            (1, "Manual Mode", 1),
            (2, "Shutter Priority Mode", 4),
            (3, "Aperture Priority Mode", 8),
        ] {
            let mut q = Querymenu {
                id,
                index,
                name: [0; 32],
                reserved: 0,
            };
            ctrls.querymenu(&mut q).unwrap();
            assert_eq!(&q.name[..name.len()], name.as_bytes());
            let mut set = Control {
                id,
                value: index as i32,
            };
            ctrls.s_ctrl(&mut set).unwrap();
            assert_eq!(handle.current.load(Ordering::SeqCst), raw);
            let mut get = Control { id, value: -1 };
            ctrls.g_ctrl(&mut get).unwrap();
            assert_eq!(get.value, index as i32);
        }

        let limited = Arc::new(ExposureHandle {
            current: AtomicU8::new(2),
            supported: 0x03,
            caps: 3,
        });
        let mut ctrls = ax_media::CtrlHandler::new();
        register_control(
            &mut ctrls,
            &limited,
            0,
            1,
            &[0x02],
            &UVC_CONTROL_CT_DEFS[0],
            0x110,
            "CT",
        );
        for index in 2..4 {
            let mut q = Querymenu {
                id,
                index,
                name: [0; 32],
                reserved: 0,
            };
            assert!(ctrls.querymenu(&mut q).is_err());
        }

        for (caps, flag) in [(1, CtrlFlags::READ_ONLY), (2, CtrlFlags::WRITE_ONLY)] {
            let handle = Arc::new(ExposureHandle {
                current: AtomicU8::new(2),
                supported: 0x0f,
                caps,
            });
            let mut ctrls = ax_media::CtrlHandler::new();
            register_control(
                &mut ctrls,
                &handle,
                0,
                1,
                &[0x02],
                &UVC_CONTROL_CT_DEFS[0],
                0x110,
                "CT",
            );
            let mut access = QueryCtrl { id, ..query };
            ctrls.queryctrl(&mut access).unwrap();
            assert!(access.flags.contains(flag));
            let mut control = Control { id, value: 1 };
            if flag == CtrlFlags::READ_ONLY {
                assert!(matches!(
                    ctrls.s_ctrl(&mut control),
                    Err(ax_media::V4l2Error::AccessDenied)
                ));
                assert_eq!(handle.current.load(Ordering::SeqCst), 2);
            } else {
                assert!(matches!(
                    ctrls.g_ctrl(&mut control),
                    Err(ax_media::V4l2Error::AccessDenied)
                ));
                ctrls.s_ctrl(&mut control).unwrap();
                assert_eq!(handle.current.load(Ordering::SeqCst), 1);
            }
        }
    }

    #[test]
    fn write_only_integer_control_remains_settable() {
        let id = UserClassCtrl::Brightness as u32;
        let handle = Arc::new(ExposureHandle {
            current: AtomicU8::new(0),
            supported: 0,
            caps: 2,
        });
        let mut ctrls = ax_media::CtrlHandler::new();
        register_control(
            &mut ctrls,
            &handle,
            0,
            2,
            &[1],
            &UVC_CONTROL_PU_DEFS[0],
            0x110,
            "PU",
        );
        let mut control = Control { id, value: 42 };
        assert!(matches!(
            ctrls.g_ctrl(&mut control),
            Err(ax_media::V4l2Error::AccessDenied)
        ));
        ctrls.s_ctrl(&mut control).unwrap();
        assert_eq!(handle.current.load(Ordering::SeqCst), 42);

        let wide_id = CameraClassCtrl::ExposureAbsolute as u32;
        register_control(
            &mut ctrls,
            &handle,
            0,
            1,
            &[0x08],
            &UVC_CONTROL_CT_DEFS[2],
            0x110,
            "CT",
        );
        let mut query = QueryCtrl {
            id: wide_id,
            ty: 0,
            name: [0; 32],
            minimum: 0,
            maximum: 0,
            step: 0,
            default_value: 0,
            flags: CtrlFlags::empty(),
            reserved: [0; 2],
        };
        ctrls.queryctrl(&mut query).unwrap();
        assert_eq!(query.maximum, i32::MAX);
    }

    struct PowerLineHandle {
        current: AtomicU8,
        default: u8,
        supports_auto: bool,
        fail_restore: bool,
        writes: AtomicU8,
    }

    impl UvcHandle for PowerLineHandle {
        fn claim_interface(&self, _: u8, _: u8) -> Result<(), USBError> {
            Ok(())
        }

        fn release_interface(&self, _: u8) -> Result<(), USBError> {
            Ok(())
        }

        fn control_in(&self, setup: ControlSetup, data: &mut [u8]) -> Result<usize, USBError> {
            data[0] = match setup.request {
                crab_usb::usb_if::transfer::Request::Other(0x86) => 3,
                crab_usb::usb_if::transfer::Request::Other(0x87) => self.default,
                crab_usb::usb_if::transfer::Request::Other(0x81) => {
                    self.current.load(Ordering::SeqCst)
                }
                _ => return Err(USBError::NotSupported),
            };
            Ok(1)
        }

        fn control_out(&self, setup: ControlSetup, data: &[u8]) -> Result<(), USBError> {
            if !matches!(
                setup.request,
                crab_usb::usb_if::transfer::Request::ClearFeature
            ) {
                return Err(USBError::NotSupported);
            }
            if data[0] == 3 && !self.supports_auto {
                return Err(USBError::NotSupported);
            }
            if self.fail_restore && self.writes.fetch_add(1, Ordering::SeqCst) == 2 {
                return Err(USBError::NotSupported);
            }
            self.current.store(data[0], Ordering::SeqCst);
            Ok(())
        }

        fn submit_endpoint_transfer(
            &self,
            _: u8,
            _: TransferRequest,
        ) -> Result<crate::IsoPending, USBError> {
            Err(USBError::NotSupported)
        }
    }

    #[test]
    fn power_line_frequency_auto_is_queryable_and_settable() {
        let id = UserClassCtrl::PowerLineFrequency as u32;
        let handle = Arc::new(PowerLineHandle {
            current: AtomicU8::new(3),
            default: 3,
            supports_auto: true,
            fail_restore: false,
            writes: AtomicU8::new(0),
        });
        let mut ctrls = ax_media::CtrlHandler::new();
        register_control(
            &mut ctrls,
            &handle,
            0,
            2,
            &[0, 0b100],
            &UVC_CONTROL_PU_DEFS[9],
            0x150,
            "PU",
        );

        let mut query = QueryCtrl {
            id,
            ty: 0,
            name: [0; 32],
            minimum: 0,
            maximum: 0,
            step: 0,
            default_value: -1,
            flags: CtrlFlags::empty(),
            reserved: [0; 2],
        };
        ctrls.queryctrl(&mut query).unwrap();
        assert_eq!(query.maximum, 3);
        assert_eq!(query.default_value, 3);
        let mut menu = Querymenu {
            id,
            index: 3,
            name: [0; 32],
            reserved: 0,
        };
        ctrls.querymenu(&mut menu).unwrap();
        assert_eq!(&menu.name[..4], b"Auto");
        let mut get = Control { id, value: -1 };
        ctrls.g_ctrl(&mut get).unwrap();
        assert_eq!(get.value, 3);
        let mut set = Control { id, value: 3 };
        ctrls.s_ctrl(&mut set).unwrap();
        assert_eq!(handle.current.load(Ordering::SeqCst), 3);

        for (version, supports_auto) in [(0x150, false), (0x110, true)] {
            let handle = Arc::new(PowerLineHandle {
                current: AtomicU8::new(1),
                default: 1,
                supports_auto,
                fail_restore: false,
                writes: AtomicU8::new(0),
            });
            let mut ctrls = ax_media::CtrlHandler::new();
            register_control(
                &mut ctrls,
                &handle,
                0,
                2,
                &[0, 0b100],
                &UVC_CONTROL_PU_DEFS[9],
                version,
                "PU",
            );
            assert_eq!(handle.current.load(Ordering::SeqCst), 1);
            let mut limited = QueryCtrl { id, ..query };
            ctrls.queryctrl(&mut limited).unwrap();
            assert_eq!(limited.maximum, 2);
            let mut auto = Querymenu { id, ..menu };
            assert!(ctrls.querymenu(&mut auto).is_err());
            let mut set = Control { id, value: 3 };
            assert!(ctrls.s_ctrl(&mut set).is_err());
            assert_eq!(handle.current.load(Ordering::SeqCst), 1);
        }

        let handle = Arc::new(PowerLineHandle {
            current: AtomicU8::new(1),
            default: 1,
            supports_auto: true,
            fail_restore: true,
            writes: AtomicU8::new(0),
        });
        let mut ctrls = ax_media::CtrlHandler::new();
        register_control(
            &mut ctrls,
            &handle,
            0,
            2,
            &[0, 0b100],
            &UVC_CONTROL_PU_DEFS[9],
            0x150,
            "PU",
        );
        let mut available = QueryCtrl { id, ..query };
        ctrls.queryctrl(&mut available).unwrap();
        assert_eq!(available.maximum, 3);
        let mut get = Control { id, value: -1 };
        ctrls.g_ctrl(&mut get).unwrap();
        assert_eq!(get.value, 3);
    }
}
