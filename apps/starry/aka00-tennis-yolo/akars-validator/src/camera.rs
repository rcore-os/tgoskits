use std::{
    error::Error,
    fmt,
    fs::{File, OpenOptions},
    io,
    os::{fd::AsRawFd, unix::fs::FileExt},
    path::Path,
    str::FromStr,
    time::Instant,
};

use cvi_vdec_uapi::{
    COLOR_GAMUT_BT601, COMPRESS_MODE_NONE, CVI_VC_VDEC_CREATE_CHN, CVI_VC_VDEC_DESTROY_CHN,
    CVI_VC_VDEC_GET_FRAME, CVI_VC_VDEC_RELEASE_FRAME, CVI_VC_VDEC_SEND_STREAM,
    CVI_VC_VDEC_SET_CHN_PARAM, CVI_VC_VDEC_SET_JPEG_SCALE, CVI_VC_VDEC_START_RECV_STREAM,
    CVI_VC_VDEC_STOP_RECV_STREAM, CVI_VDEC_JPEG_SCALE_EIGHTH, CVI_VDEC_JPEG_SCALE_FULL,
    CVI_VDEC_JPEG_SCALE_HALF, CVI_VDEC_JPEG_SCALE_QUARTER, DYNAMIC_RANGE_SDR8,
    PIXEL_FORMAT_YUV_PLANAR_420, VIDEO_FORMAT_LINEAR, VdecChnAttr, VdecChnParam, VdecStream,
    VdecStreamEx, VideoFrameInfo, VideoFrameInfoEx,
};
use zune_core::bytestream::ZCursor;
use zune_jpeg::JpegDecoder;

use crate::yuv420::{ImageSize, PlanarYuv420, PlaneLayout, Yuv420Error, Yuv420Layout};

const MAX_JPEG_BYTES: usize = 16 * 1024 * 1024;
const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct CameraFrame {
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JpuScale {
    Full,
    Half,
    Quarter,
    Eighth,
}

impl JpuScale {
    pub const fn as_uapi(self) -> u32 {
        match self {
            Self::Full => CVI_VDEC_JPEG_SCALE_FULL,
            Self::Half => CVI_VDEC_JPEG_SCALE_HALF,
            Self::Quarter => CVI_VDEC_JPEG_SCALE_QUARTER,
            Self::Eighth => CVI_VDEC_JPEG_SCALE_EIGHTH,
        }
    }

    pub const fn factor(self) -> u32 {
        match self {
            Self::Full => 1,
            Self::Half => 2,
            Self::Quarter => 4,
            Self::Eighth => 8,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Half => "half",
            Self::Quarter => "quarter",
            Self::Eighth => "eighth",
        }
    }
}

impl fmt::Display for JpuScale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for JpuScale {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "full" => Ok(Self::Full),
            "half" => Ok(Self::Half),
            "quarter" => Ok(Self::Quarter),
            "eighth" => Ok(Self::Eighth),
            _ => Err("expected full, half, quarter, or eighth"),
        }
    }
}

#[derive(Debug)]
pub enum CameraError {
    Io(io::Error),
    Jpeg(zune_jpeg::errors::DecodeErrors),
    InvalidInput(&'static str),
    Protocol(&'static str),
    UnsupportedLayout(&'static str),
    Yuv420(Yuv420Error),
}

impl fmt::Display for CameraError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "VDEC operation failed: {error}"),
            Self::Jpeg(error) => write!(f, "JPEG header decode failed: {error}"),
            Self::InvalidInput(message) => write!(f, "invalid JPU input: {message}"),
            Self::Protocol(message) => write!(f, "invalid VDEC response: {message}"),
            Self::UnsupportedLayout(message) => {
                write!(f, "unsupported VDEC output layout: {message}")
            }
            Self::Yuv420(error) => write!(f, "invalid VDEC YUV420 frame: {error}"),
        }
    }
}

impl Error for CameraError {}

impl From<io::Error> for CameraError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<zune_jpeg::errors::DecodeErrors> for CameraError {
    fn from(error: zune_jpeg::errors::DecodeErrors) -> Self {
        Self::Jpeg(error)
    }
}

