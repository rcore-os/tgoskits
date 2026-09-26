use alloc::vec::Vec;

use anyhow::anyhow;
use crab_usb::{
    err::USBError,
    usb_if::{
        descriptor::{Class, DescriptorType, EndpointType},
        endpoint::EndpointAddress,
        transfer::Direction,
    },
};
use log::debug;
use uvc_if::descriptors::protocol_codes;

use crate::{
    AlternateSetting, DescriptorParser, FrameIntervals, InputTerminalDescriptor,
    UncompressedFormat, VideoFormat, VideoFormatType,
    controls::VcUnits,
    descriptors::{
        InputHeaderDescriptor, InterfaceSubclass, TerminalType, VcDescriptorSubtype,
        VsDescriptorSubtype,
    },
};

#[derive(Debug)]
pub(crate) struct UvcDeviceConfig {
    pub vc_iface_num: u8,
    pub vs_iface_num: u8,
    pub alt_settings: Vec<AlternateSetting>,
    pub formats: Vec<VideoFormat>,
    pub vc_units: VcUnits,
    pub uvc_version: u16,
}

fn handle_vs_block(
    blob: &[u8],
    parser: &DescriptorParser,
) -> Result<(Vec<VideoFormat>, usize), USBError> {
    use VsDescriptorSubtype::*;

    if blob.len() < 3 {
        return Err(anyhow!("VS block too short").into());
    }
    if blob[1] != DescriptorType::CLASS_SPECIFIC_INTERFACE.into() {
        return Err(anyhow!("VS block must start with CS_INTERFACE").into());
    }
    let first_subtype = VsDescriptorSubtype::from(blob[2]);
    match first_subtype {
        InputHeader => {}
        OutputHeader => {
            return Err(anyhow!("UVC OutputHeader (0x02) not supported for capture device").into());
        }
        _ => {
            return Err(anyhow!(
                "VS block must start with InputHeader, got 0x{:02x}",
                blob[2]
            )
            .into());
        }
    }
    // 解析 InputHeader（已校验 bLength/bNumFormats）
    let hdr_len = blob[0] as usize;
    if hdr_len == 0 || hdr_len > blob.len() {
        return Err(anyhow!("InputHeader length invalid").into());
    }
    let hdr = parser.parse_input_header(&blob[..hdr_len])?;
    let total = hdr.total_length as usize;
    let mut consumed = hdr_len;
    let header: Option<InputHeaderDescriptor> = Some(hdr.clone());
    let mut cur_format: Option<(u8, VideoFormatType)> = None;
    let mut format_count = 0usize;

    let mut out = Vec::new();
    // 若 wTotalLength 非 0，则块边界为 total；为 0 时以遇到非 CS_INTERFACE 为止
    let block_limit = if total != 0 {
        core::cmp::min(total, blob.len())
    } else {
        blob.len()
    };

    while consumed + 3 <= blob.len() && consumed < block_limit {
        let len = blob[consumed] as usize;
        if len < 3 || consumed + len > blob.len() {
            break;
        }
        if total != 0 && consumed + len > total {
            // 超出 wTotalLength，块结束
            break;
        }
        let dtype = blob[consumed + 1];
        if dtype != DescriptorType::CLASS_SPECIFIC_INTERFACE.into() {
            break;
        }
        let subtype = VsDescriptorSubtype::from(blob[consumed + 2]);
        let desc = &blob[consumed..consumed + len];

        match subtype {
            Undefined => {
                debug!("VS descriptor ignored: undefined subtype 0x00");
            }
            InputHeader => {
                return Err(anyhow!("duplicate VS InputHeader inside block").into());
            }
            OutputHeader => {
                return Err(
                    anyhow!("UVC OutputHeader inside VS InputHeader block not supported").into(),
                );
            }
            Colorformat => {
                debug!("VS Colorformat ignored");
            }
            FormatUncompressed | FormatMjpeg => {
                let header = header.as_ref().unwrap();
                format_count += 1;
                cur_format = None;
                if format_count > header.num_formats as usize || desc.len() < 4 || desc[3] == 0 {
                    return Err(anyhow!(
                        "VS Format count {} exceeds bNumFormats {}",
                        format_count,
                        header.num_formats
                    )
                    .into());
                }
                let format_type = match subtype {
                    FormatUncompressed => {
                        let uncomp_desc =
                            match DescriptorParser::new().parse_uncompressed_format(desc) {
                                Ok(d) => d,
                                Err(_) => {
                                    consumed += len;
                                    continue;
                                }
                            };
                        let Some(fmt) = UncompressedFormat::from_guid(&uncomp_desc.guid) else {
                            consumed += len;
                            continue;
                        };
                        VideoFormatType::Uncompressed(fmt)
                    }
                    FormatMjpeg => VideoFormatType::Mjpeg,
                    _ => unreachable!(),
                };
                cur_format = Some((desc[3], format_type));
            }
            FrameUncompressed | FrameMjpeg => {
                // 需已解析 Header 且已有 Format
                if header.is_none() {
                    return Err(anyhow!("VS Frame before InputHeader").into());
                }
                let Some((format_index, format_type)) = cur_format else {
                    // The current format may have an unsupported GUID.
                    consumed += len;
                    continue;
                };
                if !matches!(
                    (subtype, format_type),
                    (FrameUncompressed, VideoFormatType::Uncompressed(_))
                        | (FrameMjpeg, VideoFormatType::Mjpeg)
                ) {
                    consumed += len;
                    continue;
                }
                if let Ok(fd) = parse_frame_descriptor(desc, format_index, format_type) {
                    out.push(fd);
                }
            }
            StillImageFrame => {
                // Still-image capture is optional and does not affect video frames.
                debug!("VS StillImageFrame ignored");
            }
            FormatMpeg2Ts => {
                return Err(anyhow!("UVC VideoStreaming MPEG-2 TS format not supported").into());
            }
            FormatDv => {
                return Err(anyhow!("UVC VideoStreaming DV format not supported").into());
            }
            FormatFrameBased | FrameFrameBased => {
                return Err(anyhow!("UVC VideoStreaming Frame-Based format not supported").into());
            }
            FormatStreamBased => {
                return Err(anyhow!("UVC VideoStreaming Stream-Based format not supported").into());
            }
            FormatH264 | FrameH264 | FormatH264Simulcast => {
                return Err(anyhow!("UVC VideoStreaming H.264 format not supported").into());
            }
        }
        consumed += len;
    }

    Ok((out, consumed))
}

