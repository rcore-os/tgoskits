#![allow(dead_code)]
use alloc::vec::Vec;
use core::{any::Any, ptr::NonNull, time::Duration};

use ax_errno::{AxError, LinuxError};
use ax_kspin::SpinNoIrq as Mutex;
use ax_memory_addr::PhysAddr;
use ax_runtime::hal::mem::phys_to_virt;
use ax_task::sleep;
use axfs_ng_vfs::{NodeFlags, VfsResult};
use sg200x_bsp::{
    pinmux::{FMUX_SD1_D1, FMUX_SD1_D2, Pinmux},
    soc::{FMUX_BASE, IOBLK_BASE, IOBLK_GRTC_BASE},
};
use some_serial::InterruptMask;
use starry_vm::{VmMutPtr, vm_write_slice};
use tock_registers::interfaces::Writeable;

use crate::pseudofs::{DeviceOps, dev::irq_byte_ring::ByteRing};

pub const CMD_INIT: u8 = 0x01;
pub const CMD_GET_CAMERA_INFO: u8 = 0x02;
pub const CMD_GET_CAMERA_FRAME: u8 = 0x03;
pub const CMD_PING: u8 = 0x7F;
pub const RESP_MASK: u8 = 0x80;
pub const RESP_FRAME_CHUNK: u8 = 0x90;
pub const MAX_FRAME_SIZE: usize = 2 * 1024 * 1024;
pub const FRAME_CHUNK_TIMEOUT_MS: u64 = 1000;
pub const DEFAULT_TIMEOUT_MS: u64 = 2000;
const CAMERA_UART_BUF_CAP: usize = MAX_FRAME_SIZE + 8192;

const SLIP_END: u8 = 0xC0;
const SLIP_ESC: u8 = 0xDB;
const SLIP_ESC_END: u8 = 0xDC;
const SLIP_ESC_ESC: u8 = 0xDD;

unsafe fn cvi_camera_raw_irq_handler(
    _ctx: ax_runtime::hal::irq::IrqContext,
    _data: NonNull<()>,
) -> ax_runtime::hal::irq::IrqReturn {
    let mut uart3 = some_serial::ns16550::dw_apb::DwApbUart::new(
        phys_to_virt(PhysAddr::from(UART3_ADDR)).as_usize(),
    );
    let _ = uart3.handle_irq();
    let mut scratch = [0u8; 64];
    let mut buf = CAMERA_UART_BUF.lock();
    loop {
        let n = match uart3.try_read(&mut scratch) {
            Ok(n) => n,
            Err(err) => err.bytes_transferred,
        };
        if n == 0 {
            break;
        }
        buf.extend(scratch[..n].iter().copied());
    }
    let mask = uart3.get_irq_mask();
    uart3.set_irq_mask(mask | InterruptMask::RX_AVAILABLE);
    ax_runtime::hal::irq::IrqReturn::Handled
}

#[derive(Debug)]
pub enum CameraError {
    Timeout,
    SlipEscapeAtEnd,
    InvalidSlipEscape(u8),
    PacketTooShort,
    PacketLengthMismatch,
    CrcMismatch { expected: u16, actual: u16 },
    UnexpectedResponse { ptype: u8, seq: u8 },
    InvalidFrameLength(u32),
    TransportError,
}

pub trait UartTransport {
    fn write_all(&mut self, data: &[u8]) -> Result<(), CameraError>;
    fn read_bytes(&mut self, buf: &mut [u8], timeout_ms: u64) -> Result<usize, CameraError>;
}

pub fn crc16_ccitt_false(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

pub fn slip_encode(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 2);
    out.push(SLIP_END);
    for &b in payload {
        match b {
            SLIP_END => {
                out.push(SLIP_ESC);
                out.push(SLIP_ESC_END);
            }
            SLIP_ESC => {
                out.push(SLIP_ESC);
                out.push(SLIP_ESC_ESC);
            }
            _ => out.push(b),
        }
    }
    out.push(SLIP_END);
    out
}

