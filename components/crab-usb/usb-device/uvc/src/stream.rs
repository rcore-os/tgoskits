use alloc::vec::Vec;
use crab_usb::Endpoint;
use log::debug;
use usb_if::{descriptor::EndpointDescriptor, endpoint::TransferRequest, err::USBError};

use crate::{
    VideoFormat,
    frame::{FrameEvent, FrameParser},
};

pub struct VideoStream {
    ep: Endpoint,
    frame_parser: FrameParser,
    pub vedio_format: VideoFormat,
    packets_per_transfer: usize,
    packet_size: usize,
    buffer: Vec<u8>,
}

unsafe impl Send for VideoStream {}

impl VideoStream {
    pub fn new(ep: Endpoint, desc: EndpointDescriptor, vfmt: VideoFormat) -> Self {
        let max_packet_size = desc.max_packet_size;
        // 参考libusb计算逻辑:
        // packets_per_transfer = (dwMaxVideoFrameSize + endpoint_bytes_per_packet - 1) / endpoint_bytes_per_packet
        // 但保持合理的限制(最多32个包)
        let packets_per_transfer =
            core::cmp::min(vfmt.frame_bytes().div_ceil(max_packet_size as _), 32);
        let buffer = vec![0u8; (max_packet_size as usize) * packets_per_transfer];
        debug!(
            "VideoStream created: max_packet_size={}, packets_per_transfer={}, buffer_size={}",
            max_packet_size,
            packets_per_transfer,
            buffer.len()
        );
        VideoStream {
            ep,

            frame_parser: FrameParser::new(vfmt.frame_bytes()),
            vedio_format: vfmt,
            packets_per_transfer,
            buffer,
            packet_size: max_packet_size as usize,
        }
    }

    pub async fn recv(&mut self) -> Result<Vec<FrameEvent>, USBError> {
        self.buffer.fill(0);

        let packet_lengths = alloc::vec![self.packet_size; self.packets_per_transfer];
        self.ep
            .wait(TransferRequest::iso_in(&mut self.buffer, &packet_lengths))
            .await?;

        let mut events = Vec::new();

        for data in self.buffer.chunks(self.packet_size) {
            if data.iter().all(|&b| b == 0) {
                // 空包，跳过
                continue;
            }
            if let Ok(Some(one)) = self.frame_parser.push_packet(data) {
                events.push(one);
            }
        }

        Ok(events)
    }

    /// 获取错误包统计信息
    pub fn error_packet_count(&self) -> u32 {
        self.frame_parser.error_packet_count()
    }

    /// 重置错误包统计
    pub fn reset_error_count(&mut self) {
        self.frame_parser.reset_error_count();
    }
}
