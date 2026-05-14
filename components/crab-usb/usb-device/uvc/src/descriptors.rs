use alloc::vec::Vec;
use anyhow::anyhow;
use crab_usb::err::USBError;
use log::trace;

// UVC描述符解析和常量定义模块
// 参考libuvc的实现结构

/// UVC类特定请求常量 (A.8)
pub mod request_codes {
    pub const SET_CUR: u8 = 0x01;
    pub const GET_CUR: u8 = 0x81;
    pub const GET_MIN: u8 = 0x82;
    pub const GET_MAX: u8 = 0x83;
    pub const GET_RES: u8 = 0x84;
    pub const GET_LEN: u8 = 0x85;
    pub const GET_INFO: u8 = 0x86;
    pub const GET_DEF: u8 = 0x87;
}

/// UVC接口子类代码 (A.2)
pub mod interface_subclass {
    pub const UNDEFINED: u8 = 0x00;
    pub const VIDEO_CONTROL: u8 = 0x01;
    pub const VIDEO_STREAMING: u8 = 0x02;
    pub const VIDEO_INTERFACE_COLLECTION: u8 = 0x03;
}

/// UVC协议代码 (A.3)
pub mod protocol_codes {
    pub const UNDEFINED: u8 = 0x00;
}

/// VideoControl接口描述符子类型 (A.5)
pub mod vc_descriptor_subtypes {
    pub const UNDEFINED: u8 = 0x00;
    pub const HEADER: u8 = 0x01;
    pub const INPUT_TERMINAL: u8 = 0x02;
    pub const OUTPUT_TERMINAL: u8 = 0x03;
    pub const SELECTOR_UNIT: u8 = 0x04;
    pub const PROCESSING_UNIT: u8 = 0x05;
    pub const EXTENSION_UNIT: u8 = 0x06;
}

/// VideoStreaming接口描述符子类型 (A.6)
pub mod vs_descriptor_subtypes {
    pub const UNDEFINED: u8 = 0x00;
    pub const INPUT_HEADER: u8 = 0x01;
    pub const OUTPUT_HEADER: u8 = 0x02;
    pub const STILL_IMAGE_FRAME: u8 = 0x03;
    pub const FORMAT_UNCOMPRESSED: u8 = 0x04;
    pub const FRAME_UNCOMPRESSED: u8 = 0x05;
    pub const FORMAT_MJPEG: u8 = 0x06;
    pub const FRAME_MJPEG: u8 = 0x07;
    pub const FORMAT_MPEG2TS: u8 = 0x0A;
    pub const FORMAT_DV: u8 = 0x0C;
    pub const COLORFORMAT: u8 = 0x0D;
    pub const FORMAT_FRAME_BASED: u8 = 0x10;
    pub const FRAME_FRAME_BASED: u8 = 0x11;
    pub const FORMAT_STREAM_BASED: u8 = 0x12;
    pub const FORMAT_H264: u8 = 0x13;
    pub const FRAME_H264: u8 = 0x14;
    pub const FORMAT_H264_SIMULCAST: u8 = 0x15;
}

/// UVC描述符类型
pub mod descriptor_types {
    pub const DEVICE: u8 = 0x01;
    pub const CONFIGURATION: u8 = 0x02;
    pub const STRING: u8 = 0x03;
    pub const INTERFACE: u8 = 0x04;
    pub const ENDPOINT: u8 = 0x05;
    pub const CS_INTERFACE: u8 = 0x24;
    pub const CS_ENDPOINT: u8 = 0x25;
}