impl From<Yuv420Error> for CameraError {
    fn from(error: Yuv420Error) -> Self {
        Self::Yuv420(error)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DecodedYuv420Frame<'a> {
    pub planar: PlanarYuv420<'a>,
    pub decode_us: i64,
}

#[derive(Clone, Copy, Debug)]
struct PreparedJpeg {
    address: usize,
    length: usize,
    source: ImageSize,
}

/// Replays JPEG bytes through the SG2002-compatible VDEC channel lifecycle.
pub struct CviJpuDecoder {
    device: File,
    scale: JpuScale,
    output: Vec<u8>,
    prepared: Vec<PreparedJpeg>,
    max_width: u32,
    max_height: u32,
    max_stream_len: usize,
    max_frame_len: usize,
    started: bool,
}

impl CviJpuDecoder {
    pub fn open(path: &Path, scale: JpuScale) -> Result<Self, CameraError> {
        let device = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Self {
            device,
            scale,
            output: Vec::new(),
            prepared: Vec::new(),
            max_width: 0,
            max_height: 0,
            max_stream_len: 0,
            max_frame_len: 0,
            started: false,
        })
    }

    pub const fn scale(&self) -> JpuScale {
        self.scale
    }

    /// Parses metadata and grows channel limits without touching JPU hardware.
    pub fn prepare_jpeg(&mut self, jpeg: &[u8]) -> Result<usize, CameraError> {
        if self.started {
            return Err(CameraError::InvalidInput(
                "cannot prepare more JPEGs after the VDEC channel starts",
            ));
        }
        validate_jpeg_input(jpeg)?;
        let source = jpeg_dimensions(jpeg)?;
        let frame_len = conservative_frame_len(source, self.scale)?;
        if frame_len > MAX_FRAME_BYTES {
            return Err(CameraError::InvalidInput(
                "scaled frame exceeds the 64 MiB limit",
            ));
        }

        self.max_width = self.max_width.max(source.width);
        self.max_height = self.max_height.max(source.height);
        self.max_stream_len = self.max_stream_len.max(jpeg.len());
        self.max_frame_len = self.max_frame_len.max(frame_len);
        let prepared = PreparedJpeg {
            address: jpeg.as_ptr() as usize,
            length: jpeg.len(),
            source,
        };
        if !self
            .prepared
            .iter()
            .any(|entry| entry.address == prepared.address && entry.length == prepared.length)
        {
            self.prepared.push(prepared);
        }
        Ok(frame_len)
    }

    /// Creates and starts the VDEC channel after all benchmark inputs are known.
    pub fn start(&mut self) -> Result<(), CameraError> {
        if self.started {
            return Err(CameraError::InvalidInput("VDEC channel is already started"));
        }
        if self.prepared.is_empty() {
            return Err(CameraError::InvalidInput("no JPEG inputs were prepared"));
        }

        let stream_size = u32::try_from(self.max_stream_len)
            .map_err(|_| CameraError::InvalidInput("stream size does not fit u32"))?;
        let frame_size = u32::try_from(self.max_frame_len)
            .map_err(|_| CameraError::InvalidInput("frame size does not fit u32"))?;
        let mut attr =
            VdecChnAttr::jpeg_frame(self.max_width, self.max_height, stream_size, frame_size);
        ioctl_pointer(&self.device, CVI_VC_VDEC_CREATE_CHN, &mut attr)?;

        let configured = (|| {
            let mut param = VdecChnParam::jpeg_yuv420();
            ioctl_pointer(&self.device, CVI_VC_VDEC_SET_CHN_PARAM, &mut param)?;
            let mut scale = self.scale.as_uapi();
            ioctl_pointer(&self.device, CVI_VC_VDEC_SET_JPEG_SCALE, &mut scale)?;
            ioctl_no_argument(&self.device, CVI_VC_VDEC_START_RECV_STREAM)
        })();
        if let Err(error) = configured {
            let _ = ioctl_no_argument(&self.device, CVI_VC_VDEC_DESTROY_CHN);
            return Err(error);
        }
        self.started = true;
        Ok(())
    }

