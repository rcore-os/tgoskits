#![no_std]

#[macro_use]
extern crate alloc;

use alloc::{
    string::{String, ToString},
    vec::Vec,
};
use anyhow::anyhow;
use crab_usb::{Device, DeviceInfo, err::USBError};
use log::*;
use usb_if::descriptor::EndpointType;
use usb_if::{
    descriptor::Class,
    host::ControlSetup,
    transfer::{Direction, Recipient, Request, RequestType},
};

// 导入描述符解析模块
pub mod descriptors;
pub use descriptors::*;

pub mod stream;
// 帧解析模块（参考 libuvc 的包头解析与帧组装）
pub mod frame;

use crate::stream::VideoStream;

// 保持向后兼容的常量别名
pub mod uvc_requests {
    pub use crate::descriptors::request_codes::*;
}

pub mod pu_controls {
    pub use crate::descriptors::processing_unit_controls::*;
    // 添加原有的常量别名
    pub const PU_BRIGHTNESS_CONTROL: u8 = super::descriptors::processing_unit_controls::BRIGHTNESS;
    pub const PU_CONTRAST_CONTROL: u8 = super::descriptors::processing_unit_controls::CONTRAST;
    pub const PU_HUE_CONTROL: u8 = super::descriptors::processing_unit_controls::HUE;
    pub const PU_SATURATION_CONTROL: u8 = super::descriptors::processing_unit_controls::SATURATION;
    pub const PU_SHARPNESS_CONTROL: u8 = super::descriptors::processing_unit_controls::SHARPNESS;
    pub const PU_GAMMA_CONTROL: u8 = super::descriptors::processing_unit_controls::GAMMA;
    pub const PU_WHITE_BALANCE_TEMPERATURE_CONTROL: u8 =
        super::descriptors::processing_unit_controls::WHITE_BALANCE_TEMPERATURE;
    pub const PU_WHITE_BALANCE_COMPONENT_CONTROL: u8 =
        super::descriptors::processing_unit_controls::WHITE_BALANCE_COMPONENT;
    pub const PU_BACKLIGHT_COMPENSATION_CONTROL: u8 =
        super::descriptors::processing_unit_controls::BACKLIGHT_COMPENSATION;
    pub const PU_GAIN_CONTROL: u8 = super::descriptors::processing_unit_controls::GAIN;
    pub const PU_POWER_LINE_FREQUENCY_CONTROL: u8 =
        super::descriptors::processing_unit_controls::POWER_LINE_FREQUENCY;
    pub const PU_HUE_AUTO_CONTROL: u8 = super::descriptors::processing_unit_controls::HUE_AUTO;
    pub const PU_WHITE_BALANCE_TEMPERATURE_AUTO_CONTROL: u8 =
        super::descriptors::processing_unit_controls::WHITE_BALANCE_TEMPERATURE_AUTO;
    pub const PU_WHITE_BALANCE_COMPONENT_AUTO_CONTROL: u8 =
        super::descriptors::processing_unit_controls::WHITE_BALANCE_COMPONENT_AUTO;
}

pub mod vs_controls {
    pub use crate::descriptors::video_streaming_controls::*;
    // 添加原有的常量别名
    pub const VS_PROBE_CONTROL: u8 = super::descriptors::video_streaming_controls::PROBE;
    pub const VS_COMMIT_CONTROL: u8 = super::descriptors::video_streaming_controls::COMMIT;
    pub const VS_STILL_PROBE_CONTROL: u8 =
        super::descriptors::video_streaming_controls::STILL_PROBE;
    pub const VS_STILL_COMMIT_CONTROL: u8 =
        super::descriptors::video_streaming_controls::STILL_COMMIT;
}

pub mod terminal_types {
    pub use crate::descriptors::terminal_types::*;
}

pub mod uvc_descriptor_types {
    pub use crate::descriptors::descriptor_types::*;
}

pub mod uvc_interface_subtypes {
    // 保持原有命名
    pub const VC_DESCRIPTOR_UNDEFINED: u8 = super::descriptors::vc_descriptor_subtypes::UNDEFINED;
    pub const VC_HEADER: u8 = super::descriptors::vc_descriptor_subtypes::HEADER;
    pub const VC_INPUT_TERMINAL: u8 = super::descriptors::vc_descriptor_subtypes::INPUT_TERMINAL;
    pub const VC_OUTPUT_TERMINAL: u8 = super::descriptors::vc_descriptor_subtypes::OUTPUT_TERMINAL;
    pub const VC_SELECTOR_UNIT: u8 = super::descriptors::vc_descriptor_subtypes::SELECTOR_UNIT;
    pub const VC_PROCESSING_UNIT: u8 = super::descriptors::vc_descriptor_subtypes::PROCESSING_UNIT;
    pub const VC_EXTENSION_UNIT: u8 = super::descriptors::vc_descriptor_subtypes::EXTENSION_UNIT;