/// 摄像头终端控制选择器 (A.9.4)
pub mod camera_terminal_controls {
    pub const UNDEFINED: u8 = 0x00;
    pub const SCANNING_MODE: u8 = 0x01;
    pub const AE_MODE: u8 = 0x02;
    pub const AE_PRIORITY: u8 = 0x03;
    pub const EXPOSURE_TIME_ABSOLUTE: u8 = 0x04;
    pub const EXPOSURE_TIME_RELATIVE: u8 = 0x05;
    pub const FOCUS_ABSOLUTE: u8 = 0x06;
    pub const FOCUS_RELATIVE: u8 = 0x07;
    pub const FOCUS_AUTO: u8 = 0x08;
    pub const IRIS_ABSOLUTE: u8 = 0x09;
    pub const IRIS_RELATIVE: u8 = 0x0A;
    pub const ZOOM_ABSOLUTE: u8 = 0x0B;
    pub const ZOOM_RELATIVE: u8 = 0x0C;
    pub const PANTILT_ABSOLUTE: u8 = 0x0D;
    pub const PANTILT_RELATIVE: u8 = 0x0E;
    pub const ROLL_ABSOLUTE: u8 = 0x0F;
    pub const ROLL_RELATIVE: u8 = 0x10;
    pub const PRIVACY: u8 = 0x11;
    pub const FOCUS_SIMPLE: u8 = 0x12;
    pub const DIGITAL_WINDOW: u8 = 0x13;
    pub const REGION_OF_INTEREST: u8 = 0x14;
}

/// 处理单元控制选择器 (A.9.5)
pub mod processing_unit_controls {
    pub const UNDEFINED: u8 = 0x00;
    pub const BACKLIGHT_COMPENSATION: u8 = 0x01;
    pub const BRIGHTNESS: u8 = 0x02;
    pub const CONTRAST: u8 = 0x03;
    pub const GAIN: u8 = 0x04;
    pub const POWER_LINE_FREQUENCY: u8 = 0x05;
    pub const HUE: u8 = 0x06;
    pub const SATURATION: u8 = 0x07;
    pub const SHARPNESS: u8 = 0x08;
    pub const GAMMA: u8 = 0x09;
    pub const WHITE_BALANCE_TEMPERATURE: u8 = 0x0A;
    pub const WHITE_BALANCE_TEMPERATURE_AUTO: u8 = 0x0B;
    pub const WHITE_BALANCE_COMPONENT: u8 = 0x0C;
    pub const WHITE_BALANCE_COMPONENT_AUTO: u8 = 0x0D;
    pub const DIGITAL_MULTIPLIER: u8 = 0x0E;
    pub const DIGITAL_MULTIPLIER_LIMIT: u8 = 0x0F;
    pub const HUE_AUTO: u8 = 0x10;
    pub const ANALOG_VIDEO_STANDARD: u8 = 0x11;
    pub const ANALOG_LOCK_STATUS: u8 = 0x12;
    pub const CONTRAST_AUTO: u8 = 0x13;
}

/// VideoStreaming接口控制选择器 (A.9.7)
pub mod video_streaming_controls {
    pub const UNDEFINED: u8 = 0x00;
    pub const PROBE: u8 = 0x01;
    pub const COMMIT: u8 = 0x02;
    pub const STILL_PROBE: u8 = 0x03;
    pub const STILL_COMMIT: u8 = 0x04;
    pub const STILL_IMAGE_TRIGGER: u8 = 0x05;
    pub const STREAM_ERROR_CODE: u8 = 0x06;
    pub const GENERATE_KEY_FRAME: u8 = 0x07;
    pub const UPDATE_FRAME_SEGMENT: u8 = 0x08;
    pub const SYNC_DELAY: u8 = 0x09;
}

/// 终端类型常量 (B.1-B.4)
pub mod terminal_types {
    // USB终端类型 (B.1)
    pub const TT_VENDOR_SPECIFIC: u16 = 0x0100;
    pub const TT_STREAMING: u16 = 0x0101;

    // 输入终端类型 (B.2)
    pub const ITT_VENDOR_SPECIFIC: u16 = 0x0200;
    pub const ITT_CAMERA: u16 = 0x0201;
    pub const ITT_MEDIA_TRANSPORT_INPUT: u16 = 0x0202;

