use std::{
    error::Error,
    fmt,
    fs::{File, OpenOptions},
    io,
    os::fd::AsRawFd,
    path::Path,
    str::FromStr,
    time::Instant,
};

use cvi_camera_uapi::{
    CVI_CAMERA_CHROMA_SITING_CENTER, CVI_CAMERA_COLOR_MATRIX_BT601, CVI_CAMERA_COLOR_RANGE_FULL,
    CVI_CAMERA_FORMAT_YUV420_PLANAR, CVI_CAMERA_IOCTL_DECODE_JPEG, CVI_CAMERA_SCALE_EIGHTH,
    CVI_CAMERA_SCALE_FULL, CVI_CAMERA_SCALE_HALF, CVI_CAMERA_SCALE_QUARTER, CviCameraExtentV1,
    CviCameraFrameLayoutV1, CviCameraPlaneV1, CviCameraRequestV1, CviCameraValidationError,
};

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
            Self::Full => CVI_CAMERA_SCALE_FULL,
            Self::Half => CVI_CAMERA_SCALE_HALF,
            Self::Quarter => CVI_CAMERA_SCALE_QUARTER,
            Self::Eighth => CVI_CAMERA_SCALE_EIGHTH,
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
    InvalidInput(&'static str),
    Protocol(&'static str),
    UnsupportedLayout(&'static str),
    Uapi(CviCameraValidationError),
    Yuv420(Yuv420Error),
}

impl fmt::Display for CameraError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "camera ioctl failed: {error}"),
            Self::InvalidInput(message) => write!(f, "invalid JPU input: {message}"),
            Self::Protocol(message) => write!(f, "invalid camera response: {message}"),
            Self::UnsupportedLayout(message) => {
                write!(f, "unsupported camera output layout: {message}")
            }
            Self::Uapi(error) => write!(f, "invalid camera UAPI data: {error}"),
            Self::Yuv420(error) => write!(f, "invalid camera YUV420 frame: {error}"),
        }
    }
}

impl Error for CameraError {}

impl From<io::Error> for CameraError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<CviCameraValidationError> for CameraError {
    fn from(error: CviCameraValidationError) -> Self {
        Self::Uapi(error)
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

/// Replays caller-provided JPEG bytes through the StarryOS SG2002 JPU ioctl.
///
/// `prepare_jpeg` performs the metadata query and grows the reusable output
/// buffer. `decode_jpeg` never queries or allocates, keeping timed runs stable.
pub struct CviJpuDecoder {
    device: File,
    scale: JpuScale,
    output: Vec<u8>,
}

impl CviJpuDecoder {
    pub fn open(path: &Path, scale: JpuScale) -> Result<Self, CameraError> {
        let device = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Self {
            device,
            scale,
            output: Vec::new(),
        })
    }

    pub const fn scale(&self) -> JpuScale {
        self.scale
    }

    pub fn prepare_jpeg(&mut self, jpeg: &[u8]) -> Result<usize, CameraError> {
        validate_jpeg_input(jpeg)?;
        let mut request = CviCameraRequestV1::new_decode_query(
            user_pointer(jpeg.as_ptr())?,
            jpeg.len() as u64,
            self.scale.as_uapi(),
        );
        ioctl_request(&self.device, CVI_CAMERA_IOCTL_DECODE_JPEG, &mut request)?;
        request.validate_decode()?;
        let (frame_len, _) = validate_response_layout(&request.layout)?;
        if self.output.len() < frame_len {
            self.output.resize(frame_len, 0);
        }
        Ok(frame_len)
    }

    pub fn decode_jpeg<'a>(
        &'a mut self,
        jpeg: &[u8],
    ) -> Result<DecodedYuv420Frame<'a>, CameraError> {
        validate_jpeg_input(jpeg)?;
        if self.output.is_empty() {
            return Err(CameraError::InvalidInput(
                "JPEG was not prepared before the measured decode",
            ));
        }

        let mut request = CviCameraRequestV1::new_decode(
            user_pointer(jpeg.as_ptr())?,
            jpeg.len() as u64,
            self.scale.as_uapi(),
            user_pointer(self.output.as_mut_ptr())?,
            self.output.len() as u64,
        );
        let started = Instant::now();
        ioctl_request(&self.device, CVI_CAMERA_IOCTL_DECODE_JPEG, &mut request)?;
        let decode_us = i64::try_from(started.elapsed().as_micros()).unwrap_or(i64::MAX);

        request.validate_decode()?;
        let (frame_len, layout) = validate_response_layout(&request.layout)?;
        if frame_len > self.output.len() {
            return Err(CameraError::Protocol(
                "driver returned data_len larger than the prepared buffer",
            ));
        }
        let planar = PlanarYuv420::new(&self.output[..frame_len], layout)?;
        Ok(DecodedYuv420Frame { planar, decode_us })
    }
}

