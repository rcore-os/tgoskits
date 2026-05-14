use crate::descriptors::payload_header_flags as flags;
use alloc::vec::Vec;
use core::fmt::Debug;
use log::{debug, warn};
use usb_if::err::TransferError;

/// UVC 载荷头（2.4.3.3）
#[derive(Debug, Clone, Default)]
pub struct UvcPayloadHeader {
    pub length: u8,              // bLength
    pub info: u8,                // bmHeaderInfo
    pub fid: bool,               // Frame ID
    pub eof: bool,               // End of Frame
    pub pts: Option<u32>,        // Presentation Time Stamp (4 bytes, 90kHz)
    pub scr: Option<(u32, u16)>, // Source Clock Reference: SOF timestamp (32) + SOF count (16)
    pub has_err: bool,
}

impl UvcPayloadHeader {
    /// 从字节流解析 UVC 载荷头；若数据不合法，返回 None 以允许上层丢弃该包。
    pub fn parse(buf: &[u8]) -> Option<(Self, usize)> {
        if buf.len() < 2 {
            return None;
        }
        let b_length = buf[0] as usize;
        let info = buf[1];
        if b_length < 2 || b_length > buf.len() {
            return None;
        }

        let fid = (info & flags::FID) != 0;
        let eof = (info & flags::EOF) != 0;
        let has_pts = (info & flags::PTS) != 0;
        let has_scr = (info & flags::SCR) != 0;
        let has_err = (info & flags::ERR) != 0;

        // 可选字段顺序：PTS(4) -> SCR(6)
        let mut _offset = 2usize;
        let pts = if has_pts {
            if _offset + 4 > b_length {
                return None;
            }
            let v = u32::from_le_bytes([
                buf[_offset],
                buf[_offset + 1],
                buf[_offset + 2],
                buf[_offset + 3],
            ]);
            _offset += 4;
            Some(v)
        } else {
            None
        };

        let scr = if has_scr {
            if _offset + 6 > b_length {
                return None;
            }
            let stc = u32::from_le_bytes([
                buf[_offset],
                buf[_offset + 1],
                buf[_offset + 2],
                buf[_offset + 3],
            ]);
            let sof = u16::from_le_bytes([buf[_offset + 4], buf[_offset + 5]]);
            _offset += 6;
            Some((stc, sof))
        } else {
            None
        };

        // 剩余可忽略的扩展字段由 b_length 统一跳过
        let header = UvcPayloadHeader {
            length: b_length as u8,
            info,
            fid,
            eof,
            pts,
            scr,
            has_err,
        };

        Some((header, b_length))
    }
}

/// 帧组装事件（供上层转换为具体视频帧结构）
#[derive(Debug, Clone)]
pub struct FrameEvent {
    pub data: Vec<u8>,
    pub pts_90khz: Option<u32>,
    pub eof: bool,
    pub fid: bool,
    pub frame_number: u32,
}

/// UVC 帧解析/组装器（参考 libuvc 的 FID 翻转与 EOF 逻辑）
#[derive(Debug)]
pub struct FrameParser {
    buffer: Option<Vec<u8>>,
    last_fid: Option<bool>,
    last_pts: Option<u32>,
    frame_number: u32,
    error_packet_count: u32, // 统计错误包数量
    frame_size: usize,
    rsv_eof: bool, // 记录上一个包的 EOF 状态，辅助调试
}

impl FrameParser {
    pub fn new(frame_size: usize) -> Self {
        Self {
            buffer: Some(Vec::with_capacity(frame_size)),
            last_fid: None,
            frame_number: 0,
            last_pts: None,
            error_packet_count: 0,
            frame_size,
            rsv_eof: false,
        }
    }

    fn check_fid(&mut self, fid: bool) {
        let Some(last) = self.last_fid else {
            self.last_fid = Some(fid);
            return;
        };

        if last == fid {
            return;
        }

        debug!("FID toggled ({last} -> {fid})",);

        self.last_fid = Some(fid);

        self.buffer = Some(Vec::with_capacity(self.frame_size));
    }

    /// 获取错误包统计信息
    pub fn error_packet_count(&self) -> u32 {
        self.error_packet_count
    }

    /// 重置错误包统计
    pub fn reset_error_count(&mut self) {
        self.error_packet_count = 0;
    }

    /// 处理一包 UVC 传输数据；返回完整帧事件（若 EOF 收到）
    pub fn push_packet(&mut self, data: &[u8]) -> Result<Option<FrameEvent>, TransferError> {
        if data.len() < 2 {
            return Ok(None);
        }

        let (hdr, hdr_len) = match UvcPayloadHeader::parse(data) {
            Some(v) => v,
            None => {
                debug!(
                    "Invalid UVC payload header, dropping packet: {} bytes",
                    data.len()
                );
                return Ok(None);
            }
        };
        // debug!("UVC payload header: {:?}", hdr);
        if hdr.has_err {
            // 记录统计信息，了解错误频率
            self.error_packet_count += 1;
            debug!(
                "UVC payload ERR set; dropping current buffer ({} bytes), total error packets: {}",
                self.buffer.as_ref().map_or(0, |b| b.len()),
                self.error_packet_count
            );
            debug!(
                "Error details: FID={}, EOF={}, PTS={:?}, SCR={:?}, info=0x{:02x}",
                hdr.fid, hdr.eof, hdr.pts, hdr.scr, hdr.info
            );

            // UVC 载荷头中的 ERR 标志表示设备端检测到错误，常见原因包括：
            // 1. 带宽不足：USB 总线带宽不够，导致数据传输延迟或丢失
            // 2. 设备内部错误：传感器或编码器出现临时故障
            // 3. 时序问题：主机请求数据的时机与设备生成数据的时机不匹配
            // 4. 缓冲区溢出：设备内部缓冲区满了，无法继续接收数据

            // 分析错误模式
            if self.error_packet_count % 32 == 1 {
                warn!(
                    "UVC error pattern analysis: {} errors so far, current PTS={:?}, last good PTS={:?}",
                    self.error_packet_count, hdr.pts, self.last_pts
                );
            }

            self.buffer = Some(Vec::with_capacity(self.frame_size));
            self.last_pts = None;
            // 继续后面的包，不要因为单个错误包就停止
            return Ok(None);
        }

        self.check_fid(hdr.fid);

        let Some(ref mut buffer) = self.buffer else {
            // 理论上不应发生
            // warn!("Internal buffer is None, resetting");
            self.buffer = Some(Vec::with_capacity(self.frame_size));
            return Ok(None);
        };

        // 载荷数据在头之后
        if hdr_len <= data.len() {
            let payload = &data[hdr_len..];

            // 高效地trim尾部全0：找到最后一个非0字节，直接截取
            if let Some(last_non_zero_pos) = payload.iter().rposition(|&b| b != 0) {
                buffer.extend_from_slice(&payload[..=last_non_zero_pos]);
            }
        }
        if let Some(pts) = hdr.pts {
            self.last_pts = Some(pts);
        }

        if hdr.eof {
            if !self.rsv_eof {
                self.rsv_eof = true;
                self.buffer = Some(Vec::with_capacity(self.frame_size));
                return Ok(None);
            }

            if buffer.is_empty() {
                // 某些设备会发送空 EOF 包，忽略
                return Ok(None);
            }
            let data = self.buffer.take().unwrap();

            let evt = FrameEvent {
                data,
                pts_90khz: self.last_pts.take(),
                eof: true,
                fid: hdr.fid,
                frame_number: self.frame_number,
            };
            self.frame_number = self.frame_number.wrapping_add(1);
            return Ok(Some(evt));
        }

        Ok(None)
    }
}
