//! SG2002-compatible JPEG VDEC channel.
//!
//! Sophgo userspace maps the physical planes returned by `GET_FRAME` through
//! `/dev/mem`. Starry does not expose arbitrary physical memory, so this node
//! additionally lets userspace `read` the one outstanding frame by byte
//! offset. The ioctl command numbers, nested structures, and lifecycle remain
//! wire-compatible with `/dev/cvi_vc_dec0`.

use alloc::{sync::Arc, vec::Vec};
use core::{any::Any, mem::size_of};

use axfs_ng_vfs::{NodeFlags, VfsError, VfsResult};
use cvi_vdec_uapi::{
    COLOR_GAMUT_BT601, COMPRESS_MODE_NONE, CVI_VC_VDEC_CREATE_CHN, CVI_VC_VDEC_DESTROY_CHN,
    CVI_VC_VDEC_GET_CHN_ATTR, CVI_VC_VDEC_GET_CHN_PARAM, CVI_VC_VDEC_GET_FRAME,
    CVI_VC_VDEC_QUERY_STATUS, CVI_VC_VDEC_RELEASE_FRAME, CVI_VC_VDEC_RESET_CHN,
    CVI_VC_VDEC_SEND_STREAM, CVI_VC_VDEC_SET_CHN_ATTR, CVI_VC_VDEC_SET_CHN_PARAM,
    CVI_VC_VDEC_SET_JPEG_SCALE, CVI_VC_VDEC_START_RECV_STREAM, CVI_VC_VDEC_STOP_RECV_STREAM,
    CVI_VDEC_JPEG_SCALE_EIGHTH, CVI_VDEC_JPEG_SCALE_FULL, CVI_VDEC_JPEG_SCALE_HALF,
    CVI_VDEC_JPEG_SCALE_QUARTER, DYNAMIC_RANGE_SDR8, PIXEL_FORMAT_YUV_PLANAR_420, PT_JPEG,
    PT_MJPEG, VB_INVALID_POOL_ID, VIDEO_FORMAT_LINEAR, VIDEO_MODE_FRAME, VdecChnAttr, VdecChnParam,
    VdecChnStatus, VdecStream, VdecStreamEx, VideoFrame, VideoFrameInfo, VideoFrameInfoEx,
};
use sg200x_jpu::{
    FrameLayout, FrameLayoutError, JpuInspectError, JpuPixelFormat, JpuScale, PlaneLayout,
    inspect_jpeg_layout,
};
use starry_vm::{VmMutPtr, VmPtr, vm_read_slice};

use super::cvi_jpu::{CviJpu, DecodedJpuFrame};
use crate::{StarryError, StarryResult, pseudofs::DeviceOps, sync::Mutex};

const MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;
const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChannelPhase {
    Uncreated,
    Created,
    Receiving,
}

#[derive(Clone, Copy, Debug)]
struct PendingFrame {
    info: VideoFrameInfo,
    total_len: usize,
}

struct VdecState {
    phase: ChannelPhase,
    attr: VdecChnAttr,
    param: VdecChnParam,
    scale: JpuScale,
    pending: Option<PendingFrame>,
    stream_scratch: Vec<u8>,
    received_frames: u32,
    decoded_frames: u32,
}

impl Default for VdecState {
    fn default() -> Self {
        Self {
            phase: ChannelPhase::Uncreated,
            attr: VdecChnAttr::default(),
            param: VdecChnParam::jpeg_yuv420(),
            scale: JpuScale::Full,
            pending: None,
            stream_scratch: Vec::new(),
            received_frames: 0,
            decoded_frames: 0,
        }
    }
}

impl VdecState {
    fn create(&mut self, jpu: &CviJpu, attr: VdecChnAttr) -> StarryResult<()> {
        if self.phase != ChannelPhase::Uncreated {
            return Err(StarryError::ResourceBusy);
        }
        validate_attr(&attr)?;
        jpu.acquire_vdec()?;
        self.phase = ChannelPhase::Created;
        self.attr = attr;
        self.param = VdecChnParam {
            payload_type: attr.payload_type,
            ..VdecChnParam::jpeg_yuv420()
        };
        self.scale = JpuScale::Full;
        self.pending = None;
        self.received_frames = 0;
        self.decoded_frames = 0;
        Ok(())
    }

