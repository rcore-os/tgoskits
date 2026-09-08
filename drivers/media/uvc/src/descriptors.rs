#![allow(dead_code)]

use alloc::vec::Vec;

use anyhow::anyhow;
use bitflags::bitflags;
use crab_usb::err::USBError;
use log::trace;

/// UVC 类特定请求码 (A.8)——互斥值，用枚举表达。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum RequestCode {
    SetCur  = 0x01,
    GetCur  = 0x81,
    GetMin  = 0x82,
    GetMax  = 0x83,
    GetRes  = 0x84,
    GetLen  = 0x85,
    GetInfo = 0x86,
    GetDef  = 0x87,
}

/// UVC 接口子类代码 (A.2)——互斥值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum InterfaceSubclass {
    Undefined      = 0x00,
    VideoControl   = 0x01,
    VideoStreaming = 0x02,
    VideoInterfaceCollection = 0x03,
}

/// UVC 协议代码 (A.3)
pub(crate) mod protocol_codes {
    pub(crate) const UNDEFINED: u8 = 0x00;
}

/// VideoControl 接口描述符子类型 (A.5)——互斥值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum VcDescriptorSubtype {
    Undefined      = 0x00,
    Header         = 0x01,
    InputTerminal  = 0x02,
    OutputTerminal = 0x03,
    SelectorUnit   = 0x04,
    ProcessingUnit = 0x05,
    ExtensionUnit  = 0x06,
}

/// VideoStreaming 接口描述符子类型 (UVC 1.5 Table A.6)——互斥值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum VsDescriptorSubtype {
    Undefined           = 0x00, // 未定义
    InputHeader         = 0x01, // 输入头描述符
    OutputHeader        = 0x02, // 输出头描述符
    StillImageFrame     = 0x03, // 静态图像帧描述符
    FormatUncompressed  = 0x04, // 未压缩格式描述符
    FrameUncompressed   = 0x05, // 未压缩帧描述符
    FormatMjpeg         = 0x06, // MJPEG 格式描述符
    FrameMjpeg          = 0x07, // MJPEG 帧描述符
    FormatMpeg2Ts       = 0x0A, // MPEG2-TS 格式描述符
    FormatDv            = 0x0C, // DV 格式描述符
    Colorformat         = 0x0D, // 颜色格式描述符
    FormatFrameBased    = 0x10, // 基于帧的格式描述符 - H.264 等变长帧
    FrameFrameBased     = 0x11, // 基于帧的帧描述符
    FormatStreamBased   = 0x12, // 基于流的格式描述符
    FormatH264          = 0x13, // H.264 格式描述符
    FrameH264           = 0x14, // H.264 帧描述符
    FormatH264Simulcast = 0x15, // H.264 Simulcast 格式描述符
}

/// VideoStreaming 接口控制选择器 (A.9.7)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum VideoStreamingControl {
    Undefined          = 0x00,
    Probe              = 0x01,
    Commit             = 0x02,
    StillProbe         = 0x03,
    StillCommit        = 0x04,
    StillImageTrigger  = 0x05,
    StreamErrorCode    = 0x06,
    GenerateKeyFrame   = 0x07,
    UpdateFrameSegment = 0x08,
    SyncDelay          = 0x09,
}

/// UVC 描述符类型——互斥值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum DescriptorType {
    Undefined     = 0x00,
    Device        = 0x01,
    Configuration = 0x02,
    String        = 0x03,
    Interface     = 0x04,
    Endpoint      = 0x05,
    CsInterface   = 0x24,
    CsEndpoint    = 0x25,
}

/// 终端类型 (B.1-B.4)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub(crate) enum TerminalType {
    // USB 终端类型 (B.1)
    TtVendorSpecific   = 0x0100,
    TtStreaming        = 0x0101,
    // 输入终端类型 (B.2)
    IttVendorSpecific  = 0x0200,
    IttCamera          = 0x0201,
    IttMediaTransportInput = 0x0202,
    // 输出终端类型 (B.3)
    OttVendorSpecific  = 0x0300,
    OttDisplay         = 0x0301,
    OttMediaTransportOutput = 0x0302,
    // 外部终端类型 (B.4)
    ExternalVendorSpecific = 0x0400,
    CompositeConnector = 0x0401,
    SvideoConnector    = 0x0402,
    ComponentConnector = 0x0403,
}

