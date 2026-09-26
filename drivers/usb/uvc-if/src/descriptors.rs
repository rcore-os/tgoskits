use alloc::{vec, vec::Vec};

use anyhow::anyhow;
use bitflags::bitflags;
use log::trace;
use usb_if::err::USBError;

/// UVC class-specific request codes (UVC Table A.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RequestCode {
    SetCur  = 0x01,
    GetCur  = 0x81,
    GetMin  = 0x82,
    GetMax  = 0x83,
    GetRes  = 0x84,
    GetLen  = 0x85,
    GetInfo = 0x86,
    GetDef  = 0x87,
}

/// UVC interface subclasses (UVC Table A.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum InterfaceSubclass {
    Undefined      = 0x00,
    VideoControl   = 0x01,
    VideoStreaming = 0x02,
    VideoInterfaceCollection = 0x03,
}

/// UVC interface protocol codes (UVC Table A.3).
pub mod protocol_codes {
    pub const UNDEFINED: u8 = 0x00;
}

/// VideoControl descriptor subtypes (UVC Table A.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum VcDescriptorSubtype {
    Undefined      = 0x00,
    Header         = 0x01,
    InputTerminal  = 0x02,
    OutputTerminal = 0x03,
    SelectorUnit   = 0x04,
    ProcessingUnit = 0x05,
    ExtensionUnit  = 0x06,
}

/// VideoStreaming descriptor subtypes (UVC Table A.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum VsDescriptorSubtype {
    Undefined           = 0x00,
    InputHeader         = 0x01,
    OutputHeader        = 0x02,
    StillImageFrame     = 0x03,
    FormatUncompressed  = 0x04,
    FrameUncompressed   = 0x05,
    FormatMjpeg         = 0x06,
    FrameMjpeg          = 0x07,
    FormatMpeg2Ts       = 0x0A,
    FormatDv            = 0x0C,
    Colorformat         = 0x0D,
    FormatFrameBased    = 0x10,
    FrameFrameBased     = 0x11,
    FormatStreamBased   = 0x12,
    FormatH264          = 0x13,
    FrameH264           = 0x14,
    FormatH264Simulcast = 0x15,
}

/// VideoStreaming control selectors (UVC Table A.9.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum VideoStreamingControl {
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

/// UVC terminal types (UVC Tables B.1 through B.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum TerminalType {
    TtVendorSpecific   = 0x0100,
    TtStreaming        = 0x0101,
    IttVendorSpecific  = 0x0200,
    IttCamera          = 0x0201,
    IttMediaTransportInput = 0x0202,
    OttVendorSpecific  = 0x0300,
    OttDisplay         = 0x0301,
    OttMediaTransportOutput = 0x0302,
    ExternalVendorSpecific = 0x0400,
    CompositeConnector = 0x0401,
    SvideoConnector    = 0x0402,
    ComponentConnector = 0x0403,
}

/// Uncompressed video format GUIDs.
pub mod format_guids {
    pub const YUY2: [u8; 16] = [
        0x59, 0x55, 0x59, 0x32, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b,
        0x71,
    ];
    pub const NV12: [u8; 16] = [
        0x4e, 0x56, 0x31, 0x32, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b,
        0x71,
    ];
    pub const RGB24: [u8; 16] = [
        0x52, 0x47, 0x42, 0x33, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b,
        0x71,
    ];

    pub const UYVY: [u8; 16] = [
        0x55, 0x59, 0x56, 0x59, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b,
        0x71,
    ];
    pub const GREY: [u8; 16] = [
        0x59, 0x38, 0x30, 0x30, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b,
        0x71,
    ];
    pub const BGR24: [u8; 16] = [
        0x7d, 0xeb, 0x36, 0xe4, 0x4f, 0x52, 0xce, 0x11, 0x9f, 0x53, 0x00, 0x20, 0xaf, 0x0b, 0xa7,
        0x70,
    ];
    pub const BGR3: [u8; 16] = [
        0x42, 0x47, 0x52, 0x33, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b,
        0x71,
    ];
    pub const XBGR32: [u8; 16] = [
        0x7e, 0xeb, 0x36, 0xe4, 0x4f, 0x52, 0xce, 0x11, 0x9f, 0x53, 0x00, 0x20, 0xaf, 0x0b, 0xa7,
        0x70,
    ];
}

bitflags! {
    /// UVC control capability flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ControlCapabilities: u8 {
        const GET = 1 << 0;
        const SET = 1 << 1;
        const DISABLED = 1 << 2;
        const AUTOUPDATE = 1 << 3;
        const ASYNCHRONOUS = 1 << 4;
    }
}

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

impl From<TerminalType> for u16 {
    fn from(v: TerminalType) -> u16 {
        v as u16
    }
}

impl From<RequestCode> for usb_if::transfer::Request {
    fn from(v: RequestCode) -> Self {
        (v as u8).into()
    }
}

fn checked_descriptor(data: &[u8], minimum: usize) -> Result<&[u8], USBError> {
    let length = data.first().copied().unwrap_or(0) as usize;
    if length < minimum || length > data.len() {
        return Err(anyhow!(
            "UVC descriptor length {length} is invalid for {} received bytes (minimum {minimum})",
            data.len()
        )
        .into());
    }
    Ok(&data[..length])
}