    pub const VS_UNDEFINED: u8 = super::descriptors::vs_descriptor_subtypes::UNDEFINED;
    pub const VS_INPUT_HEADER: u8 = super::descriptors::vs_descriptor_subtypes::INPUT_HEADER;
    pub const VS_OUTPUT_HEADER: u8 = super::descriptors::vs_descriptor_subtypes::OUTPUT_HEADER;
    pub const VS_STILL_IMAGE_FRAME: u8 =
        super::descriptors::vs_descriptor_subtypes::STILL_IMAGE_FRAME;
    pub const VS_FORMAT_UNCOMPRESSED: u8 =
        super::descriptors::vs_descriptor_subtypes::FORMAT_UNCOMPRESSED;
    pub const VS_FRAME_UNCOMPRESSED: u8 =
        super::descriptors::vs_descriptor_subtypes::FRAME_UNCOMPRESSED;
    pub const VS_FORMAT_MJPEG: u8 = super::descriptors::vs_descriptor_subtypes::FORMAT_MJPEG;
    pub const VS_FRAME_MJPEG: u8 = super::descriptors::vs_descriptor_subtypes::FRAME_MJPEG;
    pub const VS_FORMAT_MPEG2TS: u8 = super::descriptors::vs_descriptor_subtypes::FORMAT_MPEG2TS;
    pub const VS_FORMAT_DV: u8 = super::descriptors::vs_descriptor_subtypes::FORMAT_DV;
    pub const VS_COLORFORMAT: u8 = super::descriptors::vs_descriptor_subtypes::COLORFORMAT;
    pub const VS_FORMAT_FRAME_BASED: u8 =
        super::descriptors::vs_descriptor_subtypes::FORMAT_FRAME_BASED;
    pub const VS_FRAME_FRAME_BASED: u8 =
        super::descriptors::vs_descriptor_subtypes::FRAME_FRAME_BASED;
    pub const VS_FORMAT_STREAM_BASED: u8 =
        super::descriptors::vs_descriptor_subtypes::FORMAT_STREAM_BASED;
    pub const VS_FORMAT_H264: u8 = super::descriptors::vs_descriptor_subtypes::FORMAT_H264;
    pub const VS_FRAME_H264: u8 = super::descriptors::vs_descriptor_subtypes::FRAME_H264;
    pub const VS_FORMAT_H264_SIMULCAST: u8 =
        super::descriptors::vs_descriptor_subtypes::FORMAT_H264_SIMULCAST;
}

pub mod uvc_guids {
    pub use crate::descriptors::format_guids::*;
}

#[derive(Debug, Clone)]
pub struct VideoFormat {
    pub width: u16,
    pub height: u16,
    pub frame_rate: u32, // 帧率 (fps)
    pub format_type: VideoFormatType,
}

#[derive(Debug, Clone, Copy)]
pub enum VideoFormatType {
    Uncompressed(UncompressedFormat),
    Mjpeg,
    H264,
}

impl VideoFormat {
    pub fn frame_bytes(&self) -> usize {
        match self.format_type {
            VideoFormatType::Uncompressed(t) => {
                let pixel_size = match t {
                    UncompressedFormat::Yuy2 => 2,  // YUY2 每像素2字节
                    UncompressedFormat::Nv12 => 1,  // NV12 每像素1字节 (平均)
                    UncompressedFormat::Rgb24 => 3, // RGB24 每像素3字节
                    UncompressedFormat::Rgb32 => 4, // RGB32 每像素4字节
                };
                (self.width as usize) * (self.height as usize) * pixel_size
            }
            VideoFormatType::Mjpeg => {
                // MJPEG 压缩后大小不定，这里返回一个估算值（假设压缩比为10:1）
                ((self.width as usize) * (self.height as usize) * 3) / 10
            }
            VideoFormatType::H264 => {
                // H.264 压缩后大小不定，这里返回一个估算值（假设压缩比为20:1）
                ((self.width as usize) * (self.height as usize) * 3) / 20
            }
        }
    }
}

/// 未压缩视频格式类型
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UncompressedFormat {
    /// YUY2 (YUYV) 格式
    Yuy2,
    /// NV12 格式
    Nv12,
    /// RGB24 格式
    Rgb24,
    /// RGB32 格式
    Rgb32,
}

/// 视频控制事件
#[derive(Debug, Clone)]
pub enum VideoControlEvent {
    /// 视频格式变更
    FormatChanged(VideoFormat),
    /// 亮度调整
    BrightnessChanged(i16),
    /// 对比度调整
    ContrastChanged(i16),
    /// 色调调整
    HueChanged(i16),
    /// 饱和度调整
    SaturationChanged(i16),
    /// 错误事件
    Error(String),
}

/// 视频数据帧
#[derive(Debug)]
pub struct VideoFrame {
    /// 帧数据
    pub data: Vec<u8>,
    /// 时间戳
    pub timestamp: u64,
    /// 帧序号
    pub frame_number: u32,
    /// 数据格式
    pub format: VideoFormat,
    /// 是否是帧结束标志
    pub end_of_frame: bool,
}

/// UVC 设备状态
#[derive(Debug, Clone, PartialEq)]
pub enum UvcDeviceState {
    /// 未配置
    Unconfigured,
    /// 已配置但未开始流传输
    Configured,
    /// 正在进行流传输
    Streaming,
    /// 错误状态
    Error(String),
}

/// UVC Stream Control 结构体 (参考 UVC 规范 4.3.1.1)
#[derive(Debug, Clone)]
struct StreamControl {
    hint: u16,                      // bmHint
    format_index: u8,               // bFormatIndex
    frame_index: u8,                // bFrameIndex
    frame_interval: u32,            // dwFrameInterval (100ns units)
    key_frame_rate: u16,            // wKeyFrameRate
    p_frame_rate: u16,              // wPFrameRate
    comp_quality: u16,              // wCompQuality
    comp_window_size: u16,          // wCompWindowSize
    delay: u16,                     // wDelay
    max_video_frame_size: u32,      // dwMaxVideoFrameSize
    max_payload_transfer_size: u32, // dwMaxPayloadTransferSize
}

pub struct UvcDevice {
    device: Device,

    video_streaming_interface_num: u8,
    processing_unit_id: Option<u8>, // 处理单元ID
    current_format: Option<VideoFormat>,
    state: UvcDeviceState,
    descriptor_parser: DescriptorParser, // 新增描述符解析器
}

impl UvcDevice {
    /// 检查设备是否为 UVC 设备
    pub fn check(info: &DeviceInfo) -> bool {
        let mut has_video_control = false;
        let mut has_video_streaming = false;

        for iface in info.interface_descriptors() {
            match iface.class() {
                Class::Video | Class::AudioVideo(_) => {
                    // UVC Video Control Interface (subclass=1)
                    if iface.subclass == 1 {
                        has_video_control = true;
                    }
                    // UVC Video Streaming Interface (subclass=2)
                    if iface.subclass == 2 {
                        has_video_streaming = true;
                    }
                }
                _ => {}
            }
        }

        has_video_control && has_video_streaming
    }

