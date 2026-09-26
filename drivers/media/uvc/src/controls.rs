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
const POWER_LINE_FREQ_MENU: &[&str] = &["Disabled", "50 Hz", "60 Hz"];

/// Exposure auto menu.
const EXPOSURE_AUTO_MENU: &[&str] = &[
    "Manual Mode",
    "Aperture Priority Mode",
    "Shutter Priority Mode",
    "Auto Mode",
];

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
        ty: UvcCtrlType::Menu(POWER_LINE_FREQ_MENU),
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

#[cfg(test)]
mod tests {
    use super::decode_uvc_value;

    #[test]
    fn control_signedness_preserves_high_unsigned_values() {
        assert_eq!(decode_uvc_value(&[0x00, 0x80], false), Some(32768));
        assert_eq!(decode_uvc_value(&[0x00, 0x80], true), Some(-32768));
        assert_eq!(
            decode_uvc_value(&[0x00, 0x00, 0x00, 0x80], false),
            Some(2147483648)
        );
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
    if !caps.contains(ControlCapabilities::GET) {
        log::debug!("uvc: {log_tag} {name} no GET support info={info_byte:#x}");
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
            Ok(raw.trailing_zeros() as i64)
        } else {
            Ok(raw)
        }
    });

    let h = handle.clone();
    let set_fn: CtrlSetFn = Box::new(move |v| {
        let orig_v = v;
        let v = if cid_raw == CameraClassCtrl::ExposureAuto as u32 {
            1i64 << v
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
            let Some(min) = read(RequestCode::GetMin) else {
                return;
            };
            let Some(max) = read(RequestCode::GetMax) else {
                return;
            };
            let step = read(RequestCode::GetRes).unwrap_or(1).max(1);
            let default = read(RequestCode::GetDef).unwrap_or(min);
            ctrls.new_int(cid_raw, name, min, max, step, default, Some(ops))
        }
        UvcCtrlType::Boolean => {
            let default = read(RequestCode::GetDef).unwrap_or(0);
            ctrls.new_bool(cid_raw, name, default != 0, Some(ops))
        }
        UvcCtrlType::Menu(qmenu) => {
            let default = read(RequestCode::GetDef).unwrap_or(0);
            let default_idx = if cid_raw == CameraClassCtrl::ExposureAuto as u32 {
                (default.trailing_zeros() as i64).clamp(0, qmenu.len() as i64 - 1) as u32
            } else {
                (default as u32).min(qmenu.len() as u32 - 1)
            };
            let res = ctrls.new_menu(
                cid_raw,
                name,
                qmenu.len() as u32,
                default_idx,
                qmenu,
                Some(ops),
            );
            if cid_raw == CameraClassCtrl::ExposureAuto as u32 {
                ctrls.set_step(cid_raw, (1u64 << 0) | (1u64 << 2));
            }
            res
        }
        UvcCtrlType::Button => ctrls.new_button(cid_raw, name, Some(ops)),
    };
    if let Err(e) = res {
        log::warn!("uvc: skip {log_tag} {name} (0x{cid_raw:08x}): {e:?}");
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
                    "CT",
                );
            }
        }
    }
}
