//! V4L2 控件类。
//!
//! 每个控件类一个独立模块，类 ID 由 [`CtrlClass`] 统一登记，
//! 各模块内再定义该类的基址（`CID_BASE`）与控件 ID：
//!
//! - [`user`]：旧式 ‘user’ 控件（`V4L2_CTRL_CLASS_USER`）。
//! - [`codec`]：有状态编解码控件（`V4L2_CTRL_CLASS_CODEC`）。
//! - [`camera`]：相机类控件（`V4L2_CTRL_CLASS_CAMERA`）。
//! - [`fm_tx`]：FM 调制器控件（`V4L2_CTRL_CLASS_FM_TX`）。
//! - [`flash`]：相机闪光灯控件（`V4L2_CTRL_CLASS_FLASH`）。
//! - [`jpeg`]：JPEG 压缩控件（`V4L2_CTRL_CLASS_JPEG`）。
//! - [`image_source`]：图像源控件（`V4L2_CTRL_CLASS_IMAGE_SOURCE`）。
//! - [`image_proc`]：图像处理控件（`V4L2_CTRL_CLASS_IMAGE_PROC`）。
//! - [`dv`]：数字视频控件（`V4L2_CTRL_CLASS_DV`）。
//! - [`fm_rx`]：FM 接收器控件（`V4L2_CTRL_CLASS_FM_RX`）。
//! - [`rf_tuner`]：RF 调谐器控件（`V4L2_CTRL_CLASS_RF_TUNER`）。
//! - [`detect`]：检测控件（`V4L2_CTRL_CLASS_DETECT`）。
//! - [`codec_stateless`]：无状态编解码控件（`V4L2_CTRL_CLASS_CODEC_STATELESS`）。
//! - [`colorimetry`]：颜色计量控件（`V4L2_CTRL_CLASS_COLORIMETRY`）。

pub mod camera;
pub mod codec;
pub mod codec_stateless;
pub mod colorimetry;
pub mod detect;
pub mod dv;
pub mod flash;
pub mod fm_rx;
pub mod fm_tx;
pub mod image_proc;
pub mod image_source;
pub mod jpeg;
pub mod rf_tuner;
pub mod user;

pub use camera::CameraClassCtrl;
pub use codec::CodecClassCtrl;
pub use codec_stateless::CodecStatelessClassCtrl;
pub use colorimetry::ColorimetryClassCtrl;
pub use detect::DetectClassCtrl;
pub use dv::DvClassCtrl;
pub use flash::FlashClassCtrl;
pub use fm_rx::FmRxClassCtrl;
pub use fm_tx::FmTxClassCtrl;
pub use image_proc::ImageProcClassCtrl;
pub use image_source::ImageSourceClassCtrl;
pub use jpeg::JpegClassCtrl;
pub use rf_tuner::RfTunerClassCtrl;
pub use user::UserClassCtrl;

/// V4L2 控制类 ID（CID 的高 16 位）。
///
/// 来自 `linux/v4l2-controls.h`。控制按功能类组织；
/// 类别编码在控制 ID 的 31:16 位。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CtrlClass {
    User           = 0x00980000, // 旧式 ‘user’ 控件
    Codec          = 0x00990000, // 有状态编解码控件
    Camera         = 0x009a0000, // 相机类控件
    FmTx           = 0x009b0000, // FM 调制器控件
    Flash          = 0x009c0000, // 相机闪光灯控件
    Jpeg           = 0x009d0000, // JPEG 压缩控件
    ImageSource    = 0x009e0000, // 图像源控件
    ImageProc      = 0x009f0000, // 图像处理控件
    Dv             = 0x00a00000, // 数字视频控件
    FmRx           = 0x00a10000, // FM 接收器控件
    RfTuner        = 0x00a20000, // RF 调谐器控件
    Detect         = 0x00a30000, // 检测控件
    CodecStateless = 0x00a40000, // 无状态编解码控件
    Colorimetry    = 0x00a50000, // 颜色计量控件
}