    /// 创建新的 UVC 设备实例
    pub async fn new(mut device: Device) -> Result<Self, USBError> {
        for config in device.configurations().iter() {
            debug!(
                "Configuration: {}",
                match &config.string {
                    Some(v) => v.clone(),
                    None => format!("{}", config.configuration_value),
                }
            );
        }

        // 首先保存需要的接口信息，避免同时持有可变和不可变引用
        let (video_control_info, video_streaming_info) = {
            let config = &device.configurations()[0];

            // 查找 Video Control Interface (class=14, subclass=1)
            let video_control_iface = config
                .interfaces
                .iter()
                .find(|iface| {
                    let iface = iface.first_alt_setting();
                    matches!(iface.class(), Class::Video) && iface.subclass == 1
                })
                .ok_or(USBError::NotFound)?
                .first_alt_setting();

            // 查找 Video Streaming Interface (class=14, subclass=2)
            let video_streaming_iface = config
                .interfaces
                .iter()
                .find(|iface| {
                    let iface = iface.first_alt_setting();
                    matches!(iface.class(), Class::Video) && iface.subclass == 2
                })
                .map(|iface| iface.first_alt_setting());

            (
                (
                    video_control_iface.interface_number,
                    video_control_iface.alternate_setting,
                ),
                video_streaming_iface.map(|vs| (vs.interface_number, vs.alternate_setting)),
            )
        };

        debug!("Using Video Control interface: {video_control_info:?}");

        device
            .claim_interface(video_control_info.0, video_control_info.1)
            .await?;

        Ok(Self {
            device,
            // video_streaming_interface,
            video_streaming_interface_num: video_streaming_info
                .map(|(num, _)| num)
                .expect("Video Streaming interface number is required"),
            processing_unit_id: Some(1), // 通常处理单元ID为1，实际应用中应该解析描述符
            // ep_in,
            current_format: None,
            state: UvcDeviceState::Configured,
            descriptor_parser: DescriptorParser::new(),
        })
    }

    /// 获取设备支持的视频格式列表
    pub async fn get_supported_formats(&mut self) -> Result<Vec<VideoFormat>, USBError> {
        let mut formats = Vec::new();

        // 获取完整的配置描述符来解析VS接口的额外描述符
        let vs_interface_num = self.video_streaming_interface_num;
        trace!("Parsing VS interface {vs_interface_num} descriptors");

        // 首先尝试通过GET_DESCRIPTOR控制请求获取完整的配置描述符
        match self.get_full_configuration_descriptor().await {
            Ok(config_data) => {
                trace!(
                    "Got full configuration descriptor: {} bytes",
                    config_data.len()
                );

                // 解析配置描述符中的VS接口部分
                if let Ok(parsed_formats) =
                    self.parse_vs_interface_descriptors(&config_data, vs_interface_num)
                    && !parsed_formats.is_empty()
                {
                    trace!(
                        "Parsed {} formats from VS interface descriptors",
                        parsed_formats.len()
                    );
                    formats.extend(parsed_formats);
                }
            }
            Err(e) => {
                debug!("Failed to get full configuration descriptor: {e:?}");
            }
        }

        // 如果上面的方法失败，尝试获取VS接口特定的描述符
        if formats.is_empty() {
            match self.get_vs_interface_descriptor(vs_interface_num).await {
                Ok(vs_desc_data) => {
                    trace!("Got VS interface descriptor: {} bytes", vs_desc_data.len());
                    if let Ok(parsed_formats) = self.parse_format_descriptors(&vs_desc_data)
                        && !parsed_formats.is_empty()
                    {
                        trace!(
                            "Parsed {} formats from VS interface specific descriptors",
                            parsed_formats.len()
                        );
                        formats.extend(parsed_formats);
                    }
                }
                Err(e) => {
                    debug!("Failed to get VS interface descriptor: {e:?}");
                }
            }
        }

        Ok(formats)
    }

    /// 通过控制请求获取完整的配置描述符
    async fn get_full_configuration_descriptor(&mut self) -> Result<Vec<u8>, USBError> {
        let setup = ControlSetup {
            request_type: RequestType::Standard,
            recipient: Recipient::Device,
            request: Request::GetDescriptor,
            value: (0x02 << 8), // Configuration descriptor type
            index: 0,           // Configuration index
        };

        // 首先获取配置描述符头来确定总长度
        let mut header_buffer = vec![0u8; 9]; // 配置描述符头是9字节
        self.device.control_in(setup, &mut header_buffer).await?;

        if header_buffer.len() < 4 {
            Err(anyhow!("Failed to read configuration descriptor header"))?;
        }

        // 提取总长度（小端格式）
        let total_length = u16::from_le_bytes([header_buffer[2], header_buffer[3]]) as usize;
        trace!("Configuration descriptor total length: {total_length} bytes");

        if total_length < 9 {
            Err(anyhow!("Invalid configuration descriptor length"))?;
        }

        // 获取完整的配置描述符
        let mut full_buffer = alloc::vec![0u8; total_length];
        let setup_full = ControlSetup {
            request_type: RequestType::Standard,
            recipient: Recipient::Device,
            request: Request::GetDescriptor,
            value: (0x02 << 8), // Configuration descriptor type
            index: 0,           // Configuration index
        };

        self.device.control_in(setup_full, &mut full_buffer).await?;

        Ok(full_buffer)
    }

