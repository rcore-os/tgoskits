//! 相机类控件（`V4L2_CTRL_CLASS_CAMERA = 0x009a0000`）。

use super::CtrlClass;

/// `V4L2_CTRL_CLASS_CAMERA` —— 相机类控件。
pub const CLASS_ID: u32 = CtrlClass::Camera as u32;

/// `V4L2_CID_CAMERA_CLASS = (V4L2_CTRL_CLASS_CAMERA | 1)`。
pub const CID_CLASS: u32 = CLASS_ID | 1;

/// `V4L2_CID_CAMERA_CLASS_BASE = (V4L2_CTRL_CLASS_CAMERA | 0x900) = 0x009a0900`。
pub const CID_BASE: u32 = CLASS_ID | 0x900;

/// V4L2 相机类控制 ID（`V4L2_CID_CAMERA_CLASS_BASE` + 偏移）。
///
/// 设计：`V4L2_CID_EXPOSURE_AUTO = (V4L2_CTRL_CLASS_CAMERA | 0x900) + 1`。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraClassCtrl {
    Class                = CID_CLASS,
    ExposureAuto         = CID_BASE + 1,
    ExposureAbsolute     = CID_BASE + 2,
    ExposureAutoPriority = CID_BASE + 3,
    PanRelative          = CID_BASE + 4,
    TiltRelative         = CID_BASE + 5,
    PanReset             = CID_BASE + 6,
    TiltReset            = CID_BASE + 7,
    PanAbsolute          = CID_BASE + 8,
    TiltAbsolute         = CID_BASE + 9,
    FocusAbsolute        = CID_BASE + 10,
    FocusRelative        = CID_BASE + 11,
    FocusAuto            = CID_BASE + 12,
    ZoomAbsolute         = CID_BASE + 13,
    ZoomRelative         = CID_BASE + 14,
    ZoomContinuous       = CID_BASE + 15,
    Privacy              = CID_BASE + 16,
    IrisAbsolute         = CID_BASE + 17,
    IrisRelative         = CID_BASE + 18,
    AutoExposureBias     = CID_BASE + 19,
    AutoNPresetWhiteBalance = CID_BASE + 20,
    WideDynamicRange     = CID_BASE + 21,
    ImageStabilization   = CID_BASE + 22,
    IsoSensitivity       = CID_BASE + 23,
    IsoSensitivityAuto   = CID_BASE + 24,
    ExposureMetering     = CID_BASE + 25,
    SceneMode            = CID_BASE + 26,
    ThreeALock           = CID_BASE + 27,
    AutoFocusStart       = CID_BASE + 28,
    AutoFocusStop        = CID_BASE + 29,
    AutoFocusStatus      = CID_BASE + 30,
    AutoFocusRange       = CID_BASE + 31,
    PanSpeed             = CID_BASE + 32,
    TiltSpeed            = CID_BASE + 33,
    CameraOrientation    = CID_BASE + 34,
    CameraSensorRotation = CID_BASE + 35,
    HdrSensorMode        = CID_BASE + 36,
}