    fn destroy(&mut self, jpu: &CviJpu) -> StarryResult<()> {
        if self.phase == ChannelPhase::Uncreated {
            return Err(StarryError::InvalidInput);
        }
        self.phase = ChannelPhase::Uncreated;
        self.pending = None;
        self.stream_scratch.clear();
        jpu.release_vdec();
        Ok(())
    }

    fn start(&mut self) -> StarryResult<()> {
        if self.phase != ChannelPhase::Created {
            return Err(StarryError::InvalidInput);
        }
        self.phase = ChannelPhase::Receiving;
        Ok(())
    }

    fn stop(&mut self) -> StarryResult<()> {
        if self.phase != ChannelPhase::Receiving {
            return Err(StarryError::InvalidInput);
        }
        if self.pending.is_some() {
            return Err(StarryError::ResourceBusy);
        }
        self.phase = ChannelPhase::Created;
        Ok(())
    }

    fn reset(&mut self) -> StarryResult<()> {
        if self.phase == ChannelPhase::Uncreated {
            return Err(StarryError::InvalidInput);
        }
        self.phase = ChannelPhase::Created;
        self.pending = None;
        self.received_frames = 0;
        self.decoded_frames = 0;
        Ok(())
    }

    fn set_attr(&mut self, attr: VdecChnAttr) -> StarryResult<()> {
        if self.phase != ChannelPhase::Created || self.pending.is_some() {
            return Err(StarryError::ResourceBusy);
        }
        validate_attr(&attr)?;
        self.attr = attr;
        self.param.payload_type = attr.payload_type;
        Ok(())
    }

    fn set_param(&mut self, param: VdecChnParam) -> StarryResult<()> {
        if self.phase != ChannelPhase::Created || self.pending.is_some() {
            return Err(StarryError::ResourceBusy);
        }
        validate_param(&param, self.attr.payload_type)?;
        self.param = param;
        Ok(())
    }

    fn set_scale(&mut self, raw_scale: u32) -> StarryResult<()> {
        if self.phase != ChannelPhase::Created || self.pending.is_some() {
            return Err(StarryError::ResourceBusy);
        }
        self.scale = scale_from_uapi(raw_scale)?;
        Ok(())
    }

    fn send_stream(&mut self, jpu: &CviJpu, argument: usize) -> StarryResult<()> {
        if self.phase != ChannelPhase::Receiving {
            return Err(StarryError::InvalidInput);
        }
        if self.pending.is_some() {
            return Err(StarryError::ResourceBusy);
        }

        let stream_ex = (argument as *const VdecStreamEx).vm_read()?;
        let stream_pointer = checked_user_pointer::<VdecStream>(stream_ex.stream)?;
        let stream = stream_pointer.vm_read()?;
        validate_stream(&stream, &self.attr)?;
        read_user_bytes_into(
            &mut self.stream_scratch,
            stream.address,
            usize::try_from(stream.length).map_err(|_| StarryError::InvalidInput)?,
        )?;

        let inspected =
            inspect_jpeg_layout(&self.stream_scratch, self.scale).map_err(map_inspect_error)?;
        validate_layout(&inspected, &self.attr, &self.param)?;
        let decoded = jpu.decode_vdec(&self.stream_scratch, self.scale)?;
        if decoded.layout != inspected {
            warn!(
                "cvi-vdec: inspected/decode layout mismatch inspected={inspected:?} decoded={:?}",
                decoded.layout
            );
            return Err(StarryError::Io);
        }

        self.received_frames = self.received_frames.wrapping_add(1);
        self.decoded_frames = self.decoded_frames.wrapping_add(1);
        let info = frame_info(decoded, stream.pts, self.decoded_frames)?;
        self.pending = Some(PendingFrame {
            info,
            total_len: inspected.total_len,
        });
        Ok(())
    }

    fn get_frame(&self, argument: usize) -> StarryResult<()> {
        let pending = self.pending.ok_or(StarryError::ResourceBusy)?;
        let wrapper_pointer = argument as *mut VideoFrameInfoEx;
        let wrapper = wrapper_pointer.vm_read()?;
        let frame_pointer = checked_user_mut_pointer::<VideoFrameInfo>(wrapper.frame_info)?;
        frame_pointer.vm_write(pending.info)?;
        wrapper_pointer.vm_write(wrapper)?;
        Ok(())
    }

    fn release_frame(&mut self, argument: usize) -> StarryResult<()> {
        let supplied = (argument as *const VideoFrameInfo).vm_read()?;
        let pending = self.pending.ok_or(StarryError::InvalidInput)?;
        if !same_frame(&supplied, &pending.info) {
            return Err(StarryError::InvalidInput);
        }
        self.pending = None;
        Ok(())
    }