    /// 解析VS接口描述符中的格式信息
    fn parse_vs_interface_descriptors(
        &self,
        config_data: &[u8],
        vs_interface_num: u8,
    ) -> Result<Vec<VideoFormat>, USBError> {
        let mut formats = Vec::new();
        let mut pos = 0;
        let mut found_vs_interface = false;
        let mut current_format_type: Option<VideoFormatType> = None;

        trace!(
            "Parsing configuration descriptor of {} bytes for VS interface {}",
            config_data.len(),
            vs_interface_num
        );

        // 解析配置描述符
        while pos < config_data.len() {
            if pos + 2 > config_data.len() {
                break;
            }

            let length = config_data[pos] as usize;
            let descriptor_type = config_data[pos + 1];

            if length < 2 || pos + length > config_data.len() {
                pos += 1; // 尝试恢复解析
                continue;
            }

            match descriptor_type {
                0x04 => {
                    // Interface descriptor
                    if length >= 9 {
                        let interface_number = config_data[pos + 2];
                        let alternate_setting = config_data[pos + 3];
                        let interface_class = config_data[pos + 5];
                        let interface_subclass = config_data[pos + 6];

                        debug!(
                            "Found interface {interface_number} alt {alternate_setting} class {interface_class} subclass {interface_subclass}"
                        );

                        // 检查是否是我们要找的VS接口 (class=14, subclass=2)
                        if interface_number == vs_interface_num
                            && interface_class == 14
                            && interface_subclass == 2
                        {
                            found_vs_interface = true;
                            trace!("Found target VS interface {vs_interface_num}");
                        } else {
                            found_vs_interface = false;
                        }
                    }
                }
                0x24 => {
                    // Class-specific interface descriptor
                    if found_vs_interface && length >= 3 {
                        let subtype = config_data[pos + 2];
                        trace!(
                            "Found class-specific descriptor subtype 0x{subtype:02x} length {length}"
                        );

                        match subtype {
                            uvc_interface_subtypes::VS_FORMAT_MJPEG => {
                                trace!("Parsing MJPEG format descriptor");
                                current_format_type = Some(VideoFormatType::Mjpeg);
                            }
                            uvc_interface_subtypes::VS_FORMAT_UNCOMPRESSED => {
                                trace!("Parsing uncompressed format descriptor");
                                if let Ok(format_type) = self
                                    .parse_uncompressed_format_type(&config_data[pos..pos + length])
                                {
                                    current_format_type =
                                        Some(VideoFormatType::Uncompressed(format_type));
                                }
                            }
                            uvc_interface_subtypes::VS_FORMAT_H264 => {
                                trace!("Found H264 format descriptor");
                                current_format_type = Some(VideoFormatType::H264);
                            }
                            uvc_interface_subtypes::VS_FRAME_MJPEG
                            | uvc_interface_subtypes::VS_FRAME_UNCOMPRESSED => {
                                trace!("Parsing frame descriptor subtype 0x{subtype:02x}");
                                if let Some(format_type) = current_format_type
                                    && let Ok(frame_formats) = self.parse_frame_descriptor(
                                        &config_data[pos..pos + length],
                                        format_type,
                                    )
                                {
                                    formats.extend(frame_formats);
                                }
                            }
                            _ => {
                                debug!("Unknown VS descriptor subtype: 0x{subtype:02x}");
                            }
                        }
                    }
                }
                _ => {
                    // 其他描述符类型，跳过
                }
            }

            pos += length;
        }

        trace!(
            "Parsed {} video formats from VS interface descriptors",
            formats.len()
        );
        Ok(formats)
    }

    /// 解析未压缩格式类型（仅返回格式类型，不生成VideoFormat）
    fn parse_uncompressed_format_type(&self, data: &[u8]) -> Result<UncompressedFormat, USBError> {
        match self.descriptor_parser.parse_uncompressed_format(data) {
            Ok(desc) => {
                // 根据GUID确定格式类型
                let format_type = if desc.guid == format_guids::YUY2 {
                    debug!("Detected YUY2 format");
                    UncompressedFormat::Yuy2
                } else if desc.guid == format_guids::NV12 {
                    debug!("Detected NV12 format");
                    UncompressedFormat::Nv12
                } else if desc.guid == format_guids::RGB24 {
                    debug!("Detected RGB24 format");
                    UncompressedFormat::Rgb24
                } else {
                    debug!(
                        "Unknown uncompressed format GUID: {:02x?}, defaulting to YUY2",
                        desc.guid
                    );
                    UncompressedFormat::Yuy2 // 默认为YUY2
                };

                Ok(format_type)
            }
            Err(e) => Err(e),
        }
    }

    /// 解析帧描述符
    fn parse_frame_descriptor(
        &self,
        data: &[u8],
        format_type: VideoFormatType,
    ) -> Result<Vec<VideoFormat>, USBError> {
        match self.descriptor_parser.parse_frame_descriptor(data) {
            Ok(frame_desc) => {
                // 计算默认帧率 (frame interval 以100ns为单位)
                let default_frame_rate =
                    DescriptorParser::interval_to_fps(frame_desc.default_frame_interval);

                // 根据格式类型创建VideoFormat
                let video_format = VideoFormat {
                    width: frame_desc.width,
                    height: frame_desc.height,
                    frame_rate: default_frame_rate,
                    format_type,
                };

                trace!("Parsed frame format: {video_format:?}");
                Ok(vec![video_format])
            }
            Err(e) => Err(e),
        }
    }

    /// 通过控制请求获取VS接口描述符
    async fn get_vs_interface_descriptor(
        &mut self,
        interface_num: u8,
    ) -> Result<Vec<u8>, USBError> {
        let setup = ControlSetup {
            request_type: RequestType::Standard,
            recipient: Recipient::Interface,
            request: Request::GetDescriptor,
            value: (0x04 << 8), // Interface descriptor type
            index: interface_num as u16,
        };

        let mut buffer = alloc::vec![0u8; 1024]; // 1KB缓冲区

        // 使用video control接口发送请求
        self.device.control_in(setup, &mut buffer).await?;

        Ok(buffer)
    }