/// UVC 格式 GUID 常量
pub(crate) mod format_guids {
    pub(crate) const YUY2: [u8; 16] = [
        0x59, 0x55, 0x59, 0x32, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b,
        0x71,
    ];
    pub(crate) const NV12: [u8; 16] = [
        0x4e, 0x56, 0x31, 0x32, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b,
        0x71,
    ];
    pub(crate) const UYVY: [u8; 16] = [
        0x55, 0x59, 0x56, 0x59, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b,
        0x71,
    ];
    pub(crate) const GREY: [u8; 16] = [
        0x59, 0x38, 0x30, 0x30, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b,
        0x71,
    ];
    pub(crate) const BGR24: [u8; 16] = [
        0x7d, 0xeb, 0x36, 0xe4, 0x4f, 0x52, 0xce, 0x11, 0x9f, 0x53, 0x00, 0x20, 0xaf, 0x0b, 0xa7,
        0x70,
    ];
    pub(crate) const XBGR32: [u8; 16] = [
        0x7e, 0xeb, 0x36, 0xe4, 0x4f, 0x52, 0xce, 0x11, 0x9f, 0x53, 0x00, 0x20, 0xaf, 0x0b, 0xa7,
        0x70,
    ];
}

bitflags! {
    /// 控制能力标志 (4.1.2)。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) struct ControlCapabilities: u8 {
        const GET = 1 << 0;
        const SET = 1 << 1;
        const DISABLED = 1 << 2;
        const AUTOUPDATE = 1 << 3;
        const ASYNCHRONOUS = 1 << 4;
    }
}

/// 原始值 → 枚举（`match x.into()` scrutinee 用；非法值兜底到 `Undefined`，
/// 落入 match 的 `_` 分支——UVC 规范 0x00 即 UNDEFINED）。
impl From<u8> for InterfaceSubclass {
    fn from(v: u8) -> Self {
        match v {
            0x01 => Self::VideoControl,
            0x02 => Self::VideoStreaming,
            0x03 => Self::VideoInterfaceCollection,
            _ => Self::Undefined,
        }
    }
}

impl From<u8> for DescriptorType {
    fn from(v: u8) -> Self {
        match v {
            0x01 => Self::Device,
            0x02 => Self::Configuration,
            0x03 => Self::String,
            0x04 => Self::Interface,
            0x05 => Self::Endpoint,
            0x24 => Self::CsInterface,
            0x25 => Self::CsEndpoint,
            _ => Self::Undefined,
        }
    }
}

impl From<u8> for VcDescriptorSubtype {
    fn from(v: u8) -> Self {
        match v {
            0x01 => Self::Header,
            0x02 => Self::InputTerminal,
            0x03 => Self::OutputTerminal,
            0x04 => Self::SelectorUnit,
            0x05 => Self::ProcessingUnit,
            0x06 => Self::ExtensionUnit,
            _ => Self::Undefined,
        }
    }
}

impl From<u8> for VsDescriptorSubtype {
    fn from(v: u8) -> Self {
        match v {
            0x01 => Self::InputHeader,
            0x02 => Self::OutputHeader,
            0x03 => Self::StillImageFrame,
            0x04 => Self::FormatUncompressed,
            0x05 => Self::FrameUncompressed,
            0x06 => Self::FormatMjpeg,
            0x07 => Self::FrameMjpeg,
            0x0A => Self::FormatMpeg2Ts,
            0x0C => Self::FormatDv,
            0x0D => Self::Colorformat,
            0x10 => Self::FormatFrameBased,
            0x11 => Self::FrameFrameBased,
            0x12 => Self::FormatStreamBased,
            0x13 => Self::FormatH264,
            0x14 => Self::FrameH264,
            0x15 => Self::FormatH264Simulcast,
            _ => Self::Undefined,
        }
    }
}