    fn status(&self) -> VdecChnStatus {
        let dimensions = self
            .pending
            .map(|pending| (pending.info.frame.width, pending.info.frame.height));
        VdecChnStatus {
            payload_type: self.attr.payload_type,
            left_stream_bytes: 0,
            left_stream_frames: 0,
            left_pictures: i32::from(self.pending.is_some()),
            receiving_stream: u8::from(self.phase == ChannelPhase::Receiving),
            padding_after_receiving_stream: [0; 3],
            received_stream_frames: self.received_frames,
            decoded_stream_frames: self.decoded_frames,
            decode_error: Default::default(),
            width: dimensions.map_or(0, |dimensions| dimensions.0),
            height: dimensions.map_or(0, |dimensions| dimensions.1),
        }
    }
}

pub(super) struct CviVdec {
    state: Mutex<VdecState>,
    jpu: Arc<CviJpu>,
}

impl CviVdec {
    pub fn new(jpu: Arc<CviJpu>) -> Self {
        Self {
            state: Mutex::new(VdecState::default()),
            jpu,
        }
    }

    fn read_frame(&self, destination: &mut [u8], offset: u64) -> StarryResult<usize> {
        let state = self.state.lock();
        let pending = state.pending.ok_or(StarryError::InvalidInput)?;
        let offset = usize::try_from(offset).map_err(|_| StarryError::InvalidInput)?;
        self.jpu
            .read_vdec_frame(pending.total_len, offset, destination)
    }

    fn ioctl_inner(&self, command: u32, argument: usize) -> StarryResult<usize> {
        let mut state = self.state.lock();
        match command {
            CVI_VC_VDEC_CREATE_CHN => {
                let attr = (argument as *const VdecChnAttr).vm_read()?;
                state.create(&self.jpu, attr)?;
            }
            CVI_VC_VDEC_DESTROY_CHN => state.destroy(&self.jpu)?,
            CVI_VC_VDEC_GET_CHN_ATTR => {
                ensure_created(state.phase)?;
                (argument as *mut VdecChnAttr).vm_write(state.attr)?;
            }
            CVI_VC_VDEC_SET_CHN_ATTR => {
                let attr = (argument as *const VdecChnAttr).vm_read()?;
                state.set_attr(attr)?;
            }
            CVI_VC_VDEC_START_RECV_STREAM => state.start()?,
            CVI_VC_VDEC_STOP_RECV_STREAM => state.stop()?,
            CVI_VC_VDEC_QUERY_STATUS => {
                ensure_created(state.phase)?;
                let pointer = argument as *mut VdecChnStatus;
                let _previous = pointer.vm_read()?;
                pointer.vm_write(state.status())?;
            }
            CVI_VC_VDEC_RESET_CHN => state.reset()?,
            CVI_VC_VDEC_SET_CHN_PARAM => {
                let param = (argument as *const VdecChnParam).vm_read()?;
                state.set_param(param)?;
            }
            CVI_VC_VDEC_GET_CHN_PARAM => {
                ensure_created(state.phase)?;
                (argument as *mut VdecChnParam).vm_write(state.param)?;
            }
            CVI_VC_VDEC_SEND_STREAM => state.send_stream(&self.jpu, argument)?,
            CVI_VC_VDEC_GET_FRAME => state.get_frame(argument)?,
            CVI_VC_VDEC_RELEASE_FRAME => state.release_frame(argument)?,
            CVI_VC_VDEC_SET_JPEG_SCALE => {
                let scale = (argument as *const u32).vm_read()?;
                state.set_scale(scale)?;
            }
            _ => return Err(StarryError::NotATty),
        }
        Ok(0)
    }
}

impl DeviceOps for CviVdec {
    fn read_at(&self, destination: &mut [u8], offset: u64) -> VfsResult<usize> {
        self.read_frame(destination, offset).map_err(VfsError::from)
    }

    fn write_at(&self, _buffer: &[u8], _offset: u64) -> VfsResult<usize> {
        Err(VfsError::OperationNotSupported)
    }

    fn ioctl(&self, command: u32, argument: usize) -> VfsResult<usize> {
        self.ioctl_inner(command, argument).map_err(VfsError::from)
    }