    /// 解析UVC格式描述符
    fn parse_format_descriptors(&self, data: &[u8]) -> Result<Vec<VideoFormat>, USBError> {
        let mut formats = Vec::new();
        let mut pos = 0;

        while pos < data.len() {
            if pos + 2 > data.len() {
                break;
            }

            let length = data[pos] as usize;
            let descriptor_type = data[pos + 1];

            if length < 3 || pos + length > data.len() {
                break;
            }

            // 检查是否是类特定接口描述符
            if descriptor_type == uvc_descriptor_types::CS_INTERFACE && length >= 3 {
                let subtype = data[pos + 2];

                match subtype {
                    uvc_interface_subtypes::VS_FORMAT_MJPEG => {
                        debug!("Found MJPEG format descriptor");
                        if let Ok(mjpeg_formats) = self.parse_mjpeg_format(&data[pos..pos + length])
                        {
                            formats.extend(mjpeg_formats);
                        }
                    }
                    uvc_interface_subtypes::VS_FORMAT_UNCOMPRESSED => {
                        debug!("Found uncompressed format descriptor");
                        if let Ok(uncompressed_formats) =
                            self.parse_uncompressed_format(&data[pos..pos + length])
                        {
                            formats.extend(uncompressed_formats);
                        }
                    }
                    uvc_interface_subtypes::VS_FORMAT_H264 => {
                        debug!("Found H264 format descriptor");
                        // H264格式解析可以在这里添加
                    }
                    _ => {
                        debug!("Unknown format descriptor subtype: 0x{subtype:02x}");
                    }
                }
            }

            pos += length;
        }

        Ok(formats)
    }

    /// 解析MJPEG格式描述符
    fn parse_mjpeg_format(&self, data: &[u8]) -> Result<Vec<VideoFormat>, USBError> {
        if data.len() < 11 {
            Err(anyhow!("mjpeg format data len not ok"))?;
        }

        let format_index = data[3];
        let num_frame_descriptors = data[4];
        let flags = data[5];
        let default_frame_index = data[6];
        let aspect_ratio_x = data[7];
        let aspect_ratio_y = data[8];
        let interlace_flags = data[9];
        let copy_protect = data[10];

        debug!(
            "MJPEG format: index={format_index}, frames={num_frame_descriptors}, flags=0x{flags:02x}, default_frame={default_frame_index}, aspect={aspect_ratio_x}:{aspect_ratio_y}, interlace=0x{interlace_flags:02x}, copy_protect=0x{copy_protect:02x}"
        );

        // 返回一些基于实际描述符信息的MJPEG格式
        // 在完整实现中，应该继续解析后续的帧描述符来获取具体的分辨率和帧率
        let mut formats = Vec::new();

        // 添加一些常见的MJPEG分辨率，实际应该从帧描述符中解析
        for &(width, height) in &[(640, 480), (1280, 720), (1920, 1080)] {
            formats.push(VideoFormat {
                width,
                height,
                frame_rate: 30,
                format_type: VideoFormatType::Mjpeg,
            });
        }

        debug!(
            "Generated {} MJPEG formats based on format descriptor",
            formats.len()
        );
        Ok(formats)
    }

    /// 解析未压缩格式描述符
    fn parse_uncompressed_format(&self, data: &[u8]) -> Result<Vec<VideoFormat>, USBError> {
        if data.len() < 27 {
            Err(anyhow!("Uncompressed format descriptor too short"))?;
        }

        let format_index = data[3];
        let num_frame_descriptors = data[4];
        let guid = &data[5..21];
        let bits_per_pixel = data[21];
        let default_frame_index = data[22];
        let aspect_ratio_x = data[23];
        let aspect_ratio_y = data[24];
        let interlace_flags = data[25];
        let copy_protect = data[26];

        debug!(
            "Uncompressed format: index={format_index}, frames={num_frame_descriptors}, bpp={bits_per_pixel}, default_frame={default_frame_index}, aspect={aspect_ratio_x}:{aspect_ratio_y}, interlace=0x{interlace_flags:02x}, copy_protect=0x{copy_protect:02x}"
        );

        debug!("Format GUID: {guid:02x?}");

        // 根据GUID确定格式类型
        let format_type = if guid == uvc_guids::YUY2 {
            debug!("Detected YUY2 format");
            UncompressedFormat::Yuy2
        } else if guid == uvc_guids::NV12 {
            debug!("Detected NV12 format");
            UncompressedFormat::Nv12
        } else if guid == uvc_guids::RGB24 {
            debug!("Detected RGB24 format");
            UncompressedFormat::Rgb24
        } else {
            debug!("Unknown uncompressed format GUID: {guid:02x?}, defaulting to YUY2");
            UncompressedFormat::Yuy2 // 默认为YUY2
        };

        // 返回一些基于实际描述符信息的未压缩格式
        // 在完整实现中，应该继续解析后续的帧描述符来获取具体的分辨率和帧率
        let mut formats = Vec::new();

        // 添加一些常见的分辨率，实际应该从帧描述符中解析
        for &(width, height) in &[(320, 240), (640, 480), (1280, 720)] {
            formats.push(VideoFormat {
                width,
                height,
                frame_rate: 30, // 默认帧率，实际应该从帧描述符解析
                format_type: VideoFormatType::Uncompressed(format_type),
            });
        }

        debug!(
            "Generated {} uncompressed formats based on format descriptor",
            formats.len()
        );
        Ok(formats)
    }