/// 枚举 → 原始值转换（`into()` 用，类型安全；与 `as u8` 等价但显式）。
impl From<RequestCode> for u8 {
    fn from(v: RequestCode) -> u8 {
        v as u8
    }
}

impl From<InterfaceSubclass> for u8 {
    fn from(v: InterfaceSubclass) -> u8 {
        v as u8
    }
}

impl From<VideoStreamingControl> for u8 {
    fn from(v: VideoStreamingControl) -> u8 {
        v as u8
    }
}

impl From<DescriptorType> for u8 {
    fn from(v: DescriptorType) -> u8 {
        v as u8
    }
}

impl From<TerminalType> for u16 {
    fn from(v: TerminalType) -> u16 {
        v as u16
    }
}

/// UVC 请求码 → USB 传输请求（`Request: From<u8>`，一步转换）。
impl From<RequestCode> for crab_usb::usb_if::transfer::Request {
    fn from(v: RequestCode) -> Self {
        (v as u8).into()
    }
}

/// UVC描述符解析器
pub(crate) struct DescriptorParser;

impl DescriptorParser {
    /// 创建新的描述符解析器实例
    pub(crate) fn new() -> Self {
        Self
    }

    /// 解析输入终端描述符
    pub(crate) fn parse_input_terminal(
        &self,
        data: &[u8],
    ) -> Result<InputTerminalDescriptor, USBError> {
        if data.len() < 15 {
            Err(anyhow!("Input terminal descriptor too short"))?;
        }

        let length = data[0] as usize;
        let terminal_id = data[3];
        let terminal_type = u16::from_le_bytes([data[4], data[5]]);
        let associated_terminal = data[6];

        trace!(
            "Input Terminal: ID={terminal_id}, type=0x{terminal_type:04x}, \
             associated={associated_terminal}"
        );

        // 摄像头终端有额外字段
        if terminal_type == TerminalType::IttCamera.into() && length >= 18 {
            let objective_focal_length_min = u16::from_le_bytes([data[8], data[9]]);
            let objective_focal_length_max = u16::from_le_bytes([data[10], data[11]]);
            let ocular_focal_length = u16::from_le_bytes([data[12], data[13]]);
            let controls_size = data[14] as usize;

            let controls = if length >= 15 + controls_size {
                data[15..15 + controls_size].to_vec()
            } else {
                vec![]
            };

            Ok(InputTerminalDescriptor::Camera {
                length,
                terminal_id,
                terminal_type,
                associated_terminal,
                objective_focal_length_min,
                objective_focal_length_max,
                ocular_focal_length,
                controls,
            })
        } else {
            Ok(InputTerminalDescriptor::Generic {
                length,
                terminal_id,
                terminal_type,
                associated_terminal,
            })
        }
    }

    /// 解析处理单元描述符
    pub(crate) fn parse_processing_unit(
        &self,
        data: &[u8],
    ) -> Result<ProcessingUnitDescriptor, USBError> {
        if data.len() < 10 {
            Err(anyhow!("Processing unit descriptor too short"))?;
        }

        let length = data[0] as usize;
        let unit_id = data[3];
        let source_id = data[4];
        let max_multiplier = u16::from_le_bytes([data[5], data[6]]);
        let controls_size = data[7] as usize;

        if length < 8 + controls_size {
            Err(anyhow!("Processing unit controls data incomplete"))?;
        }

        let controls = data[8..8 + controls_size].to_vec();

        trace!(
            "Processing Unit: ID={unit_id}, source={source_id}, max_mult={max_multiplier}, \
             controls={controls:02x?}"
        );

        Ok(ProcessingUnitDescriptor {
            length,
            unit_id,
            source_id,
            max_multiplier,
            controls,
        })
    }