    fn close(&self, _exclusive: bool) {
        let mut state = self.state.lock();
        if state.phase != ChannelPhase::Uncreated {
            let _ = state.destroy(&self.jpu);
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM
    }
}

fn ensure_created(phase: ChannelPhase) -> StarryResult<()> {
    if phase == ChannelPhase::Uncreated {
        Err(StarryError::InvalidInput)
    } else {
        Ok(())
    }
}

fn validate_attr(attr: &VdecChnAttr) -> StarryResult<()> {
    if !matches!(attr.payload_type, PT_JPEG | PT_MJPEG) {
        return Err(StarryError::OperationNotSupported);
    }
    if attr.video_mode != VIDEO_MODE_FRAME {
        return Err(StarryError::OperationNotSupported);
    }
    if attr.picture_width == 0
        || attr.picture_height == 0
        || attr.picture_width > u16::MAX as u32
        || attr.picture_height > u16::MAX as u32
        || attr.frame_buffer_count == 0
    {
        return Err(StarryError::InvalidInput);
    }
    let stream_size =
        usize::try_from(attr.stream_buffer_size).map_err(|_| StarryError::InvalidInput)?;
    let frame_size =
        usize::try_from(attr.frame_buffer_size).map_err(|_| StarryError::InvalidInput)?;
    if stream_size == 0
        || stream_size > MAX_STREAM_BYTES
        || frame_size == 0
        || frame_size > MAX_FRAME_BYTES
    {
        return Err(StarryError::InvalidInput);
    }
    Ok(())
}

fn validate_param(param: &VdecChnParam, payload_type: i32) -> StarryResult<()> {
    if param.payload_type != payload_type {
        return Err(StarryError::InvalidInput);
    }
    if param.pixel_format != PIXEL_FORMAT_YUV_PLANAR_420 {
        return Err(StarryError::OperationNotSupported);
    }
    if param.display_frame_count > 16 || param.codec_param[0] > 255 {
        return Err(StarryError::InvalidInput);
    }
    Ok(())
}

fn validate_stream(stream: &VdecStream, attr: &VdecChnAttr) -> StarryResult<()> {
    if stream.length == 0
        || stream.length > attr.stream_buffer_size
        || stream.address == 0
        || stream.end_of_frame != 1
        || stream.end_of_stream > 1
        || stream.display > 1
    {
        return Err(StarryError::InvalidInput);
    }
    Ok(())
}

fn validate_layout(
    layout: &FrameLayout,
    attr: &VdecChnAttr,
    param: &VdecChnParam,
) -> StarryResult<()> {
    if layout.source.width > attr.picture_width || layout.source.height > attr.picture_height {
        return Err(StarryError::InvalidInput);
    }
    if layout.total_len > attr.frame_buffer_size as usize {
        return Err(StarryError::StorageFull);
    }
    if layout.format != JpuPixelFormat::Yuv420 || param.pixel_format != PIXEL_FORMAT_YUV_PLANAR_420
    {
        return Err(StarryError::OperationNotSupported);
    }
    Ok(())
}

fn scale_from_uapi(scale: u32) -> StarryResult<JpuScale> {
    match scale {
        CVI_VDEC_JPEG_SCALE_FULL => Ok(JpuScale::Full),
        CVI_VDEC_JPEG_SCALE_HALF => Ok(JpuScale::Half),
        CVI_VDEC_JPEG_SCALE_QUARTER => Ok(JpuScale::Quarter),
        CVI_VDEC_JPEG_SCALE_EIGHTH => Ok(JpuScale::Eighth),
        _ => Err(StarryError::InvalidInput),
    }
}

fn checked_user_pointer<T>(address: u64) -> StarryResult<*const T> {
    let address = usize::try_from(address).map_err(|_| StarryError::InvalidInput)?;
    address
        .checked_add(size_of::<T>())
        .ok_or(StarryError::InvalidInput)?;
    Ok(address as *const T)
}

fn checked_user_mut_pointer<T>(address: u64) -> StarryResult<*mut T> {
    Ok(checked_user_pointer::<T>(address)? as *mut T)
}

fn read_user_bytes_into(bytes: &mut Vec<u8>, address: u64, length: usize) -> StarryResult<()> {
    let pointer = checked_user_pointer::<u8>(address)?;
    pointer
        .addr()
        .checked_add(length)
        .ok_or(StarryError::InvalidInput)?;
    bytes.clear();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| StarryError::NoMemory)?;
    vm_read_slice(pointer, &mut bytes.spare_capacity_mut()[..length])?;
    // SAFETY: `vm_read_slice` initialized every byte in the reserved range.
    unsafe { bytes.set_len(length) };
    Ok(())
}