    /// 设置视频格式
    pub async fn set_format(&mut self, format: VideoFormat) -> Result<(), USBError> {
        debug!("Setting video format: {format:?}");

        // 参考 libuvc 实现，需要先 probe 然后 commit
        // 1. 构建 VS stream control 结构
        let mut stream_ctrl = self.build_stream_control(&format).await?;

        // 2. 先发送 PROBE 控制请求
        debug!("Sending PROBE control request");
        self.send_vs_control(vs_controls::VS_PROBE_CONTROL, &stream_ctrl)
            .await?;

        // 3. 获取设备的 PROBE 响应
        debug!("Getting PROBE response");
        let probe_response = self
            .get_vs_control(vs_controls::VS_PROBE_CONTROL, 26)
            .await?;
        stream_ctrl = self.parse_stream_control(&probe_response)?;

        // 4. 发送 COMMIT 控制请求
        debug!("Sending COMMIT control request");
        self.send_vs_control(vs_controls::VS_COMMIT_CONTROL, &stream_ctrl)
            .await?;

        debug!("Video format set successfully");
        self.current_format = Some(format);
        Ok(())
    }

    /// 开始视频流传输
    pub async fn start_streaming(&mut self) -> Result<VideoStream, USBError> {
        let vs_interface_num = self.video_streaming_interface_num;

        let current_format = self
            .current_format
            .clone()
            .ok_or(anyhow!("No format selected"))?;

        // 参考 libuvc 的实现，根据 dwMaxPayloadTransferSize 选择合适的 alternate setting
        let config = &self.device.configurations()[0];
        let vs_interface_group = config
            .interfaces
            .iter()
            .find(|iface| iface.first_alt_setting().interface_number == vs_interface_num)
            .ok_or(USBError::NotFound)?;

        let max_payload_size = current_format.frame_bytes();

        debug!("Looking for alternate setting with payload size >= {max_payload_size}");

        // 查找能够满足带宽要求的 alternate setting
        let mut best_alt_setting = None;
        let mut best_endpoint_size = 0;

        for alt_setting in vs_interface_group.alt_settings.iter() {
            for endpoint in &alt_setting.endpoints {
                if matches!(endpoint.transfer_type, EndpointType::Isochronous)
                    && matches!(endpoint.direction, Direction::In)
                {
                    let packet_size = endpoint.max_packet_size as usize;
                    debug!(
                        "Alt setting {}: endpoint size = {}",
                        alt_setting.alternate_setting, packet_size
                    );

                    // 选择适中的端点大小以获得稳定的带宽
                    // 避免选择太小（<256）或太大（>1024）的端点
                    if (256..=1024).contains(&packet_size) && packet_size > best_endpoint_size {
                        best_alt_setting = Some(alt_setting.clone());
                        best_endpoint_size = packet_size;
                    } else if best_alt_setting.is_none() && packet_size > best_endpoint_size {
                        // 如果没有找到理想范围内的，选择最大的
                        best_alt_setting = Some(alt_setting.clone());
                        best_endpoint_size = packet_size;
                    }
                }
            }
        }

        let alt_setting =
            best_alt_setting.unwrap_or(vs_interface_group.alt_settings.first().cloned().unwrap()); // 默认为 alt setting 1

        debug!(
            "Selected alternate setting {} with endpoint size {best_endpoint_size}",
            alt_setting.alternate_setting
        );

        // 切换到选中的 alternate setting
        self.device
            .claim_interface(vs_interface_num, alt_setting.alternate_setting)
            .await?;

        let mut ep = None;
        // 查找同步 IN 端点
        for endpoint in alt_setting.endpoints.clone().into_iter() {
            if matches!(endpoint.transfer_type, EndpointType::Isochronous)
                && matches!(endpoint.direction, Direction::In)
            {
                debug!("Found isochronous IN endpoint: {endpoint:?}");
                ep = Some(endpoint.clone());
                break;
            }
        }

        let ep_desc = ep.ok_or(anyhow!("No isochronous IN endpoint found"))?;
        let ep = self.device.endpoint(ep_desc.address)?;

        debug!("Starting video streaming");
        self.state = UvcDeviceState::Streaming;
        Ok(VideoStream::new(
            ep,
            ep_desc,
            self.current_format.clone().unwrap(),
        ))
    }

    /// 获取当前设备状态
    pub fn get_state(&self) -> &UvcDeviceState {
        &self.state
    }

    /// 获取当前视频格式
    pub fn get_current_format(&self) -> Option<&VideoFormat> {
        self.current_format.as_ref()
    }

    /// 发送视频控制命令
    pub async fn send_control_command(
        &mut self,
        command: VideoControlEvent,
    ) -> Result<(), USBError> {
        debug!("Sending video control command: {command:?}");

        let processing_unit_id = self.processing_unit_id.ok_or(USBError::NotFound)?;

        match command {
            VideoControlEvent::BrightnessChanged(value) => {
                debug!("Setting brightness to: {value}");
                self.send_pu_control(
                    pu_controls::PU_BRIGHTNESS_CONTROL,
                    processing_unit_id,
                    &value.to_le_bytes(),
                )
                .await?;
            }
            VideoControlEvent::ContrastChanged(value) => {
                debug!("Setting contrast to: {value}");
                self.send_pu_control(
                    pu_controls::PU_CONTRAST_CONTROL,
                    processing_unit_id,
                    &(value as u16).to_le_bytes(),
                )
                .await?;
            }
            VideoControlEvent::HueChanged(value) => {
                debug!("Setting hue to: {value}");
                self.send_pu_control(
                    pu_controls::PU_HUE_CONTROL,
                    processing_unit_id,
                    &value.to_le_bytes(),
                )
                .await?;
            }
            VideoControlEvent::SaturationChanged(value) => {
                debug!("Setting saturation to: {value}");
                self.send_pu_control(
                    pu_controls::PU_SATURATION_CONTROL,
                    processing_unit_id,
                    &(value as u16).to_le_bytes(),
                )
                .await?;
            }
            _ => {
                debug!("Control command not implemented: {command:?}");
            }
        }

        Ok(())
    }