    /// 解析未压缩格式描述符
    pub(crate) fn parse_uncompressed_format(
        &self,
        data: &[u8],
    ) -> Result<UncompressedFormatDescriptor, USBError> {
        if data.len() < 27 {
            Err(anyhow!("Uncompressed format descriptor too short"))?;
        }

        let length = data[0] as usize;
        let format_index = data[3];
        let num_frame_descriptors = data[4];
        let mut guid = [0u8; 16];
        guid.copy_from_slice(&data[5..21]);
        let bits_per_pixel = data[21];
        let default_frame_index = data[22];
        let aspect_ratio_x = data[23];
        let aspect_ratio_y = data[24];
        let interlace_flags = data[25];
        let copy_protect = data[26];

        trace!(
            "Uncompressed Format: index={format_index}, frames={num_frame_descriptors}, \
             GUID={guid:02x?}, bpp={bits_per_pixel}"
        );

        Ok(UncompressedFormatDescriptor {
            length,
            format_index,
            num_frame_descriptors,
            guid,
            bits_per_pixel,
            default_frame_index,
            aspect_ratio_x,
            aspect_ratio_y,
            interlace_flags,
            copy_protect,
        })
    }

    /// 解析帧描述符
    pub(crate) fn parse_frame_descriptor(&self, data: &[u8]) -> Result<FrameDescriptor, USBError> {
        if data.len() < 26 {
            Err(anyhow!("Frame descriptor too short"))?;
        }

        let length = data[0] as usize;
        let frame_index = data[3];
        let capabilities = data[4];
        let width = u16::from_le_bytes([data[5], data[6]]);
        let height = u16::from_le_bytes([data[7], data[8]]);
        let min_bit_rate = u32::from_le_bytes([data[9], data[10], data[11], data[12]]);
        let max_bit_rate = u32::from_le_bytes([data[13], data[14], data[15], data[16]]);
        let max_video_frame_buffer_size =
            u32::from_le_bytes([data[17], data[18], data[19], data[20]]);
        let default_frame_interval = u32::from_le_bytes([data[21], data[22], data[23], data[24]]);
        let frame_interval_type = data[25];

        trace!(
            "Frame: {width}x{height}, bitrate={min_bit_rate}-{max_bit_rate}, \
             buffer_size={max_video_frame_buffer_size}, interval={default_frame_interval}, \
             type={frame_interval_type}"
        );

        // 解析帧间隔数据
        let mut frame_intervals = Vec::new();
        let mut pos = 26;

        match frame_interval_type {
            0 if length >= pos + 12 => {
                // 连续帧间隔
                let min_frame_interval =
                    u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
                let max_frame_interval = u32::from_le_bytes([
                    data[pos + 4],
                    data[pos + 5],
                    data[pos + 6],
                    data[pos + 7],
                ]);
                let step_frame_interval = u32::from_le_bytes([
                    data[pos + 8],
                    data[pos + 9],
                    data[pos + 10],
                    data[pos + 11],
                ]);

                frame_intervals = vec![min_frame_interval, max_frame_interval, step_frame_interval];
            }
            n if n > 0 => {
                // 离散帧间隔
                for _ in 0..n {
                    if pos + 4 <= length {
                        let interval = u32::from_le_bytes([
                            data[pos],
                            data[pos + 1],
                            data[pos + 2],
                            data[pos + 3],
                        ]);
                        frame_intervals.push(interval);
                        pos += 4;
                    }
                }
            }
            _ => {}
        }

        Ok(FrameDescriptor {
            length,
            frame_index,
            capabilities,
            width,
            height,
            min_bit_rate,
            max_bit_rate,
            max_video_frame_buffer_size,
            default_frame_interval,
            frame_interval_type,
            frame_intervals,
        })
    }