fn map_inspect_error(error: JpuInspectError) -> StarryError {
    match error {
        JpuInspectError::Layout(error) => map_layout_error(error),
        JpuInspectError::EmptyStream | JpuInspectError::InvalidJpeg(_) => StarryError::InvalidInput,
    }
}

fn map_layout_error(_error: FrameLayoutError) -> StarryError {
    StarryError::OperationNotSupported
}

fn frame_info(decoded: DecodedJpuFrame, pts: u64, time_ref: u32) -> StarryResult<VideoFrameInfo> {
    let layout = decoded.layout;
    let cb = layout.cb.ok_or(StarryError::OperationNotSupported)?;
    let cr = layout.cr.ok_or(StarryError::OperationNotSupported)?;
    Ok(VideoFrameInfo {
        frame: VideoFrame {
            width: layout.visible.width,
            height: layout.visible.height,
            pixel_format: PIXEL_FORMAT_YUV_PLANAR_420,
            bayer_format: 0,
            video_format: VIDEO_FORMAT_LINEAR,
            compress_mode: COMPRESS_MODE_NONE,
            dynamic_range: DYNAMIC_RANGE_SDR8,
            color_gamut: COLOR_GAMUT_BT601,
            stride: [layout.y.stride, cb.stride, cr.stride],
            padding_before_physical_address: [0; 4],
            physical_address: [
                plane_address(decoded.dma_address, layout.y)?,
                plane_address(decoded.dma_address, cb)?,
                plane_address(decoded.dma_address, cr)?,
            ],
            virtual_address: [0; 3],
            length: [plane_len(layout.y)?, plane_len(cb)?, plane_len(cr)?],
            offset_top: 0,
            offset_bottom: 0,
            offset_left: 0,
            offset_right: 0,
            time_ref,
            pts,
            private_data: u64::from(time_ref),
            frame_flag: 0,
            trailing_padding: [0; 4],
        },
        pool_id: VB_INVALID_POOL_ID,
        trailing_padding: [0; 4],
    })
}

fn plane_address(base: u64, plane: PlaneLayout) -> StarryResult<u64> {
    base.checked_add(u64::try_from(plane.offset).map_err(|_| StarryError::Io)?)
        .ok_or(StarryError::Io)
}

fn plane_len(plane: PlaneLayout) -> StarryResult<u32> {
    u32::try_from(plane.len).map_err(|_| StarryError::Io)
}

fn same_frame(left: &VideoFrameInfo, right: &VideoFrameInfo) -> bool {
    left.frame.physical_address == right.frame.physical_address
        && left.frame.length == right.frame.length
        && left.frame.pts == right.frame.pts
        && left.frame.private_data == right.frame.private_data
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_bounded_frame_mode_jpeg_channels() {
        let valid = VdecChnAttr::jpeg_frame(1920, 1080, 2 * 1024 * 1024, 8 * 1024 * 1024);
        assert!(validate_attr(&valid).is_ok());

        let mut stream_mode = valid;
        stream_mode.video_mode = 0;
        assert!(matches!(
            validate_attr(&stream_mode),
            Err(StarryError::OperationNotSupported)
        ));

        let mut oversized = valid;
        oversized.frame_buffer_size = (MAX_FRAME_BYTES + 1) as u32;
        assert!(matches!(
            validate_attr(&oversized),
            Err(StarryError::InvalidInput)
        ));
    }

    #[test]
    fn scale_extension_maps_the_four_hardware_modes() {
        assert!(matches!(scale_from_uapi(0), Ok(JpuScale::Full)));
        assert!(matches!(scale_from_uapi(1), Ok(JpuScale::Half)));
        assert!(matches!(scale_from_uapi(2), Ok(JpuScale::Quarter)));
        assert!(matches!(scale_from_uapi(3), Ok(JpuScale::Eighth)));
        assert!(matches!(scale_from_uapi(4), Err(StarryError::InvalidInput)));
    }

    #[test]
    fn release_identity_uses_all_driver_owned_frame_tokens() {
        let mut original = VideoFrameInfo::default();
        original.frame.physical_address = [0x1000, 0x2000, 0x3000];
        original.frame.length = [1024, 256, 256];
        original.frame.pts = 7;
        original.frame.private_data = 9;
        assert!(same_frame(&original, &original));

        let mut stale = original;
        stale.frame.private_data = 10;
        assert!(!same_frame(&stale, &original));
    }
}