fn ioctl_request(
    device: &File,
    command: u32,
    request: &mut CviCameraRequestV1,
) -> Result<(), CameraError> {
    let result = unsafe {
        libc::ioctl(
            device.as_raw_fd(),
            command as _,
            request as *mut CviCameraRequestV1,
        )
    };
    if result < 0 {
        return Err(CameraError::Io(io::Error::last_os_error()));
    }
    if result != 0 {
        return Err(CameraError::Protocol("V1 ioctl returned a non-zero value"));
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

fn user_pointer<T>(pointer: *const T) -> Result<u64, CameraError> {
    u64::try_from(pointer as usize)
        .map_err(|_| CameraError::InvalidInput("user pointer does not fit in u64"))
}

fn validate_response_layout(
    layout: &CviCameraFrameLayoutV1,
) -> Result<(usize, Yuv420Layout), CameraError> {
    let frame_len = usize::try_from(layout.data_len)
        .map_err(|_| CameraError::Protocol("data_len does not fit in usize"))?;
    if frame_len == 0 {
        return Err(CameraError::Protocol("driver returned an empty frame"));
    }
    if frame_len > MAX_FRAME_BYTES {
        return Err(CameraError::Protocol("frame exceeds the 64 MiB limit"));
    }
    layout.validate_for_buffer_len(layout.data_len)?;
    let layout = layout_to_yuv420(layout)?;
    Ok((frame_len, layout))
}

fn layout_to_yuv420(layout: &CviCameraFrameLayoutV1) -> Result<Yuv420Layout, CameraError> {
    if layout.format != CVI_CAMERA_FORMAT_YUV420_PLANAR || layout.plane_count != 3 {
        return Err(CameraError::UnsupportedLayout(
            "the validator currently accepts planar YUV420 only",
        ));
    }
    if layout.color_range != CVI_CAMERA_COLOR_RANGE_FULL {
        return Err(CameraError::UnsupportedLayout(
            "YUV range is not JPEG full range",
        ));
    }
    if layout.color_matrix != CVI_CAMERA_COLOR_MATRIX_BT601 {
        return Err(CameraError::UnsupportedLayout("YUV matrix is not BT.601"));
    }
    if layout.chroma_siting != CVI_CAMERA_CHROMA_SITING_CENTER {
        return Err(CameraError::UnsupportedLayout(
            "YUV420 chroma is not JPEG centered",
        ));
    }

    Ok(Yuv420Layout {
        source: image_size(layout.source),
        visible: image_size(layout.visible),
        storage: image_size(layout.storage),
        y: plane_layout(layout.y)?,
        cb: plane_layout(layout.cb)?,
        cr: plane_layout(layout.cr)?,
    })
}

const fn image_size(extent: CviCameraExtentV1) -> ImageSize {
    ImageSize::new(extent.width, extent.height)
}

fn plane_layout(plane: CviCameraPlaneV1) -> Result<PlaneLayout, CameraError> {
    Ok(PlaneLayout {
        offset: usize::try_from(plane.offset)
            .map_err(|_| CameraError::Protocol("plane offset does not fit in usize"))?,
        len: usize::try_from(plane.len)
            .map_err(|_| CameraError::Protocol("plane length does not fit in usize"))?,
        stride: plane.stride as usize,
        visible: image_size(plane.visible),
        storage: image_size(plane.storage),
    })
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
    fn converts_checked_uapi_layout_to_stride_aware_yuv420() {
        let layout = valid_layout();

        let converted = layout_to_yuv420(&layout).unwrap();

        assert_eq!(converted.source, ImageSize::new(127, 63));
        assert_eq!(converted.visible, ImageSize::new(64, 32));
        assert_eq!(converted.y.stride, 64);
        assert_eq!(converted.cb.offset, 2_048);
        assert_eq!(converted.cr.offset, 2_560);
    }

    #[test]
    fn rejects_unexpected_color_contract() {
        let mut layout = valid_layout();
        layout.color_range = 0;

        let error = layout_to_yuv420(&layout).unwrap_err();

        assert!(error.to_string().contains("not JPEG full range"));
    }

    fn valid_layout() -> CviCameraFrameLayoutV1 {
        CviCameraFrameLayoutV1 {
            data_len: 3_072,
            format: CVI_CAMERA_FORMAT_YUV420_PLANAR,
            plane_count: 3,
            color_range: CVI_CAMERA_COLOR_RANGE_FULL,
            color_matrix: CVI_CAMERA_COLOR_MATRIX_BT601,
            chroma_siting: CVI_CAMERA_CHROMA_SITING_CENTER,
            reserved: 0,
            source: extent(127, 63),
            visible: extent(64, 32),
            source_aligned: extent(128, 64),
            coded: extent(64, 32),
            storage: extent(64, 32),
            y: plane(0, 2_048, 64, extent(64, 32), extent(64, 32)),
            cb: plane(2_048, 512, 32, extent(32, 16), extent(32, 16)),
            cr: plane(2_560, 512, 32, extent(32, 16), extent(32, 16)),
        }
    }

    const fn extent(width: u32, height: u32) -> CviCameraExtentV1 {
        CviCameraExtentV1 { width, height }
    }

    const fn plane(
        offset: u64,
        len: u64,
        stride: u32,
        storage: CviCameraExtentV1,
        visible: CviCameraExtentV1,
    ) -> CviCameraPlaneV1 {
        CviCameraPlaneV1 {
            offset,
            len,
            stride,
            storage,
            visible,
            reserved: 0,
        }
    }
}