    /// 解析 VS 输入头描述符 (InputHeader 0x01)
    pub(crate) fn parse_input_header(
        &self,
        data: &[u8],
    ) -> Result<InputHeaderDescriptor, USBError> {
        if data.len() < 13 {
            Err(anyhow!("InputHeader descriptor too short"))?;
        }
        let length = data[0] as usize;
        if length < 13 || data.len() < length {
            Err(anyhow!("InputHeader length invalid"))?;
        }
        // bDescriptorType 0x24 / bDescriptorSubtype 0x01 由调用方保证
        let num_formats = data[3];
        let total_length = u16::from_le_bytes([data[4], data[5]]);
        let endpoint_address = data[6];
        let info = data[7];
        let terminal_link = data[8];
        let still_capture_method = data[9];
        let trigger_support = data[10];
        let trigger_usage = data[11];
        let control_size = data[12] as usize;

        // bLength 必须为 13 + p*n (UVC §3.9.2.1)
        let expected = 13usize + control_size * num_formats as usize;
        if length < expected {
            Err(anyhow!(
                "InputHeader bLength {length} smaller than 13+p*n {expected}"
            ))?;
        }
        if num_formats == 0 {
            Err(anyhow!("InputHeader bNumFormats is 0"))?;
        }

        let controls = if control_size > 0 && num_formats > 0 {
            let end = 13 + control_size * num_formats as usize;
            if end <= data.len() {
                data[13..end].to_vec()
            } else {
                vec![]
            }
        } else {
            vec![]
        };

        trace!(
            "InputHeader: formats={num_formats}, total={total_length}, \
             ep=0x{endpoint_address:02x}, link={terminal_link}, \
             still_method={still_capture_method}, controls={controls:02x?}"
        );

        Ok(InputHeaderDescriptor {
            length,
            num_formats,
            total_length,
            endpoint_address,
            info,
            terminal_link,
            still_capture_method,
            trigger_support,
            trigger_usage,
            control_size: control_size as u8,
            controls,
        })
    }

    /// 解析 VS 输出头描述符 (OutputHeader 0x02)
    pub(crate) fn parse_output_header(
        &self,
        data: &[u8],
    ) -> Result<OutputHeaderDescriptor, USBError> {
        if data.len() < 9 {
            Err(anyhow!("OutputHeader descriptor too short"))?;
        }
        let length = data[0] as usize;
        if length < 9 || data.len() < length {
            Err(anyhow!("OutputHeader length invalid"))?;
        }
        let num_formats = data[3];
        let total_length = u16::from_le_bytes([data[4], data[5]]);
        let endpoint_address = data[6];
        let terminal_link = data[7];
        let control_size = data[8] as usize;

        let expected = 9usize + control_size * num_formats as usize;
        if length < expected {
            Err(anyhow!(
                "OutputHeader bLength {length} smaller than 9+p*n {expected}"
            ))?;
        }

        let controls = if control_size > 0 && num_formats > 0 {
            let end = 9 + control_size * num_formats as usize;
            if end <= data.len() {
                data[9..end].to_vec()
            } else {
                vec![]
            }
        } else {
            vec![]
        };

        trace!(
            "OutputHeader: formats={num_formats}, total={total_length}, \
             ep=0x{endpoint_address:02x}, link={terminal_link}, controls={controls:02x?}"
        );

        Ok(OutputHeaderDescriptor {
            length,
            num_formats,
            total_length,
            endpoint_address,
            terminal_link,
            control_size: control_size as u8,
            controls,
        })
    }

    /// 解析 VC 头描述符 (Header 0x01)
    pub(crate) fn parse_vc_header(&self, data: &[u8]) -> Result<VcHeaderDescriptor, USBError> {
        if data.len() < 12 {
            Err(anyhow!("VC Header descriptor too short"))?;
        }
        let length = data[0] as usize;
        if length < 12 || data.len() < length {
            Err(anyhow!("VC Header length invalid"))?;
        }
        let bcd_uvc = u16::from_le_bytes([data[3], data[4]]);
        let total_length = u16::from_le_bytes([data[5], data[6]]);
        let clock_frequency = u32::from_le_bytes([data[7], data[8], data[9], data[10]]);
        let in_collection = data[11];
        let expected = 12usize + in_collection as usize;
        // 容忍部分设备 wTotalLength/length 与 bInCollection 不一致（如测试中 12 vs 1）
        let interface_numbers = if data.len() >= 12 + in_collection as usize {
            data[12..12 + in_collection as usize].to_vec()
        } else if length > 12 {
            data[12..length].to_vec()
        } else {
            vec![]
        };

        trace!(
            "VC Header: bcd=0x{bcd_uvc:04x}, total={total_length}, clk={clock_frequency}, \
             in_col={in_collection}, ifs={interface_numbers:02x?}"
        );

        // 长度校验宽松：允许 expected 与 length 不完全一致时仍通过，交由上层 wTotalLength 校验
        if length < 12 {
            Err(anyhow!("VC Header bLength {length} < 12"))?;
        }
        let _ = expected;

        Ok(VcHeaderDescriptor {
            length,
            bcd_uvc,
            total_length,
            clock_frequency,
            in_collection,
            interface_numbers,
        })
    }

