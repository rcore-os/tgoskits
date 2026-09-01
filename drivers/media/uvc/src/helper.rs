use alloc::vec::Vec;

use anyhow::anyhow;
use crab_usb::err::USBError;
use log::debug;

use crate::{
    AlternateSetting, DescriptorParser, InputTerminalDescriptor, StreamControl, UncompressedFormat,
    VideoFormat, VideoFormatType,
    controls::VcUnits,
    descriptors::{
        DescriptorType, InterfaceSubclass, TerminalType, VcDescriptorSubtype, VsDescriptorSubtype,
    },
};

/// Serialize stream control.
pub(crate) fn serialize_stream_control(ctrl: &StreamControl) -> Vec<u8> {
    let mut data = Vec::with_capacity(26);

    data.extend(&ctrl.hint.to_le_bytes());
    data.push(ctrl.format_index);
    data.push(ctrl.frame_index);
    data.extend(&ctrl.frame_interval.to_le_bytes());
    data.extend(&ctrl.key_frame_rate.to_le_bytes());
    data.extend(&ctrl.p_frame_rate.to_le_bytes());
    data.extend(&ctrl.comp_quality.to_le_bytes());
    data.extend(&ctrl.comp_window_size.to_le_bytes());
    data.extend(&ctrl.delay.to_le_bytes());
    data.extend(&ctrl.max_video_frame_size.to_le_bytes());
    data.extend(&ctrl.max_payload_transfer_size.to_le_bytes());

    debug!("Serialized stream control: {} bytes", data.len());
    data
}