    /// 发送处理单元控制请求
    async fn send_pu_control(
        &mut self,
        control_selector: u8,
        unit_id: u8,
        data: &[u8],
    ) -> Result<(), USBError> {
        let setup = ControlSetup {
            request_type: RequestType::Class,
            recipient: Recipient::Interface,
            request: uvc_requests::SET_CUR.into(),
            value: (control_selector as u16) << 8,
            index: unit_id as u16,
        };

        self.device.control_out(setup, data).await?;

        Ok(())
    }

    /// 构建 Stream Control 结构体
    ///
    /// 此函数参考了 libuvc 的 uvc_get_stream_ctrl_format_size 实现，包括：
    /// 1. 通过遍历设备描述符来查找匹配的格式和帧索引（而不是使用硬编码的值）
    /// 2. 正确计算帧间隔（frame interval），使用100ns为单位
    /// 3. 根据不同的格式类型估算最大帧大小
    /// 4. 设置适当的 bmHint 标志位
    ///
    /// libuvc 参考：
    /// - src/stream.c:uvc_get_stream_ctrl_format_size (line 474-524)
    /// - src/stream.c:_uvc_find_frame_desc_stream_if (line 415-444)
    async fn build_stream_control(
        &mut self,
        format: &VideoFormat,
    ) -> Result<StreamControl, USBError> {
        debug!("Building stream control for format: {format:?}");

        // 获取支持的格式列表来查找对应的格式索引
        let formats = self.get_supported_formats().await?;

        // 查找匹配的格式和帧索引（参考 libuvc 的实现逻辑）
        let (format_index, frame_index) =
            self.find_format_indices(&formats, format).ok_or_else(|| {
                debug!("Failed to find matching format for: {format:?}");
                anyhow!("No matching format found")
            })?;

        // 计算帧间隔 (100ns 单位)，参考 libuvc 的计算方式
        let frame_interval = if format.frame_rate > 0 {
            10_000_000 / format.frame_rate // 100ns units，与 libuvc 一致
        } else {
            333333 // 默认 30fps (10,000,000 / 30)
        };

        // 根据格式类型估算最大帧大小
        let width = format.width as u32;
        let height = format.height as u32;

        let max_frame_size = match format.format_type {
            VideoFormatType::Mjpeg => {
                // MJPEG 压缩格式：参考 libuvc，通常为未压缩大小的一半左右
                width * height * 2
            }
            VideoFormatType::Uncompressed(uncompressed_format) => {
                // 未压缩格式：根据具体格式计算
                match uncompressed_format {
                    UncompressedFormat::Yuy2 => width * height * 2, // YUY2: 2 bytes per pixel
                    UncompressedFormat::Nv12 => width * height * 3 / 2, // NV12: 1.5 bytes per pixel
                    UncompressedFormat::Rgb24 => width * height * 3, // RGB24: 3 bytes per pixel
                    UncompressedFormat::Rgb32 => width * height * 4, // RGB32: 4 bytes per pixel
                }
            }
            VideoFormatType::H264 => {
                // H264 压缩格式：估算为未压缩大小的 1/4 到 1/8
                width * height / 2
            }
        };

        Ok(StreamControl {
            hint: 0x0001, // bmHint: dwFrameInterval field shall be kept fixed (参考 libuvc)
            format_index,
            frame_index,
            frame_interval,
            key_frame_rate: 0,   // 默认为 0，让设备决定
            p_frame_rate: 0,     // 默认为 0，让设备决定
            comp_quality: 0,     // 默认为 0，让设备决定
            comp_window_size: 0, // 默认为 0
            delay: 0,            // 默认为 0
            max_video_frame_size: max_frame_size,
            max_payload_transfer_size: 0, // 让设备决定，参考 libuvc
        })
    }

    /// 查找格式和帧索引
    ///
    /// 此函数参考了 libuvc 的 _uvc_find_frame_desc_stream_if 实现，提供了：
    /// 1. 精确的格式类型匹配（包括未压缩格式的子类型）
    /// 2. 分辨率匹配检查
    /// 3. 优雅的降级策略（exact match -> format type match -> default）
    /// 4. 符合 UVC 规范的索引计算（从1开始）
    ///
    /// libuvc 参考：
    /// - src/stream.c:_uvc_find_frame_desc_stream_if (line 415-444)
    /// - src/stream.c:uvc_find_frame_desc (line 444-474)
    fn find_format_indices(
        &self,
        formats: &[VideoFormat],
        target: &VideoFormat,
    ) -> Option<(u8, u8)> {
        // 遍历所有支持的格式，寻找匹配的格式和帧配置
        for (format_idx, format) in formats.iter().enumerate() {
            // 检查格式类型是否匹配
            if core::mem::discriminant(&format.format_type)
                != core::mem::discriminant(&target.format_type)
            {
                continue;
            }

            // 对于未压缩格式，还需要检查具体的子格式
            if let (
                VideoFormatType::Uncompressed(format_type),
                VideoFormatType::Uncompressed(target_type),
            ) = (&format.format_type, &target.format_type)
                && format_type != target_type
            {
                continue;
            }

            // 检查分辨率是否匹配
            if format.width == target.width && format.height == target.height {
                // 找到匹配的格式，返回索引（从 1 开始，符合 UVC 规范）
                let format_index = (format_idx + 1) as u8;
                let frame_index = 1u8; // 简化实现，假设每个格式只有一个帧配置

                debug!(
                    "Found matching format: format_index={}, frame_index={}",
                    format_index, frame_index
                );
                return Some((format_index, frame_index));
            }
        }

        // 如果没有找到完全匹配的，尝试找到相同格式类型的第一个配置
        for (format_idx, format) in formats.iter().enumerate() {
            if core::mem::discriminant(&format.format_type)
                == core::mem::discriminant(&target.format_type)
            {
                let format_index = (format_idx + 1) as u8;
                let frame_index = 1u8;

                debug!(
                    "Using fallback format: format_index={}, frame_index={}",
                    format_index, frame_index
                );
                return Some((format_index, frame_index));
            }
        }

        // 如果还是没有找到，使用默认值（参考 libuvc 的错误处理）
        debug!("No matching format found, using default indices");
        None
    }