    pub fn decode_jpeg<'a>(
        &'a mut self,
        jpeg: &[u8],
    ) -> Result<DecodedYuv420Frame<'a>, CameraError> {
        if !self.started {
            return Err(CameraError::InvalidInput(
                "VDEC channel was not started after preparation",
            ));
        }
        validate_jpeg_input(jpeg)?;
        let source = self.prepared_source(jpeg)?;
        let length = u32::try_from(jpeg.len())
            .map_err(|_| CameraError::InvalidInput("JPEG length does not fit u32"))?;
        let address = user_pointer(jpeg.as_ptr())?;
        let mut stream = VdecStream::frame(address, length, 0);
        let mut stream_ex = VdecStreamEx::new(user_mut_pointer(&mut stream)?, -1);

        let started = Instant::now();
        ioctl_pointer(&self.device, CVI_VC_VDEC_SEND_STREAM, &mut stream_ex)?;

        let mut info = VideoFrameInfo::default();
        let mut frame_ex = VideoFrameInfoEx::new(user_mut_pointer(&mut info)?, -1);
        if let Err(error) = ioctl_pointer(&self.device, CVI_VC_VDEC_GET_FRAME, &mut frame_ex) {
            self.teardown();
            return Err(error);
        }

        let copied = self.copy_frame(&info, source);
        let released = ioctl_pointer(&self.device, CVI_VC_VDEC_RELEASE_FRAME, &mut info);
        let decode_us = i64::try_from(started.elapsed().as_micros()).unwrap_or(i64::MAX);
        if let Err(error) = released {
            self.teardown();
            return Err(error);
        }
        let (frame_len, layout) = copied?;
        let planar = PlanarYuv420::new(&self.output[..frame_len], layout)?;
        Ok(DecodedYuv420Frame { planar, decode_us })
    }

    fn prepared_source(&self, jpeg: &[u8]) -> Result<ImageSize, CameraError> {
        self.prepared
            .iter()
            .find(|entry| entry.address == jpeg.as_ptr() as usize && entry.length == jpeg.len())
            .map(|entry| entry.source)
            .ok_or(CameraError::InvalidInput(
                "JPEG was not prepared before measured decode",
            ))
    }

    fn copy_frame(
        &mut self,
        info: &VideoFrameInfo,
        source: ImageSize,
    ) -> Result<(usize, Yuv420Layout), CameraError> {
        let (frame_len, layout) = frame_layout_to_yuv420(info, source, self.scale)?;
        self.output.resize(frame_len, 0);
        read_exact_at(&self.device, &mut self.output, 0)?;
        Ok((frame_len, layout))
    }

    fn teardown(&mut self) {
        if self.started {
            let _ = ioctl_no_argument(&self.device, CVI_VC_VDEC_STOP_RECV_STREAM);
            let _ = ioctl_no_argument(&self.device, CVI_VC_VDEC_DESTROY_CHN);
            self.started = false;
        }
    }
}

impl Drop for CviJpuDecoder {
    fn drop(&mut self) {
        self.teardown();
    }
}

fn ioctl_pointer<T>(device: &File, command: u32, value: &mut T) -> Result<(), CameraError> {
    let result = unsafe { libc::ioctl(device.as_raw_fd(), command as _, value as *mut T) };
    check_ioctl_result(result)
}

fn ioctl_no_argument(device: &File, command: u32) -> Result<(), CameraError> {
    let result = unsafe { libc::ioctl(device.as_raw_fd(), command as _, 0usize) };
    check_ioctl_result(result)
}

fn check_ioctl_result(result: i32) -> Result<(), CameraError> {
    if result < 0 {
        return Err(CameraError::Io(io::Error::last_os_error()));
    }
    if result != 0 {
        return Err(CameraError::Protocol("ioctl returned a non-zero value"));
    }
    Ok(())
}

fn read_exact_at(file: &File, mut destination: &mut [u8], mut offset: u64) -> io::Result<()> {
    while !destination.is_empty() {
        match file.read_at(destination, offset)? {
            0 => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
            copied => {
                offset = offset
                    .checked_add(copied as u64)
                    .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
                destination = &mut destination[copied..];
            }
        }
    }
    Ok(())
}