/// Parser for class-specific UVC descriptors.
pub struct DescriptorParser;

impl DescriptorParser {
    pub fn new() -> Self {
        Self
    }

    pub fn parse_input_terminal(&self, data: &[u8]) -> Result<InputTerminalDescriptor, USBError> {
        let data = checked_descriptor(data, 15)?;

        let length = data[0] as usize;
        let terminal_id = data[3];
        let terminal_type = u16::from_le_bytes([data[4], data[5]]);
        let associated_terminal = data[6];

        trace!(
            "Input Terminal: ID={terminal_id}, type=0x{terminal_type:04x}, \
             associated={associated_terminal}"
        );

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

    pub fn parse_processing_unit(&self, data: &[u8]) -> Result<ProcessingUnitDescriptor, USBError> {
        let data = checked_descriptor(data, 10)?;

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

    pub fn parse_uncompressed_format(
        &self,
        data: &[u8],
    ) -> Result<UncompressedFormatDescriptor, USBError> {
        let data = checked_descriptor(data, 27)?;

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

    pub fn parse_frame_descriptor(&self, data: &[u8]) -> Result<FrameDescriptor, USBError> {
        let data = checked_descriptor(data, 26)?;

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

        let mut frame_intervals = Vec::new();
        let mut pos = 26;

        match frame_interval_type {
            0 if length >= pos + 12 => {
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

    pub fn parse_input_header(&self, data: &[u8]) -> Result<InputHeaderDescriptor, USBError> {
        let data = checked_descriptor(data, 13)?;
        let length = data.len();
        // The caller identifies the CS_INTERFACE input header.
        let num_formats = data[3];
        let total_length = u16::from_le_bytes([data[4], data[5]]);
        let endpoint_address = data[6];
        let info = data[7];
        let terminal_link = data[8];
        let still_capture_method = data[9];
        let trigger_support = data[10];
        let trigger_usage = data[11];
        let control_size = data[12] as usize;

        // bLength includes the control array for every advertised format.
        let expected = 13usize + control_size * num_formats as usize;
        if length < expected {
            Err(anyhow!(
                "InputHeader bLength {length} smaller than 13+p*n {expected}"
            ))?;
        }
        if num_formats == 0 {
            Err(anyhow!("InputHeader bNumFormats is 0"))?;
        }

        let controls = data[13..expected].to_vec();

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

    pub fn parse_output_header(&self, data: &[u8]) -> Result<OutputHeaderDescriptor, USBError> {
        let data = checked_descriptor(data, 9)?;
        let length = data.len();
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

        let controls = data[9..expected].to_vec();

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

    pub fn parse_vc_header(&self, data: &[u8]) -> Result<VcHeaderDescriptor, USBError> {
        let data = checked_descriptor(data, 12)?;
        let length = data.len();
        let bcd_uvc = u16::from_le_bytes([data[3], data[4]]);
        let total_length = u16::from_le_bytes([data[5], data[6]]);
        let clock_frequency = u32::from_le_bytes([data[7], data[8], data[9], data[10]]);
        let in_collection = data[11];
        // Some devices report bInCollection without including all interface numbers.
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

        Ok(VcHeaderDescriptor {
            length,
            bcd_uvc,
            total_length,
            clock_frequency,
            in_collection,
            interface_numbers,
        })
    }

    /// Convert a 100 ns frame interval to frames per second.
    pub fn interval_to_fps(interval: u32) -> u32 {
        10_000_000u32.checked_div(interval).unwrap_or(0)
    }

    /// Convert frames per second to a 100 ns frame interval.
    pub fn fps_to_interval(fps: u32) -> u32 {
        10_000_000u32.checked_div(fps).unwrap_or(0)
    }
}

/// Parsed VideoControl input terminal.
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

/// Parsed VideoControl processing unit.
#[derive(Debug, Clone)]
pub struct ProcessingUnitDescriptor {
    pub length: usize,
    pub unit_id: u8,
    pub source_id: u8,
    pub max_multiplier: u16,
    pub controls: Vec<u8>,
}

/// Parsed uncompressed format descriptor.
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

/// Parsed video frame descriptor and supported intervals.
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

/// Parsed VideoStreaming input header.
#[derive(Debug, Clone)]
pub struct InputHeaderDescriptor {
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

/// Parsed VideoStreaming output header.
#[derive(Debug, Clone)]
pub struct OutputHeaderDescriptor {
    pub length: usize,
    pub num_formats: u8,
    pub total_length: u16,
    pub endpoint_address: u8,
    pub terminal_link: u8,
    pub control_size: u8,
    pub controls: Vec<u8>,
}

/// Parsed VideoControl header.
#[derive(Debug, Clone)]
pub struct VcHeaderDescriptor {
    pub length: usize,
    pub bcd_uvc: u16,
    pub total_length: u16,
    pub clock_frequency: u32,
    pub in_collection: u8,
    pub interface_numbers: Vec<u8>,
}

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

impl Default for DescriptorParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claimed_length_must_fit_received_descriptor() {
        let truncated_camera_terminal = [18, 0x24, 0x02, 1, 0x01, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 3];
        assert!(
            DescriptorParser::new()
                .parse_input_terminal(&truncated_camera_terminal)
                .is_err()
        );
    }
}