fn handle_vc_block(
    blob: &[u8],
    parser: &DescriptorParser,
) -> Result<(VcUnits, u16, usize), USBError> {
    use VcDescriptorSubtype::*;

    if blob.len() < 3 {
        return Err(anyhow!("VC block too short").into());
    }
    if blob[1] != DescriptorType::CLASS_SPECIFIC_INTERFACE.into() {
        return Err(anyhow!("VC block must start with CS_INTERFACE").into());
    }
    if VcDescriptorSubtype::from(blob[2]) != Header {
        return Err(anyhow!("VC block must start with Header 0x01").into());
    }

    let hdr_len = blob[0] as usize;
    if hdr_len == 0 || hdr_len > blob.len() {
        return Err(anyhow!("VC Header length invalid").into());
    }
    let hdr = parser.parse_vc_header(&blob[..hdr_len])?;
    let total = hdr.total_length as usize;
    let mut consumed = hdr_len;
    let mut units = VcUnits::default();

    let block_limit = if total != 0 {
        core::cmp::min(total, blob.len())
    } else {
        blob.len()
    };

    while consumed + 3 <= blob.len() && consumed < block_limit {
        let len = blob[consumed] as usize;
        if len < 3 || consumed + len > blob.len() {
            break;
        }
        if total != 0 && consumed + len > total {
            break;
        }
        let dtype = blob[consumed + 1];
        if dtype != DescriptorType::CLASS_SPECIFIC_INTERFACE.into() {
            break;
        }
        let subtype = VcDescriptorSubtype::from(blob[consumed + 2]);
        let desc = &blob[consumed..consumed + len];

        match subtype {
            Header => {
                return Err(anyhow!("duplicate VC Header inside block").into());
            }
            InputTerminal => {
                if let Ok(InputTerminalDescriptor::Camera {
                    terminal_id,
                    terminal_type,
                    controls,
                    ..
                }) = parser.parse_input_terminal(desc)
                    && terminal_type == TerminalType::IttCamera.into()
                {
                    units.camera_terminal_id = Some(terminal_id);
                    units.camera_controls = controls;
                }
            }
            OutputTerminal | SelectorUnit | ExtensionUnit => {
                debug!("VC descriptor ignored: subtype 0x{:02x}", subtype as u8);
            }
            ProcessingUnit => {
                if let Ok(pu) = parser.parse_processing_unit(desc) {
                    units.processing_unit_id = Some(pu.unit_id);
                    units.processing_controls = pu.controls;
                }
            }
            Undefined => {
                debug!(
                    "VC descriptor ignored: undefined subtype 0x{:02x}",
                    subtype as u8
                );
            }
        }
        consumed += len;
    }

    Ok((units, hdr.bcd_uvc, consumed))
}