    // 输出终端类型 (B.3)
    pub const OTT_VENDOR_SPECIFIC: u16 = 0x0300;
    pub const OTT_DISPLAY: u16 = 0x0301;
    pub const OTT_MEDIA_TRANSPORT_OUTPUT: u16 = 0x0302;

    // 外部终端类型 (B.4)
    pub const EXTERNAL_VENDOR_SPECIFIC: u16 = 0x0400;
    pub const COMPOSITE_CONNECTOR: u16 = 0x0401;
    pub const SVIDEO_CONNECTOR: u16 = 0x0402;
    pub const COMPONENT_CONNECTOR: u16 = 0x0403;
}

/// UVC格式GUID常量
pub mod format_guids {
    // YUY2 格式 GUID
    pub const YUY2: [u8; 16] = [
        0x59, 0x55, 0x59, 0x32, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b,
        0x71,
    ];

    // NV12 格式 GUID
    pub const NV12: [u8; 16] = [
        0x4e, 0x56, 0x31, 0x32, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b,
        0x71,
    ];

    // RGB24 格式 GUID (RGB3)
    pub const RGB24: [u8; 16] = [
        0x52, 0x47, 0x42, 0x33, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b,
        0x71,
    ];

    // UYVY 格式 GUID
    pub const UYVY: [u8; 16] = [
        0x55, 0x59, 0x56, 0x59, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b,
        0x71,
    ];

    // BGR24 格式 GUID (BGR3)
    pub const BGR24: [u8; 16] = [
        0x42, 0x47, 0x52, 0x33, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b,
        0x71,
    ];
}

/// 载荷头标志 (2.4.3.3)
pub mod payload_header_flags {
    pub const EOH: u8 = 1 << 7; // End of Header
    pub const ERR: u8 = 1 << 6; // Error
    pub const STI: u8 = 1 << 5; // Still Image
    pub const RES: u8 = 1 << 4; // Reserved
    pub const SCR: u8 = 1 << 3; // Source Clock Reference
    pub const PTS: u8 = 1 << 2; // Presentation Time Stamp
    pub const EOF: u8 = 1 << 1; // End of Frame
    pub const FID: u8 = 1 << 0; // Frame ID
}

/// 控制能力标志 (4.1.2)
pub mod control_capabilities {
    pub const GET: u8 = 1 << 0;
    pub const SET: u8 = 1 << 1;
    pub const DISABLED: u8 = 1 << 2;
    pub const AUTOUPDATE: u8 = 1 << 3;
    pub const ASYNCHRONOUS: u8 = 1 << 4;
}

/// UVC描述符解析器
pub struct DescriptorParser;

impl DescriptorParser {
    /// 创建新的描述符解析器实例
    pub fn new() -> Self {
        Self
    }

    /// 解析VideoControl头描述符
    pub fn parse_vc_header(&self, data: &[u8]) -> Result<VcHeaderDescriptor, USBError> {
        if data.len() < 12 {
            Err(anyhow!("VC header descriptor too short"))?;
        }

        let length = data[0] as usize;
        let descriptor_type = data[1];
        let descriptor_subtype = data[2];

        if descriptor_type != descriptor_types::CS_INTERFACE
            || descriptor_subtype != vc_descriptor_subtypes::HEADER
        {
            Err(anyhow!("Not a VC header descriptor"))?;
        }

        let bcd_uvc = u16::from_le_bytes([data[3], data[4]]);
        let total_length = u16::from_le_bytes([data[5], data[6]]);
        let clock_frequency = u32::from_le_bytes([data[7], data[8], data[9], data[10]]);
        let in_collection = data[11];

        trace!(
            "VC Header: UVC {}.{}, total_len={}, clock={} Hz, interfaces={}",
            bcd_uvc >> 8,
            bcd_uvc & 0xff,
            total_length,
            clock_frequency,
            in_collection
        );

        Ok(VcHeaderDescriptor {
            length,
            bcd_uvc,
            total_length,
            clock_frequency,
            in_collection,
        })
    }