/// Parse stream control.
pub(crate) fn parse_stream_control(data: &[u8]) -> Result<StreamControl, USBError> {
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
    let max_payload_transfer_size = u32::from_le_bytes([data[22], data[23], data[24], data[25]]);

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

#[derive(Debug)]
pub(crate) struct UvcDeviceConfig {
    pub vc_iface_num: u8,
    pub vs_iface_num: u8,
    pub alt_settings: Vec<AlternateSetting>,
    pub formats: Vec<VideoFormat>,
    pub vc_units: VcUnits,
}

/// Parse UVC device from descriptor blob.
pub(crate) fn parse_uvc_device(blob: &[u8]) -> Result<UvcDeviceConfig, USBError> {
    use VcDescriptorSubtype::*;
    use VsDescriptorSubtype::*;

    let parser = DescriptorParser::new();

    let mut vc_iface_num = None;
    let mut vs_iface_num = None;
    let mut alt_settings = Vec::new();
    let mut formats = Vec::new();
    let mut vc_units = VcUnits::default();

    let mut cur_iface: Option<(u8, u8, u8, u8, u8)> = None;
    let mut cur_format: Option<(u8, VideoFormatType)> = None;

    let mut pos = 18usize;
    while pos + 2 <= blob.len() {
        let length = blob[pos] as usize;
        let descriptor_type = blob[pos + 1];
        if length < 2 || pos + length > blob.len() {
            break;
        }
        let desc = &blob[pos..pos + length];

        match DescriptorType::from(descriptor_type) {
            DescriptorType::Interface if length >= 9 => {
                let (number, alternate, class, subclass, protocol) =
                    (desc[2], desc[3], desc[5], desc[6], desc[7]);
                if class == 0x0E {
                    match InterfaceSubclass::from(subclass) {
                        InterfaceSubclass::VideoControl => vc_iface_num = Some(number),
                        InterfaceSubclass::VideoStreaming => vs_iface_num = Some(number),
                        _ => {}
                    }
                }
                if !(class == 0x0E
                    && InterfaceSubclass::from(subclass) == InterfaceSubclass::VideoStreaming
                    && vs_iface_num == Some(number))
                {
                    cur_format = None;
                }
                cur_iface = Some((number, alternate, class, subclass, protocol));
            }
            DescriptorType::Endpoint if length >= 7 => {
                if let Some((number, alternate, class, subclass, protocol)) = cur_iface
                    && class == 0x0E
                    && InterfaceSubclass::from(subclass) == InterfaceSubclass::VideoStreaming
                    && protocol == 0x00
                    && vs_iface_num == Some(number)
                    && desc[3] & 0x03 == 0x01
                    && desc[2] & 0x80 != 0
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
            DescriptorType::CsInterface if length >= 3 => {
                let subtype = desc[2];
                match cur_iface
                    .map(|(_, _, class, subclass, _)| (class, InterfaceSubclass::from(subclass)))
                {
                    Some((0x0E, InterfaceSubclass::VideoControl)) => {
                        match VcDescriptorSubtype::from(subtype) {
                            InputTerminal => {
                                if let Ok(InputTerminalDescriptor::Camera {
                                    terminal_id,
                                    terminal_type,
                                    controls,
                                    ..
                                }) = parser.parse_input_terminal(desc)
                                    && terminal_type == TerminalType::IttCamera.into()
                                {
                                    vc_units.camera_terminal_id = Some(terminal_id);
                                    vc_units.camera_controls = controls;
                                }
                            }
                            ProcessingUnit => {
                                if let Ok(pu) = parser.parse_processing_unit(desc) {
                                    vc_units.processing_unit_id = Some(pu.unit_id);
                                    vc_units.processing_controls = pu.controls;
                                }
                            }
                            _ => {}
                        }
                    }
                    Some((0x0E, InterfaceSubclass::VideoStreaming)) => {
                        match VsDescriptorSubtype::from(subtype) {
                            FormatUncompressed | FormatMjpeg | FormatH264 => {
                                let format_type = match VsDescriptorSubtype::from(subtype) {
                                    VsDescriptorSubtype::FormatUncompressed => {
                                        if let Ok(format_type) =
                                            parse_uncompressed_format_type(desc)
                                        {
                                            VideoFormatType::Uncompressed(format_type)
                                        } else {
                                            debug!(
                                                "Failed to parse uncompressed format descriptor"
                                            );
                                            pos += length;
                                            continue;
                                        }
                                    }
                                    VsDescriptorSubtype::FormatMjpeg => VideoFormatType::Mjpeg,
                                    VsDescriptorSubtype::FormatH264 => VideoFormatType::H264,
                                    _ => unreachable!(),
                                };
                                let format_index =
                                    cur_format.map_or(0, |(index, _)| index).wrapping_add(1);
                                cur_format = Some((format_index, format_type));
                            }
                            FrameMjpeg | FrameUncompressed => {
                                let format_index = cur_format.map_or(0, |(index, _)| index);
                                let format_type = cur_format
                                    .map_or(VideoFormatType::Mjpeg, |(_, format_type)| format_type);
                                if let Ok(frame_descriptor) =
                                    parse_frame_descriptor(desc, format_index, format_type)
                                {
                                    formats.push(frame_descriptor);
                                }
                            }
                            _ => {
                                debug!("Unknown VS descriptor subtype: 0x{subtype:02x}");
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

    match (vc_iface_num, vs_iface_num) {
        (Some(vc_iface_num), Some(vs_iface_num)) => Ok(UvcDeviceConfig {
            vc_iface_num,
            vs_iface_num,
            alt_settings,
            formats,
            vc_units,
        }),
        (None, _) => Err(anyhow!("UVC VideoControl interface not found").into()),
        (_, None) => Err(anyhow!("UVC VideoStreaming interface not found").into()),
    }
}

/// Parse uncompressed format type.
fn parse_uncompressed_format_type(data: &[u8]) -> Result<UncompressedFormat, USBError> {
    let desc = DescriptorParser::new().parse_uncompressed_format(data)?;
    let fmt = UncompressedFormat::from_guid(&desc.guid).ok_or_else(|| {
        debug!("Unknown uncompressed GUID {:02x?}", desc.guid);
        USBError::from(anyhow!("Unsupported uncompressed GUID"))
    })?;
    debug!("Detected uncompressed {fmt:?} GUID {:02x?}", desc.guid);
    Ok(fmt)
}

/// Parse frame descriptor.
fn parse_frame_descriptor(
    data: &[u8],
    format_index: u8,
    format_type: VideoFormatType,
) -> Result<VideoFormat, USBError> {
    match DescriptorParser::new().parse_frame_descriptor(data) {
        Ok(frame_desc) => {
            let default_frame_rate =
                DescriptorParser::interval_to_fps(frame_desc.default_frame_interval);

            let video_format = VideoFormat {
                format_type,
                format_index,
                frame_index: frame_desc.frame_index,
                width: frame_desc.width,
                height: frame_desc.height,
                frame_rate: default_frame_rate,
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
        let blob = build_uvc_blob(0x01, 0x02);
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
        assert_eq!(fmt.frame_rate, 30);
        assert_eq!(
            fmt.format_type,
            VideoFormatType::Uncompressed(UncompressedFormat::Yuyv)
        );

        assert_eq!(cfg.vc_units.camera_terminal_id, Some(1));
        assert_eq!(cfg.vc_units.camera_controls, vec![0x02]);
        assert_eq!(cfg.vc_units.processing_unit_id, Some(2));
        assert_eq!(cfg.vc_units.processing_controls, vec![0x04]);
    }

    #[test]
    fn parse_uvc_device_missing_interface_errors() {
        let blob = build_uvc_blob(0x00, 0x02);
        assert!(
            parse_uvc_device(&blob)
                .unwrap_err()
                .to_string()
                .contains("VideoControl")
        );
        let blob = build_uvc_blob(0x01, 0x00);
        assert!(
            parse_uvc_device(&blob)
                .unwrap_err()
                .to_string()
                .contains("VideoStreaming")
        );
    }

    #[test]
    fn parse_uvc_device_no_config_errors() {
        let blob = vec![18, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert!(parse_uvc_device(&blob).is_err());
    }
}