fn validate_jpeg_input(jpeg: &[u8]) -> Result<(), CameraError> {
    if jpeg.is_empty() {
        return Err(CameraError::InvalidInput("JPEG is empty"));
    }
    if jpeg.len() > MAX_JPEG_BYTES {
        return Err(CameraError::InvalidInput("JPEG exceeds the 16 MiB limit"));
    }
    Ok(())
}

fn jpeg_dimensions(jpeg: &[u8]) -> Result<ImageSize, CameraError> {
    let mut decoder = JpegDecoder::new(ZCursor::new(jpeg));
    decoder.decode_headers()?;
    let info = decoder
        .info()
        .ok_or(CameraError::InvalidInput("JPEG metadata is missing"))?;
    Ok(ImageSize::new(
        u32::from(info.width),
        u32::from(info.height),
    ))
}

fn conservative_frame_len(source: ImageSize, scale: JpuScale) -> Result<usize, CameraError> {
    let aligned_width = align_up(source.width, 16)?;
    let aligned_height = align_up(source.height, 16)?;
    if scale != JpuScale::Full && (aligned_width < 128 || aligned_height < 128) {
        return Err(CameraError::InvalidInput(
            "JPU scaling requires both coded dimensions to be at least 128",
        ));
    }
    let coded_width = aligned_width / scale.factor();
    let coded_height = aligned_height / scale.factor();
    let storage_width = align_up(coded_width, 2)?;
    let storage_height = align_up(coded_height, 2)?;
    let stride = align_up(storage_width, 16)? as usize;
    stride
        .checked_mul(storage_height as usize)
        .and_then(|plane| plane.checked_mul(3))
        .ok_or(CameraError::InvalidInput("frame size overflows usize"))
}

fn align_up(value: u32, alignment: u32) -> Result<u32, CameraError> {
    value
        .checked_add(alignment - 1)
        .map(|rounded| rounded & !(alignment - 1))
        .ok_or(CameraError::InvalidInput("aligned dimension overflows u32"))
}

fn user_pointer<T>(pointer: *const T) -> Result<u64, CameraError> {
    u64::try_from(pointer as usize)
        .map_err(|_| CameraError::InvalidInput("user pointer does not fit u64"))
}

fn user_mut_pointer<T>(pointer: *mut T) -> Result<u64, CameraError> {
    u64::try_from(pointer as usize)
        .map_err(|_| CameraError::InvalidInput("user pointer does not fit u64"))
}

fn frame_layout_to_yuv420(
    info: &VideoFrameInfo,
    source: ImageSize,
    scale: JpuScale,
) -> Result<(usize, Yuv420Layout), CameraError> {
    let frame = &info.frame;
    if frame.pixel_format != PIXEL_FORMAT_YUV_PLANAR_420 {
        return Err(CameraError::UnsupportedLayout(
            "the validator accepts planar YUV420 only",
        ));
    }
    if frame.video_format != VIDEO_FORMAT_LINEAR
        || frame.compress_mode != COMPRESS_MODE_NONE
        || frame.dynamic_range != DYNAMIC_RANGE_SDR8
        || frame.color_gamut != COLOR_GAMUT_BT601
    {
        return Err(CameraError::UnsupportedLayout(
            "frame is not linear SDR8 BT.601 data",
        ));
    }
    let expected_visible = ImageSize::new(
        ceil_div(source.width, scale.factor()),
        ceil_div(source.height, scale.factor()),
    );
    let visible = ImageSize::new(frame.width, frame.height);
    if visible != expected_visible || visible.width == 0 || visible.height == 0 {
        return Err(CameraError::Protocol(
            "decoded dimensions do not match JPEG scale",
        ));
    }

    let base = frame.physical_address[0];
    let y = plane_layout(frame, 0, base, visible)?;
    let chroma_visible = ImageSize::new(ceil_div(visible.width, 2), ceil_div(visible.height, 2));
    let cb = plane_layout(frame, 1, base, chroma_visible)?;
    let cr = plane_layout(frame, 2, base, chroma_visible)?;
    let y_end = checked_plane_end(y)?;
    let cb_end = checked_plane_end(cb)?;
    let cr_end = checked_plane_end(cr)?;
    if y_end > cb.offset || cb_end > cr.offset {
        return Err(CameraError::Protocol("VDEC frame planes overlap"));
    }
    let frame_len = y_end.max(cb_end).max(cr_end);
    if frame_len == 0 || frame_len > MAX_FRAME_BYTES {
        return Err(CameraError::Protocol(
            "VDEC frame length is outside the supported range",
        ));
    }
    let storage = y.storage;
    Ok((
        frame_len,
        Yuv420Layout {
            source,
            visible,
            storage,
            y,
            cb,
            cr,
        },
    ))
}

