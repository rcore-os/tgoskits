//! UVC VS PROBE and COMMIT wire format.

use alloc::vec;

use usb_if::err::USBError;

pub const STREAM_CONTROL_LEN: usize = 26;
pub const STREAM_CONTROL_MAX_LEN: usize = 48;

pub const fn stream_control_len(uvc_version: u16) -> usize {
    if uvc_version < 0x0110 {
        26
    } else if uvc_version < 0x0150 {
        34
    } else {
        48
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamControl {
    pub hint: u16,
    pub format_index: u8,
    pub frame_index: u8,
    pub frame_interval: u32,
    pub key_frame_rate: u16,
    pub p_frame_rate: u16,
    pub comp_quality: u16,
    pub comp_window_size: u16,
    pub delay: u16,
    pub max_video_frame_size: u32,
    pub max_payload_transfer_size: u32,
    /// Opaque version-specific fields returned by the device.
    pub extension: [u8; STREAM_CONTROL_MAX_LEN - STREAM_CONTROL_LEN],
    pub wire_len: usize,
}

impl StreamControl {
    pub fn to_bytes(&self) -> Result<alloc::vec::Vec<u8>, USBError> {
        if !matches!(self.wire_len, 26 | 34 | 48) {
            return Err(USBError::InvalidParameter);
        }
        let mut data = vec![0; self.wire_len];
        data[0..2].copy_from_slice(&self.hint.to_le_bytes());
        data[2] = self.format_index;
        data[3] = self.frame_index;
        data[4..8].copy_from_slice(&self.frame_interval.to_le_bytes());
        data[8..10].copy_from_slice(&self.key_frame_rate.to_le_bytes());
        data[10..12].copy_from_slice(&self.p_frame_rate.to_le_bytes());
        data[12..14].copy_from_slice(&self.comp_quality.to_le_bytes());
        data[14..16].copy_from_slice(&self.comp_window_size.to_le_bytes());
        data[16..18].copy_from_slice(&self.delay.to_le_bytes());
        data[18..22].copy_from_slice(&self.max_video_frame_size.to_le_bytes());
        data[22..26].copy_from_slice(&self.max_payload_transfer_size.to_le_bytes());
        data[STREAM_CONTROL_LEN..]
            .copy_from_slice(&self.extension[..self.wire_len - STREAM_CONTROL_LEN]);
        Ok(data)
    }

    pub fn parse(data: &[u8]) -> Result<Self, USBError> {
        if !matches!(data.len(), 26 | 34 | 48) {
            return Err(USBError::InvalidParameter);
        }
        let mut extension = [0; STREAM_CONTROL_MAX_LEN - STREAM_CONTROL_LEN];
        extension[..data.len() - STREAM_CONTROL_LEN].copy_from_slice(&data[STREAM_CONTROL_LEN..]);
        Ok(Self {
            hint: u16::from_le_bytes(data[0..2].try_into().unwrap()),
            format_index: data[2],
            frame_index: data[3],
            frame_interval: u32::from_le_bytes(data[4..8].try_into().unwrap()),
            key_frame_rate: u16::from_le_bytes(data[8..10].try_into().unwrap()),
            p_frame_rate: u16::from_le_bytes(data[10..12].try_into().unwrap()),
            comp_quality: u16::from_le_bytes(data[12..14].try_into().unwrap()),
            comp_window_size: u16::from_le_bytes(data[14..16].try_into().unwrap()),
            delay: u16::from_le_bytes(data[16..18].try_into().unwrap()),
            max_video_frame_size: u32::from_le_bytes(data[18..22].try_into().unwrap()),
            max_payload_transfer_size: u32::from_le_bytes(data[22..26].try_into().unwrap()),
            extension,
            wire_len: data.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_control_round_trip_and_short_read() {
        let control = StreamControl {
            hint: 0x1234,
            format_index: 2,
            frame_index: 3,
            frame_interval: 333_333,
            key_frame_rate: 4,
            p_frame_rate: 5,
            comp_quality: 6,
            comp_window_size: 7,
            delay: 8,
            max_video_frame_size: 614_400,
            max_payload_transfer_size: 3_072,
            extension: [0; STREAM_CONTROL_MAX_LEN - STREAM_CONTROL_LEN],
            wire_len: STREAM_CONTROL_LEN,
        };
        let bytes = control.to_bytes().unwrap();
        assert_eq!(StreamControl::parse(&bytes).unwrap(), control);
        assert!(StreamControl::parse(&bytes[..25]).is_err());

        for (version, length) in [(0x0100, 26), (0x0110, 34), (0x0150, 48)] {
            assert_eq!(stream_control_len(version), length);
            let mut extended = control;
            extended.wire_len = length;
            for (index, byte) in extended.extension.iter_mut().enumerate() {
                *byte = index as u8 + 1;
            }
            let wire = extended.to_bytes().unwrap();
            assert_eq!(wire.len(), length);
            assert_eq!(
                StreamControl::parse(&wire).unwrap().to_bytes().unwrap(),
                wire
            );
        }
    }
}