    /// 发送 VS 控制请求
    async fn send_vs_control(
        &mut self,
        control_selector: u8,
        stream_ctrl: &StreamControl,
    ) -> Result<(), USBError> {
        let vs_interface_num = self.video_streaming_interface_num;

        // 序列化 StreamControl 到字节数组
        let data = self.serialize_stream_control(stream_ctrl);

        let setup = ControlSetup {
            request_type: RequestType::Class,
            recipient: Recipient::Interface,
            request: uvc_requests::SET_CUR.into(),
            value: (control_selector as u16) << 8,
            index: vs_interface_num as u16,
        };

        debug!(
            "Sending VS control: selector=0x{:02x}, data_len={}",
            control_selector,
            data.len()
        );

        // 使用 video control 接口发送请求到 video streaming 接口
        self.device.control_out(setup, &data).await?;

        Ok(())
    }

    /// 获取 VS 控制响应
    async fn get_vs_control(
        &mut self,
        control_selector: u8,
        length: usize,
    ) -> Result<Vec<u8>, USBError> {
        let vs_interface_num = self.video_streaming_interface_num;

        let setup = ControlSetup {
            request_type: RequestType::Class,
            recipient: Recipient::Interface,
            request: uvc_requests::GET_CUR.into(),
            value: (control_selector as u16) << 8,
            index: vs_interface_num as u16,
        };

        let mut buffer = vec![0u8; length];
        self.device.control_in(setup, &mut buffer).await?;

        debug!(
            "Received VS control response: selector=0x{:02x}, data_len={}",
            control_selector,
            buffer.len()
        );

        Ok(buffer)
    }

    /// 序列化 StreamControl 结构体
    fn serialize_stream_control(&self, ctrl: &StreamControl) -> Vec<u8> {
        let mut data = Vec::with_capacity(26);

        // bmHint (2 bytes)
        data.extend(&ctrl.hint.to_le_bytes());
        // bFormatIndex (1 byte)
        data.push(ctrl.format_index);
        // bFrameIndex (1 byte)
        data.push(ctrl.frame_index);
        // dwFrameInterval (4 bytes)
        data.extend(&ctrl.frame_interval.to_le_bytes());
        // wKeyFrameRate (2 bytes)
        data.extend(&ctrl.key_frame_rate.to_le_bytes());
        // wPFrameRate (2 bytes)
        data.extend(&ctrl.p_frame_rate.to_le_bytes());
        // wCompQuality (2 bytes)
        data.extend(&ctrl.comp_quality.to_le_bytes());
        // wCompWindowSize (2 bytes)
        data.extend(&ctrl.comp_window_size.to_le_bytes());
        // wDelay (2 bytes)
        data.extend(&ctrl.delay.to_le_bytes());
        // dwMaxVideoFrameSize (4 bytes)
        data.extend(&ctrl.max_video_frame_size.to_le_bytes());
        // dwMaxPayloadTransferSize (4 bytes)
        data.extend(&ctrl.max_payload_transfer_size.to_le_bytes());

        debug!("Serialized stream control: {} bytes", data.len());
        data
    }

    /// 解析 StreamControl 响应
    fn parse_stream_control(&self, data: &[u8]) -> Result<StreamControl, USBError> {
        if data.len() < 26 {
            Err(anyhow!("Stream control response too short"))?;
        }

        let hint = u16::from_le_bytes([data[0], data[1]]);
        let format_index = data[2];
        let frame_index = data[3];
        let frame_interval = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let key_frame_rate = u16::from_le_bytes([data[8], data[9]]);
        let p_frame_rate = u16::from_le_bytes([data[10], data[11]]);
        let comp_quality = u16::from_le_bytes([data[12], data[13]]);
        let comp_window_size = u16::from_le_bytes([data[14], data[15]]);
        let delay = u16::from_le_bytes([data[16], data[17]]);
        let max_video_frame_size = u32::from_le_bytes([data[18], data[19], data[20], data[21]]);
        let max_payload_transfer_size =
            u32::from_le_bytes([data[22], data[23], data[24], data[25]]);

        debug!(
            "Parsed stream control: format={format_index}, frame={frame_index}, interval={frame_interval}, max_frame_size={max_video_frame_size}"
        );

        Ok(StreamControl {
            hint,
            format_index,
            frame_index,
            frame_interval,
            key_frame_rate,
            p_frame_rate,
            comp_quality,
            comp_window_size,
            delay,
            max_video_frame_size,
            max_payload_transfer_size,
        })
    }

    // /// 获取当前的 Stream Control 参数
    // async fn get_current_stream_control(&mut self) -> Result<StreamControl, USBError> {
    //     // 发送 GET_CUR 请求获取当前的 commit 参数
    //     debug!("Getting current stream control parameters");
    //     let response = self
    //         .get_vs_control(vs_controls::VS_COMMIT_CONTROL, 26)
    //         .await?;
    //     self.parse_stream_control(&response)
    // }

    /// 获取设备信息字符串
    pub async fn get_device_info(&self) -> Result<String, USBError> {
        // 在实际实现中，这里可以读取设备的字符串描述符
        Ok("UVC Video Device".to_string())
    }

    /// 获取流错误代码
    pub async fn get_stream_error_code(&mut self) -> Result<u8, USBError> {
        debug!("Getting stream error code");
        let response = self
            .get_vs_control(vs_controls::STREAM_ERROR_CODE, 1)
            .await?;
        let error_code = response.first().copied().unwrap_or(0);
        debug!("Stream error code: 0x{:02x}", error_code);
        Ok(error_code)
    }
}