    /// 解析输入终端描述符
    pub fn parse_input_terminal(&self, data: &[u8]) -> Result<InputTerminalDescriptor, USBError> {
        if data.len() < 15 {
            Err(anyhow!("Input terminal descriptor too short"))?;
        }

        let length = data[0] as usize;
        let terminal_id = data[3];
        let terminal_type = u16::from_le_bytes([data[4], data[5]]);
        let associated_terminal = data[6];

        trace!(
            "Input Terminal: ID={terminal_id}, type=0x{terminal_type:04x}, associated={associated_terminal}"
        );

        // 摄像头终端有额外字段
        if terminal_type == terminal_types::ITT_CAMERA && length >= 18 {
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
    pub fn parse_processing_unit(&self, data: &[u8]) -> Result<ProcessingUnitDescriptor, USBError> {
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
            "Processing Unit: ID={unit_id}, source={source_id}, max_mult={max_multiplier}, controls={controls:02x?}"
        );

        Ok(ProcessingUnitDescriptor {
            length,
            unit_id,
            source_id,
            max_multiplier,
            controls,
        })
    }

    /// 解析VideoStreaming输入头描述符
    pub fn parse_vs_input_header(&self, data: &[u8]) -> Result<VsInputHeaderDescriptor, USBError> {
        if data.len() < 13 {
            Err(anyhow!("VS input header descriptor too short"))?;
        }

        let length = data[0] as usize;
        let num_formats = data[3];
        let total_length = u16::from_le_bytes([data[4], data[5]]);
        let endpoint_address = data[6];
        let info = data[7];
        let terminal_link = data[8];
        let still_capture_method = data[9];
        let trigger_support = data[10];
        let trigger_usage = data[11];
        let controls_size = data[12] as usize;

        if length < 13 + controls_size * num_formats as usize {
            Err(anyhow!("VS input header format controls data incomplete"))?;
        }

        let format_controls = data[13..13 + controls_size * num_formats as usize].to_vec();

        trace!(
            "VS Input Header: formats={num_formats}, total_len={total_length}, endpoint=0x{endpoint_address:02x}, terminal={terminal_link}"
        );

        Ok(VsInputHeaderDescriptor {
            length,
            num_formats,
            total_length,
            endpoint_address,
            info,
            terminal_link,
            still_capture_method,
            trigger_support,
            trigger_usage,
            format_controls,
        })
    }

    /// 解析未压缩格式描述符
    pub fn parse_uncompressed_format(
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
            "Uncompressed Format: index={format_index}, frames={num_frame_descriptors}, GUID={guid:02x?}, bpp={bits_per_pixel}"
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

    /// 解析MJPEG格式描述符
    pub fn parse_mjpeg_format(&self, data: &[u8]) -> Result<MjpegFormatDescriptor, USBError> {
        if data.len() < 11 {
            Err(anyhow!("MJPEG format descriptor too short"))?;
        }

        let length = data[0] as usize;
        let format_index = data[3];
        let num_frame_descriptors = data[4];
        let flags = data[5];
        let default_frame_index = data[6];
        let aspect_ratio_x = data[7];
        let aspect_ratio_y = data[8];
        let interlace_flags = data[9];
        let copy_protect = data[10];

        trace!(
            "MJPEG Format: index={format_index}, frames={num_frame_descriptors}, flags=0x{flags:02x}"
        );

        Ok(MjpegFormatDescriptor {
            length,
            format_index,
            num_frame_descriptors,
            flags,
            default_frame_index,
            aspect_ratio_x,
            aspect_ratio_y,
            interlace_flags,
            copy_protect,
        })
    }

    /// 解析帧描述符
    pub fn parse_frame_descriptor(&self, data: &[u8]) -> Result<FrameDescriptor, USBError> {
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
            "Frame: {width}x{height}, bitrate={min_bit_rate}-{max_bit_rate}, buffer_size={max_video_frame_buffer_size}, interval={default_frame_interval}, type={frame_interval_type}"
        );

        // 解析帧间隔数据
        let mut frame_intervals = Vec::new();
        let mut pos = 26;

        match frame_interval_type {
            0 => {
                // 连续帧间隔
                if length >= pos + 12 {
                    let min_frame_interval = u32::from_le_bytes([
                        data[pos],
                        data[pos + 1],
                        data[pos + 2],
                        data[pos + 3],
                    ]);
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

                    frame_intervals =
                        vec![min_frame_interval, max_frame_interval, step_frame_interval];
                }
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

    /// 计算帧率（从帧间隔）
    pub fn interval_to_fps(interval: u32) -> u32 {
        if interval > 0 {
            10_000_000 / interval // 100ns单位转换为fps
        } else {
            0
        }
    }

    /// 计算帧间隔（从帧率）
    pub fn fps_to_interval(fps: u32) -> u32 {
        if fps > 0 {
            10_000_000 / fps // fps转换为100ns单位
        } else {
            0
        }
    }
}

/// VideoControl头描述符
#[derive(Debug, Clone)]
pub struct VcHeaderDescriptor {
    pub length: usize,
    pub bcd_uvc: u16,
    pub total_length: u16,
    pub clock_frequency: u32,
    pub in_collection: u8,
}

/// 输入终端描述符
#[derive(Debug, Clone)]
pub enum InputTerminalDescriptor {
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
pub struct ProcessingUnitDescriptor {
    pub length: usize,
    pub unit_id: u8,
    pub source_id: u8,
    pub max_multiplier: u16,
    pub controls: Vec<u8>,
}

/// VideoStreaming输入头描述符
#[derive(Debug, Clone)]
pub struct VsInputHeaderDescriptor {
    pub length: usize,
    pub num_formats: u8,
    pub total_length: u16,
    pub endpoint_address: u8,
    pub info: u8,
    pub terminal_link: u8,
    pub still_capture_method: u8,
    pub trigger_support: u8,
    pub trigger_usage: u8,
    pub format_controls: Vec<u8>,
}

/// 未压缩格式描述符
#[derive(Debug, Clone)]
pub struct UncompressedFormatDescriptor {
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

/// MJPEG格式描述符
#[derive(Debug, Clone)]
pub struct MjpegFormatDescriptor {
    pub length: usize,
    pub format_index: u8,
    pub num_frame_descriptors: u8,
    pub flags: u8,
    pub default_frame_index: u8,
    pub aspect_ratio_x: u8,
    pub aspect_ratio_y: u8,
    pub interlace_flags: u8,
    pub copy_protect: u8,
}

/// 帧描述符
#[derive(Debug, Clone)]
pub struct FrameDescriptor {
    pub length: usize,
    pub frame_index: u8,
    pub capabilities: u8,
    pub width: u16,
    pub height: u16,
    pub min_bit_rate: u32,
    pub max_bit_rate: u32,
    pub max_video_frame_buffer_size: u32,
    pub default_frame_interval: u32,
    pub frame_interval_type: u8,
    pub frame_intervals: Vec<u32>,
}

impl Default for DescriptorParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fps_conversion() {
        // 测试30fps
        let interval_30fps = 333333; // 100ns单位
        assert_eq!(DescriptorParser::interval_to_fps(interval_30fps), 30);
        assert_eq!(DescriptorParser::fps_to_interval(30), 333333);

        // 测试60fps
        let interval_60fps = 166666;
        assert_eq!(DescriptorParser::interval_to_fps(interval_60fps), 60);
        assert_eq!(DescriptorParser::fps_to_interval(60), 166666);
    }

    #[test]
    fn test_guid_constants() {
        // 确保GUID常量正确定义
        assert_eq!(format_guids::YUY2[0..4], [0x59, 0x55, 0x59, 0x32]);
        assert_eq!(format_guids::NV12[0..4], [0x4e, 0x56, 0x31, 0x32]);
        assert_eq!(format_guids::RGB24[0..4], [0x52, 0x47, 0x42, 0x33]);
    }
}