fn plane_layout(
    frame: &cvi_vdec_uapi::VideoFrame,
    index: usize,
    base: u64,
    visible: ImageSize,
) -> Result<PlaneLayout, CameraError> {
    let stride = frame.stride[index] as usize;
    let len = frame.length[index] as usize;
    if stride == 0 || len == 0 || !len.is_multiple_of(stride) {
        return Err(CameraError::Protocol(
            "VDEC plane length is not a whole number of rows",
        ));
    }
    let physical_offset =
        frame.physical_address[index]
            .checked_sub(base)
            .ok_or(CameraError::Protocol(
                "VDEC plane addresses are not ordered",
            ))?;
    let offset = usize::try_from(physical_offset)
        .map_err(|_| CameraError::Protocol("VDEC plane offset does not fit usize"))?;
    let storage = ImageSize::new(frame.stride[index], (len / stride) as u32);
    Ok(PlaneLayout {
        offset,
        len,
        stride,
        visible,
        storage,
    })
}

fn checked_plane_end(plane: PlaneLayout) -> Result<usize, CameraError> {
    plane
        .offset
        .checked_add(plane.len)
        .ok_or(CameraError::Protocol("VDEC plane range overflows usize"))
}

const fn ceil_div(value: u32, divisor: u32) -> u32 {
    value / divisor + if value.is_multiple_of(divisor) { 0 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_explicit_scale_names() {
        assert_eq!("full".parse(), Ok(JpuScale::Full));
        assert_eq!("half".parse(), Ok(JpuScale::Half));
        assert_eq!("quarter".parse(), Ok(JpuScale::Quarter));
        assert_eq!("eighth".parse(), Ok(JpuScale::Eighth));
        assert!("1/2".parse::<JpuScale>().is_err());
    }

    #[test]
    fn conservative_capacity_covers_native_yuv444_layout() {
        assert_eq!(
            conservative_frame_len(ImageSize::new(1279, 1706), JpuScale::Half).unwrap(),
            640 * 856 * 3
        );
    }

    #[test]
    fn converts_vendor_frame_description_to_stride_aware_yuv420() {
        let info = valid_frame();

        let (frame_len, layout) =
            frame_layout_to_yuv420(&info, ImageSize::new(1279, 1706), JpuScale::Half).unwrap();

        assert_eq!(frame_len, 821_760);
        assert_eq!(layout.source, ImageSize::new(1279, 1706));
        assert_eq!(layout.visible, ImageSize::new(640, 853));
        assert_eq!(layout.y.stride, 640);
        assert_eq!(layout.cb.offset, 547_840);
        assert_eq!(layout.cr.offset, 684_800);
    }

    #[test]
    fn rejects_unexpected_pixel_format() {
        let mut info = valid_frame();
        info.frame.pixel_format = 12;

        let error =
            frame_layout_to_yuv420(&info, ImageSize::new(1279, 1706), JpuScale::Half).unwrap_err();

        assert!(error.to_string().contains("planar YUV420 only"));
    }

    fn valid_frame() -> VideoFrameInfo {
        let mut info = VideoFrameInfo::default();
        info.frame.width = 640;
        info.frame.height = 853;
        info.frame.pixel_format = PIXEL_FORMAT_YUV_PLANAR_420;
        info.frame.video_format = VIDEO_FORMAT_LINEAR;
        info.frame.compress_mode = COMPRESS_MODE_NONE;
        info.frame.dynamic_range = DYNAMIC_RANGE_SDR8;
        info.frame.color_gamut = COLOR_GAMUT_BT601;
        info.frame.stride = [640, 320, 320];
        info.frame.physical_address = [0x10_0000, 0x18_5c00, 0x1a_7300];
        info.frame.length = [547_840, 136_960, 136_960];
        info
    }
}