pub fn slip_decode(frame: &[u8]) -> Result<Vec<u8>, CameraError> {
    let mut out = Vec::with_capacity(frame.len());
    let mut i = 0;
    while i < frame.len() {
        if frame[i] == SLIP_ESC {
            if i + 1 >= frame.len() {
                return Err(CameraError::SlipEscapeAtEnd);
            }
            match frame[i + 1] {
                SLIP_ESC_END => out.push(SLIP_END),
                SLIP_ESC_ESC => out.push(SLIP_ESC),
                n => return Err(CameraError::InvalidSlipEscape(n)),
            }
            i += 2;
        } else {
            out.push(frame[i]);
            i += 1;
        }
    }
    Ok(out)
}

pub struct Packet {
    pub ptype: u8,
    pub seq: u8,
    pub payload: Vec<u8>,
}

fn build_packet(ptype: u8, seq: u8, payload: &[u8]) -> Vec<u8> {
    let plen = payload.len() as u16;
    let mut pkt = Vec::with_capacity(4 + payload.len() + 2);
    pkt.push(ptype);
    pkt.push(seq);
    pkt.extend_from_slice(&plen.to_le_bytes());
    pkt.extend_from_slice(payload);
    let crc = crc16_ccitt_false(&pkt);
    pkt.extend_from_slice(&crc.to_le_bytes());
    pkt
}

fn parse_packet(raw: &[u8]) -> Result<Packet, CameraError> {
    if raw.len() < 6 {
        return Err(CameraError::PacketTooShort);
    }
    let ptype = raw[0];
    let seq = raw[1];
    let plen = u16::from_le_bytes([raw[2], raw[3]]) as usize;
    if raw.len() != 4 + plen + 2 {
        return Err(CameraError::PacketLengthMismatch);
    }
    let payload = raw[4..4 + plen].to_vec();
    let recv_crc = u16::from_le_bytes([raw[4 + plen], raw[5 + plen]]);
    let calc_crc = crc16_ccitt_false(&raw[..4 + plen]);
    if recv_crc != calc_crc {
        return Err(CameraError::CrcMismatch {
            expected: calc_crc,
            actual: recv_crc,
        });
    }
    Ok(Packet {
        ptype,
        seq,
        payload,
    })
}

#[derive(Debug)]
pub struct CameraInfo {
    pub width: u16,
    pub height: u16,
    pub format: u8,
    pub connected: u8,
}

pub struct CameraProtocol<T: UartTransport> {
    transport: T,
    rx_buf: Vec<u8>,
    seq: u8,
    timeout_ms: u64,
}

impl<T: UartTransport> CameraProtocol<T> {
    pub fn new(transport: T, timeout_ms: u64) -> Self {
        Self {
            transport,
            rx_buf: Vec::new(),
            seq: 0,
            timeout_ms,
        }
    }
    pub fn new_default(transport: T) -> Self {
        Self::new(transport, DEFAULT_TIMEOUT_MS)
    }

    fn next_seq(&mut self) -> u8 {
        let s = self.seq;
        self.seq = self.seq.wrapping_add(1);
        s
    }

    pub fn send_packet(&mut self, ptype: u8, payload: &[u8]) -> Result<u8, CameraError> {
        let seq = self.next_seq();
        let encoded = slip_encode(&build_packet(ptype, seq, payload));
        self.transport.write_all(&encoded)?;
        Ok(seq)
    }

    pub fn recv_packet(&mut self, timeout_ms: Option<u64>) -> Result<Packet, CameraError> {
        let t = timeout_ms.unwrap_or(self.timeout_ms);
        let raw = self.read_slip_frame(t)?;
        parse_packet(&slip_decode(&raw)?)
    }

    pub fn request(
        &mut self,
        cmd: u8,
        payload: &[u8],
        timeout_ms: Option<u64>,
    ) -> Result<Vec<u8>, CameraError> {
        let seq = self.send_packet(cmd, payload)?;
        let pkt = self.recv_packet(timeout_ms)?;
        let expected_rsp = cmd | RESP_MASK;
        if pkt.ptype != expected_rsp || pkt.seq != seq {
            return Err(CameraError::UnexpectedResponse {
                ptype: pkt.ptype,
                seq: pkt.seq,
            });
        }
        Ok(pkt.payload)
    }