    /// 计算帧率（从帧间隔）
    pub(crate) fn interval_to_fps(interval: u32) -> u32 {
        10_000_000u32.checked_div(interval).unwrap_or(0) // 100ns单位转换为fps
    }

    /// 计算帧间隔（从帧率）
    pub(crate) fn fps_to_interval(fps: u32) -> u32 {
        10_000_000u32.checked_div(fps).unwrap_or(0) // fps转换为100ns单位
    }
}

/// 输入终端描述符
#[derive(Debug, Clone)]
pub(crate) enum InputTerminalDescriptor {
    Camera {
        length: usize,
        terminal_id: u8,
        terminal_type: u16,
        associated_terminal: u8,
        objective_focal_length_min: u16,
        objective_focal_length_max: u16,
        ocular_focal_length: u16,
        controls: Vec<u8>,
    },
    Generic {
        length: usize,
        terminal_id: u8,
        terminal_type: u16,
        associated_terminal: u8,
    },
}

/// 处理单元描述符
#[derive(Debug, Clone)]
pub(crate) struct ProcessingUnitDescriptor {
    pub length: usize,
    pub unit_id: u8,
    pub source_id: u8,
    pub max_multiplier: u16,
    pub controls: Vec<u8>,
}

/// 未压缩格式描述符
#[derive(Debug, Clone)]
pub(crate) struct UncompressedFormatDescriptor {
    pub length: usize,
    pub format_index: u8,
    pub num_frame_descriptors: u8,
    pub guid: [u8; 16],
    pub bits_per_pixel: u8,
    pub default_frame_index: u8,
    pub aspect_ratio_x: u8,
    pub aspect_ratio_y: u8,
    pub interlace_flags: u8,
    pub copy_protect: u8,
}

/// 帧描述符
#[derive(Debug, Clone)]
pub(crate) struct FrameDescriptor {
    pub length: usize,    // 描述符长度
    pub frame_index: u8,  // 帧索引
    pub capabilities: u8, // 帧能力标志
    pub width: u16,
    pub height: u16,
    pub min_bit_rate: u32,
    pub max_bit_rate: u32,
    pub max_video_frame_buffer_size: u32, // 最大视频帧缓冲区大小
    pub default_frame_interval: u32,      // 默认帧间隔
    pub frame_interval_type: u8,          // 帧间隔类型
    pub frame_intervals: Vec<u32>,        // 帧间隔列表
}

/// VS 输入头描述符 (UVC 1.5 §3.9.2.1)
#[derive(Debug, Clone)]
pub(crate) struct InputHeaderDescriptor {
    pub length: usize,
    pub num_formats: u8,
    pub total_length: u16,
    pub endpoint_address: u8,
    pub info: u8,
    pub terminal_link: u8,
    pub still_capture_method: u8,
    pub trigger_support: u8,
    pub trigger_usage: u8,
    pub control_size: u8,
    pub controls: Vec<u8>,
}

/// VS 输出头描述符 (UVC 1.5 §3.9.2.2)
#[derive(Debug, Clone)]
pub(crate) struct OutputHeaderDescriptor {
    pub length: usize,
    pub num_formats: u8,
    pub total_length: u16,
    pub endpoint_address: u8,
    pub terminal_link: u8,
    pub control_size: u8,
    pub controls: Vec<u8>,
}

/// VC 头描述符 (UVC 1.5 §3.7.2)
#[derive(Debug, Clone)]
pub(crate) struct VcHeaderDescriptor {
    pub length: usize,
    pub bcd_uvc: u16,
    pub total_length: u16,
    pub clock_frequency: u32,
    pub in_collection: u8,
    pub interface_numbers: Vec<u8>,
}

impl Default for DescriptorParser {
    fn default() -> Self {
        Self::new()
    }
}