/// Parse UVC device from descriptor blob.
pub(crate) fn parse_uvc_device(blob: &[u8]) -> Result<UvcDeviceConfig, USBError> {
    use InterfaceSubclass::*;

    let parser = DescriptorParser::new();

    let mut vc_iface_num = None;
    let mut vs_iface_num = None;
    let mut alt_settings = Vec::new();
    let mut formats = Vec::new();
    let mut vc_units = VcUnits::default();
    let mut uvc_version = 0;

    let mut cur_iface: Option<(u8, u8, u8, u8, u8)> = None;
    let mut vc_parsed = false;
    let mut vs_parsed = false;

    let mut pos = 18usize;
    while pos + 2 <= blob.len() {
        let length = blob[pos] as usize;
        let descriptor_type = blob[pos + 1];
        if length < 2 || pos + length > blob.len() {
            break;
        }
        let desc = &blob[pos..pos + length];

        match DescriptorType::from(descriptor_type) {
            DescriptorType::INTERFACE if length >= 9 => {
                let (number, alternate, class, subclass, protocol) =
                    (desc[2], desc[3], desc[5], desc[6], desc[7]);
                if matches!(
                    Class::from_class_and_subclass(class, subclass, protocol),
                    Class::Video
                ) {
                    match InterfaceSubclass::from(subclass) {
                        VideoControl => vc_iface_num = Some(number),
                        VideoStreaming => vs_iface_num = Some(number),
                        InterfaceSubclass::Undefined
                        | InterfaceSubclass::VideoInterfaceCollection => {
                            return Err(anyhow!(
                                "Unsupported UVC interface subclass 0x{subclass:02x} on interface \
                                 {number}"
                            )
                            .into());
                        }
                    }
                }
                cur_iface = Some((number, alternate, class, subclass, protocol));
            }
            DescriptorType::ENDPOINT if length >= 7 => {
                if let Some((number, alternate, class, subclass, protocol)) = cur_iface
                    && matches!(
                        Class::from_class_and_subclass(class, subclass, protocol),
                        Class::Video
                    )
                    && InterfaceSubclass::from(subclass) == VideoStreaming
                    && protocol == protocol_codes::UNDEFINED
                    && vs_iface_num == Some(number)
                    && desc[3] & 0x03 == EndpointType::Isochronous as u8
                    && EndpointAddress::new(desc[2]).direction() == Direction::In
                {
                    let mps_raw = u16::from_le_bytes([desc[4], desc[5]]);
                    alt_settings.push(AlternateSetting {
                        alt_setting: alternate,
                        ep: desc[2],
                        mps: mps_raw & 0x7FF,
                        packets_per_uframe: ((mps_raw >> 11) & 0b11) as usize + 1,
                        interval: desc[6],
                    });
                }
            }
            DescriptorType::CLASS_SPECIFIC_INTERFACE if length >= 3 => {
                let subtype = desc[2];
                match cur_iface.map(|(_, _, class, subclass, protocol)| {
                    (
                        Class::from_class_and_subclass(class, subclass, protocol),
                        InterfaceSubclass::from(subclass),
                    )
                }) {
                    Some((Class::Video, VideoControl)) => {
                        let vc_subtype = VcDescriptorSubtype::from(subtype);
                        match vc_subtype {
                            VcDescriptorSubtype::Header => {
                                if vc_parsed {
                                    return Err(anyhow!("duplicate VC Header").into());
                                }
                                let (produced, version, consumed) =
                                    handle_vc_block(&blob[pos..], &parser)?;
                                vc_units = produced;
                                uvc_version = version;
                                vc_parsed = true;
                                pos += consumed;
                                continue;
                            }
                            VcDescriptorSubtype::Undefined => {
                                debug!("VC descriptor ignored: undefined subtype 0x{subtype:02x}");
                            }
                            _ => {
                                if !vc_parsed {
                                    return Err(anyhow!(
                                        "VC descriptor 0x{subtype:02x} before Header"
                                    )
                                    .into());
                                }
                                return Err(anyhow!(
                                    "VC descriptor 0x{subtype:02x} outside Header block"
                                )
                                .into());
                            }
                        }
                    }
                    Some((Class::Video, VideoStreaming)) => {
                        let vs_subtype = VsDescriptorSubtype::from(subtype);
                        match vs_subtype {
                            VsDescriptorSubtype::InputHeader => {
                                if vs_parsed {
                                    return Err(anyhow!("duplicate VS InputHeader").into());
                                }
                                let (produced, consumed) = handle_vs_block(&blob[pos..], &parser)?;
                                vs_parsed = true;
                                formats.extend(produced);
                                pos += consumed;
                                continue;
                            }
                            VsDescriptorSubtype::OutputHeader => {
                                return Err(anyhow!(
                                    "UVC OutputHeader (0x02) not supported for capture device"
                                )
                                .into());
                            }
                            VsDescriptorSubtype::Undefined => {
                                debug!("VS descriptor ignored: undefined subtype 0x00");
                            }
                            _ => {
                                // 连续块外出现的 VS 描述符视为非法（正常应由 InputHeader 块内消耗）
                                if !vs_parsed {
                                    return Err(anyhow!(
                                        "VS descriptor 0x{subtype:02x} before InputHeader"
                                    )
                                    .into());
                                }
                                return Err(anyhow!(
                                    "VS descriptor 0x{subtype:02x} outside InputHeader block"
                                )
                                .into());
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }

        pos += length;
    }

    // 健康性检查
    if !vc_parsed {
        return Err(anyhow!("UVC VC Header not found").into());
    }
    if !vs_parsed {
        return Err(anyhow!("UVC VS InputHeader not found").into());
    }
    if alt_settings.is_empty() {
        return Err(anyhow!("UVC VideoStreaming alternate settings not found").into());
    }
    if formats.is_empty() {
        return Err(anyhow!("UVC VideoStreaming formats not found").into());
    }
    match (vc_iface_num, vs_iface_num) {
        (Some(vc_iface_num), Some(vs_iface_num)) => Ok(UvcDeviceConfig {
            vc_iface_num,
            vs_iface_num,
            alt_settings,
            formats,
            vc_units,
            uvc_version,
        }),
        (None, _) => Err(anyhow!("UVC VideoControl interface not found").into()),
        (_, None) => Err(anyhow!("UVC VideoStreaming interface not found").into()),
    }
}

/// Parse frame descriptor.
fn parse_frame_descriptor(
    data: &[u8],
    format_index: u8,
    format_type: VideoFormatType,
) -> Result<VideoFormat, USBError> {
    match DescriptorParser::new().parse_frame_descriptor(data) {
        Ok(frame_desc) => {
            let intervals = if frame_desc.frame_interval_type == 0 {
                if frame_desc.frame_intervals.len() >= 3 {
                    if frame_desc.frame_intervals[0] == 0
                        || frame_desc.frame_intervals[1] < frame_desc.frame_intervals[0]
                        || frame_desc.frame_intervals[2] == 0
                    {
                        return Err(USBError::InvalidParameter);
                    }
                    FrameIntervals::Continuous {
                        min: frame_desc.frame_intervals[0],
                        max: frame_desc.frame_intervals[1],
                        step: frame_desc.frame_intervals[2],
                    }
                } else {
                    FrameIntervals::Discrete(frame_desc.frame_intervals.clone())
                }
            } else {
                FrameIntervals::Discrete(frame_desc.frame_intervals.clone())
            };
            if matches!(&intervals, FrameIntervals::Discrete(values) if values.contains(&0)) {
                return Err(USBError::InvalidParameter);
            }
            let pixels = u32::from(frame_desc.width)
                .checked_mul(u32::from(frame_desc.height))
                .filter(|pixels| *pixels != 0)
                .ok_or(USBError::InvalidParameter)?;
            let max_frame_size = match format_type {
                VideoFormatType::Uncompressed(t) => match t {
                    UncompressedFormat::Yuyv | UncompressedFormat::Uyvy => pixels.checked_mul(2),
                    UncompressedFormat::Nv12 => pixels.checked_mul(3).map(|bytes| bytes / 2),
                    UncompressedFormat::Grey => Some(pixels),
                    UncompressedFormat::Bgr24 => pixels.checked_mul(3),
                    UncompressedFormat::Xbgr32 => pixels.checked_mul(4),
                }
                .ok_or(USBError::InvalidParameter)?,
                VideoFormatType::Mjpeg => {
                    let v = frame_desc.max_video_frame_buffer_size;
                    if v != 0 {
                        v
                    } else {
                        // UVC 1.1+ 的 frame-based/无效描述符回退
                        pixels.checked_mul(2).ok_or(USBError::InvalidParameter)?
                    }
                }
            };
            let video_format = VideoFormat {
                format_type,
                format_index,
                frame_index: frame_desc.frame_index,
                width: frame_desc.width,
                height: frame_desc.height,
                default_interval: frame_desc.default_frame_interval,
                intervals,
                max_frame_size,
            };

            Ok(video_format)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
pub(crate) mod test_util {
    use alloc::vec::Vec;

    /// Build UVC blob for tests.
    pub(crate) fn build_uvc_blob(vc_subclass: u8, vs_subclass: u8) -> Vec<u8> {
        let mut blob = Vec::new();
        blob.extend_from_slice(&[18, 0x01]);
        blob.extend_from_slice(&0x0200u16.to_le_bytes());
        blob.extend_from_slice(&[0xEF, 0x02, 0x01, 64]);
        blob.extend_from_slice(&0x1234u16.to_le_bytes());
        blob.extend_from_slice(&0x5678u16.to_le_bytes());
        blob.extend_from_slice(&0x0100u16.to_le_bytes());
        blob.extend_from_slice(&[0, 0, 0, 1]);

        let mut config = Vec::new();
        config.extend_from_slice(&[9, 0x04, 0, 0, 0, 0x0E, vc_subclass, 0x00, 0]);
        config.extend_from_slice(&[12, 0x24, 0x01, 0, 1, 0, 0, 0, 0, 0, 0, 1]);
        config.extend_from_slice(&[
            18, 0x24, 0x02, 1, 0x01, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0x02, 0, 0,
        ]);
        config.extend_from_slice(&[10, 0x24, 0x05, 2, 1, 0, 0, 1, 0x04, 0]);
        config.extend_from_slice(&[9, 0x04, 1, 0, 0, 0x03, 0x00, 0x00, 0]);
        config.extend_from_slice(&[9, 0x04, 3, 0, 0, 0x0E, vs_subclass, 0x00, 0]);
        config.extend_from_slice(&[13, 0x24, 0x01, 1, 0, 0, 0x81, 0, 3, 0, 0, 0, 0]);
        config.extend_from_slice(&[
            27, 0x24, 0x04, 1, 1, // FormatUncompressed
            0x59, 0x55, 0x59, 0x32, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38,
            0x9b, 0x71, // YUY2 GUID
            16, 1, 0, 0, 0, 0,
        ]);
        config.extend_from_slice(&[
            26, 0x24, 0x05, 1, 0, // FrameUncompressed
            0x80, 0x02, 0xE0, 0x01, // 640x480
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // bitrate / buffer size
            0x15, 0x16, 0x05, 0x00, // 333333 (30fps)
            0,
        ]);
        config.extend_from_slice(&[9, 0x04, 3, 1, 1, 0x0E, vs_subclass, 0x00, 0]);
        config.extend_from_slice(&[7, 0x05, 0x81, 0x05, 0x10, 0x00, 1]);
        config.extend_from_slice(&[9, 0x04, 3, 2, 1, 0x0E, vs_subclass, 0x00, 0]);
        config.extend_from_slice(&[7, 0x05, 0x81, 0x05, 0x00, 0x04, 1]);

        let total_length = (9 + config.len()) as u16;
        blob.push(9);
        blob.push(0x02);
        blob.extend_from_slice(&total_length.to_le_bytes());
        blob.extend_from_slice(&[3, 1, 0, 0x80, 50]);
        blob.extend_from_slice(&config);
        blob
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::{test_util::build_uvc_blob, *};

    #[test]
    fn parse_uvc_device_single_pass_extracts_all() {
        let mut blob = build_uvc_blob(0x01, 0x02);
        let still_image = [6, 0x24, 0x03, 0, 0, 0];
        let next_interface = blob
            .windows(9)
            .position(|descriptor| descriptor == [9, 0x04, 3, 1, 1, 0x0E, 0x02, 0, 0])
            .unwrap();
        blob.splice(next_interface..next_interface, still_image);
        let cfg = parse_uvc_device(&blob).unwrap();

        assert_eq!(cfg.vc_iface_num, 0);
        assert_eq!(cfg.vs_iface_num, 3);

        assert_eq!(cfg.alt_settings.len(), 2);
        let alt = &cfg.alt_settings[0];
        assert_eq!(alt.alt_setting, 1);
        assert_eq!(alt.ep, 0x81);
        assert_eq!(alt.mps, 0x10);
        assert_eq!(alt.packets_per_uframe, 1);
        assert_eq!(alt.interval, 1);
        let alt = &cfg.alt_settings[1];
        assert_eq!(alt.alt_setting, 2);
        assert_eq!(alt.mps, 0x400);

        assert_eq!(cfg.formats.len(), 1);
        let fmt = &cfg.formats[0];
        assert_eq!(fmt.format_index, 1);
        assert_eq!(fmt.frame_index, 1);
        assert_eq!(fmt.width, 640);
        assert_eq!(fmt.height, 480);
        assert_eq!(fmt.frame_rate(), 30);
        assert_eq!(fmt.default_interval, 333_333);
        assert_eq!(
            fmt.format_type,
            VideoFormatType::Uncompressed(UncompressedFormat::Yuyv)
        );

        assert_eq!(cfg.vc_units.camera_terminal_id, Some(1));
        assert_eq!(cfg.vc_units.camera_controls, vec![0x02]);
        assert_eq!(cfg.vc_units.processing_unit_id, Some(2));
        assert_eq!(cfg.vc_units.processing_controls, vec![0x04]);

        // A device-level class of zero delegates classification to its interfaces.
        let mut interface_class_device = blob;
        interface_class_device[4..7].fill(0);
        assert!(parse_uvc_device(&interface_class_device).is_ok());
    }

    #[test]
    fn unsupported_format_does_not_reindex_or_capture_its_frames() {
        let mut blob = build_uvc_blob(0x01, 0x02);
        let header = blob
            .windows(4)
            .position(|bytes| bytes == [13, 0x24, 0x01, 1])
            .unwrap();
        blob[header + 3] = 3;
        let format = blob
            .windows(5)
            .position(|bytes| bytes == [27, 0x24, 0x04, 1, 1])
            .unwrap();
        let frame = blob
            .windows(5)
            .position(|bytes| bytes == [26, 0x24, 0x05, 1, 0])
            .unwrap();
        let mut unknown_format = blob[format..format + 27].to_vec();
        unknown_format[3] = 2;
        unknown_format[5..21].fill(0);
        let mut unknown_frame = blob[frame..frame + 26].to_vec();
        unknown_frame[5..7].copy_from_slice(&800u16.to_le_bytes());
        let mut supported_format = blob[format..format + 27].to_vec();
        supported_format[3] = 3;
        let mut supported_frame = blob[frame..frame + 26].to_vec();
        supported_frame[5..7].copy_from_slice(&1280u16.to_le_bytes());
        let next_interface = blob
            .windows(9)
            .position(|bytes| bytes == [9, 0x04, 3, 1, 1, 0x0e, 0x02, 0, 0])
            .unwrap();
        blob.splice(
            next_interface..next_interface,
            unknown_format
                .into_iter()
                .chain(unknown_frame)
                .chain(supported_format)
                .chain(supported_frame),
        );
        let formats = parse_uvc_device(&blob).unwrap().formats;
        assert_eq!(formats.len(), 2);
        assert_eq!((formats[0].format_index, formats[0].width), (1, 640));
        assert_eq!((formats[1].format_index, formats[1].width), (3, 1280));
    }

    #[test]
    fn parse_uvc_device_missing_interface_errors() {
        // 当接口子类不支持时，parse_uvc_device 应直接报 Err，而不是等到健康检查
        let blob = build_uvc_blob(0x00, 0x02);
        assert!(
            parse_uvc_device(&blob)
                .unwrap_err()
                .to_string()
                .contains("Unsupported UVC interface subclass")
        );
        let blob = build_uvc_blob(0x01, 0x00);
        assert!(
            parse_uvc_device(&blob)
                .unwrap_err()
                .to_string()
                .contains("Unsupported UVC interface subclass")
        );
    }

    #[test]
    fn parse_uvc_device_no_config_errors() {
        let blob = vec![18, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert!(parse_uvc_device(&blob).is_err());
    }

    #[test]
    fn parse_uvc_device_missing_input_header_errors() {
        let mut blob = build_uvc_blob(0x01, 0x02);
        // 移除 VS InputHeader (13,0x24,0x01) — VC Header 为 12,0x24,0x01 以示区分
        if let Some(pos) = blob
            .windows(3)
            .position(|w| w == [13, 0x24, 0x01])
            // windows 找到的是 VC Header(12)之后的 VS InputHeader，但要确保是长度 13 的那个
            // 若首次命中是 VC Header 的变体（12,0x24,0x01），则找下一个
            .and_then(|p| {
                if blob[p] == 13 {
                    Some(p)
                } else {
                    blob[p + 1..]
                        .windows(3)
                        .position(|w| w == [13, 0x24, 0x01])
                        .map(|q| p + 1 + q)
                }
            })
        {
            blob.drain(pos..pos + 13);
            let total = (blob.len() - 18) as u16;
            blob[19] = (total & 0xFF) as u8;
            blob[20] = (total >> 8) as u8;
        }
        let err = parse_uvc_device(&blob).unwrap_err().to_string();
        assert!(
            err.contains("InputHeader"),
            "expected InputHeader error, got {err}"
        );
    }

    #[test]
    fn parse_uvc_device_output_header_errors() {
        let mut blob = build_uvc_blob(0x01, 0x02);
        if let Some(pos) = blob
            .windows(3)
            .position(|w| w == [13, 0x24, 0x01])
            .and_then(|p| {
                if blob[p] == 13 {
                    Some(p)
                } else {
                    blob[p + 1..]
                        .windows(3)
                        .position(|w| w == [13, 0x24, 0x01])
                        .map(|q| p + 1 + q)
                }
            })
        {
            // 替换为 OutputHeader: 9,0x24,0x02,1,0,0,0x81,3,0
            blob[pos] = 9;
            blob[pos + 2] = 0x02;
            blob[pos + 3] = 1;
            blob[pos + 4] = 0;
            blob[pos + 5] = 0;
            blob[pos + 6] = 0x81;
            blob[pos + 7] = 3;
            blob[pos + 8] = 0;
            blob.drain(pos + 9..pos + 13);
            let total = (blob.len() - 18) as u16;
            blob[19] = (total & 0xFF) as u8;
            blob[20] = (total >> 8) as u8;
        }
        let err = parse_uvc_device(&blob).unwrap_err().to_string();
        assert!(
            err.contains("OutputHeader"),
            "expected OutputHeader error, got {err}"
        );
    }

    #[test]
    fn parse_uvc_device_undefined_vs_ignored() {
        let mut blob = build_uvc_blob(0x01, 0x02);
        // 在 InputHeader 后插入一条 Undefined VS 描述符 (3,0x24,0x00)
        if let Some(pos) = blob
            .windows(3)
            .position(|w| w == [13, 0x24, 0x01])
            .and_then(|p| {
                if blob[p] == 13 {
                    Some(p)
                } else {
                    blob[p + 1..]
                        .windows(3)
                        .position(|w| w == [13, 0x24, 0x01])
                        .map(|q| p + 1 + q)
                }
            })
        {
            let insert_at = pos + 13;
            blob.splice(insert_at..insert_at, [3, 0x24, 0x00]);
            let total = (blob.len() - 18) as u16;
            blob[19] = (total & 0xFF) as u8;
            blob[20] = (total >> 8) as u8;
        }
        // Undefined 应被忽略，仍能正常解析
        assert!(parse_uvc_device(&blob).is_ok());
    }
}