    fn read_slip_frame(&mut self, timeout_ms: u64) -> Result<Vec<u8>, CameraError> {
        use core::time::Duration;

        use ax_runtime::hal::time::wall_time;
        let deadline = wall_time() + Duration::from_millis(timeout_ms);
        let mut tmp = [0u8; 0x1200];
        loop {
            if let Some(frame) = self.try_extract_frame() {
                return Ok(frame);
            }
            if wall_time() >= deadline {
                return Err(CameraError::Timeout);
            }
            let n = self.transport.read_bytes(&mut tmp, timeout_ms)?;
            if n > 0 {
                self.rx_buf.extend_from_slice(&tmp[..n]);
            }
        }
    }

    fn try_extract_frame(&mut self) -> Option<Vec<u8>> {
        let start = self
            .rx_buf
            .iter()
            .position(|&b| b != SLIP_END)
            .unwrap_or(self.rx_buf.len());
        if start > 0 {
            self.rx_buf.drain(..start);
        }
        let pos = self.rx_buf.iter().position(|&b| b == SLIP_END)?;
        let frame: Vec<u8> = self.rx_buf[..pos].to_vec();
        self.rx_buf.drain(..=pos);
        if frame.is_empty() { None } else { Some(frame) }
    }

    pub fn ping(&mut self) -> Result<Vec<u8>, CameraError> {
        self.request(CMD_PING, b"ping", None)
    }
    pub fn init_camera(&mut self) -> Result<Vec<u8>, CameraError> {
        self.request(CMD_INIT, &[], None)
    }

    pub fn get_camera_info(&mut self) -> Result<CameraInfo, CameraError> {
        let rsp = self.request(CMD_GET_CAMERA_INFO, &[], None)?;
        if rsp.len() < 6 {
            return Err(CameraError::PacketTooShort);
        }
        Ok(CameraInfo {
            width: u16::from_le_bytes([rsp[0], rsp[1]]),
            height: u16::from_le_bytes([rsp[2], rsp[3]]),
            format: rsp[4],
            connected: rsp[5],
        })
    }

    pub fn get_frame(&mut self) -> Result<Vec<u8>, CameraError> {
        self.get_frame_with_timeout(FRAME_CHUNK_TIMEOUT_MS)
    }

    pub fn get_frame_with_timeout(
        &mut self,
        chunk_timeout_ms: u64,
    ) -> Result<Vec<u8>, CameraError> {
        let rsp = self.request(CMD_GET_CAMERA_FRAME, &[], None)?;
        if rsp.len() < 4 {
            return Err(CameraError::PacketTooShort);
        }
        let frame_len = u32::from_le_bytes([rsp[0], rsp[1], rsp[2], rsp[3]]) as usize;
        if frame_len == 0 || frame_len > MAX_FRAME_SIZE {
            return Err(CameraError::InvalidFrameLength(frame_len as u32));
        }
        let mut data = Vec::with_capacity(frame_len);
        if rsp.len() > 4 {
            data.extend_from_slice(&rsp[4..]);
        }
        while data.len() < frame_len {
            let pkt = self.recv_packet(Some(chunk_timeout_ms))?;
            if pkt.ptype != RESP_FRAME_CHUNK {
                return Err(CameraError::UnexpectedResponse {
                    ptype: pkt.ptype,
                    seq: pkt.seq,
                });
            }
            data.extend_from_slice(&pkt.payload);
        }
        data.truncate(frame_len);
        Ok(data)
    }
}

const UART3_ADDR: usize = 0x04170000;
static CAMERA_UART_BUF: Mutex<ByteRing<CAMERA_UART_BUF_CAP>> = Mutex::new(ByteRing::new());

struct Uart3;

impl UartTransport for Uart3 {
    fn write_all(&mut self, data: &[u8]) -> Result<(), CameraError> {
        let mut uart3 = some_serial::ns16550::dw_apb::DwApbUart::new(
            phys_to_virt(PhysAddr::from(UART3_ADDR)).as_usize(),
        );
        let mut written = 0;
        while written < data.len() {
            let n = uart3.try_write(&data[written..]);
            if n == 0 {
                core::hint::spin_loop();
                continue;
            }
            written += n;
        }
        Ok(())
    }

    fn read_bytes(&mut self, buf: &mut [u8], _timeout_ms: u64) -> Result<usize, CameraError> {
        sleep(Duration::from_millis(3));
        ax_runtime::hal::irq::set_enable(47, false);
        let n = {
            let mut cache_buf = CAMERA_UART_BUF.lock();
            let n = cache_buf.len().min(buf.len());
            if n > 0 {
                cache_buf.drain_into(&mut buf[..n]);
            }
            n
        };
        // Always re-enable the IRQ before returning, otherwise the ESP32's
        // reply traffic stops landing in CAMERA_UART_BUF and every subsequent
        // poll sees an empty queue forever.
        ax_runtime::hal::irq::set_enable(47, true);
        if n == 0 {
            sleep(Duration::from_millis(1));
        }
        Ok(n)
    }
}

pub struct CviCamera {
    inner: Mutex<CameraProtocol<Uart3>>,
}

#[repr(u8)]
#[derive(num_enum::TryFromPrimitive)]
enum CviCameraArgs {
    Init     = 1,
    GetInfo  = 2,
    GetFrame = 3,
}

impl CviCamera {
    pub fn new() -> Self {
        use ax_config::plat::PHYS_VIRT_OFFSET;
        let pinmux = unsafe {
            Pinmux::new(
                FMUX_BASE + PHYS_VIRT_OFFSET,
                IOBLK_BASE + PHYS_VIRT_OFFSET,
                IOBLK_GRTC_BASE + PHYS_VIRT_OFFSET,
            )
        };
        pinmux.fmux().sd1_d2.write(FMUX_SD1_D2::FSEL::UART3_TX);
        pinmux.fmux().sd1_d1.write(FMUX_SD1_D1::FSEL::UART3_RX);

        let mut uart3 = some_serial::ns16550::dw_apb::DwApbUart::new(
            phys_to_virt(PhysAddr::from(UART3_ADDR)).as_usize(),
        );
        uart3.init_with_baud_clk(1_500_000, some_serial::ns16550::dw_apb::SG2002_UART_CLOCK);
        let mask = uart3.get_irq_mask();
        uart3.set_irq_mask(mask | InterruptMask::RX_AVAILABLE);
        let _ = ax_runtime::hal::irq::request_shared_irq(
            47,
            cvi_camera_raw_irq_handler,
            NonNull::dangling(),
        )
        .map_err(|err| warn!("failed to request cvi camera IRQ: {err:?}"));
        ax_runtime::hal::irq::set_enable(47, true);
        Self {
            inner: Mutex::new(CameraProtocol::new_default(Uart3)),
        }
    }
}

impl DeviceOps for CviCamera {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        Ok(0)
    }
    fn write_at(&self, buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Ok(buf.len())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        let cmd = CviCameraArgs::try_from(cmd as u8).map_err(|_| AxError::InvalidInput)?;
        match cmd {
            CviCameraArgs::Init => {
                if let Err(e) = self.inner.lock().init_camera() {
                    warn!("cvi-camera INIT (init_camera) failed: {:?}", e);
                    return Err(LinuxError::EBADF.into());
                }
                if let Err(e) = self.inner.lock().ping() {
                    warn!("cvi-camera INIT (ping) failed: {:?}", e);
                    return Err(LinuxError::EBADF.into());
                }
            }
            CviCameraArgs::GetInfo => {
                let info = match self.inner.lock().get_camera_info() {
                    Ok(i) => i,
                    Err(e) => {
                        warn!("cvi-camera GET_INFO failed: {:?}", e);
                        return Err(LinuxError::EBADF.into());
                    }
                };
                (arg as *mut CameraInfo).vm_write(info)?;
            }
            CviCameraArgs::GetFrame => {
                let frame = match self.inner.lock().get_frame() {
                    Ok(f) => f,
                    Err(e) => {
                        warn!("cvi-camera GET_FRAME failed: {:?}", e);
                        return Err(LinuxError::EBADF.into());
                    }
                };
                vm_write_slice(arg as *mut u8, frame.as_slice())?;
                return Ok(frame.len());
            }
        }
        Ok(0)
    }
}
