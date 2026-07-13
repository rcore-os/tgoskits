#![no_std]

use bytemuck::{Pod, Zeroable};

/// Legacy command that initializes the camera device.
pub const CVI_CAMERA_IOCTL_INIT: u32 = 1;
/// Legacy command that returns the camera information structure.
pub const CVI_CAMERA_IOCTL_GET_INFO: u32 = 2;
/// Legacy command that copies one captured JPEG to a raw user pointer.
pub const CVI_CAMERA_IOCTL_GET_FRAME: u32 = 3;
/// Legacy command that copies one full-resolution decoded frame to a raw user pointer.
pub const CVI_CAMERA_IOCTL_GET_YUV_FRAME: u32 = 4;

/// Magic byte used by versioned CVI camera ioctl commands.
pub const CVI_CAMERA_IOCTL_MAGIC: u8 = b'C';
/// Command number for a scaled camera capture request.
pub const CVI_CAMERA_IOCTL_CAPTURE_SCALED_NR: u8 = 0x10;
/// Command number for a submitted-JPEG decode request.
pub const CVI_CAMERA_IOCTL_DECODE_JPEG_NR: u8 = 0x11;

/// Version number carried by the first version of the camera UAPI.
pub const CVI_CAMERA_UAPI_VERSION_V1: u32 = 1;

/// Request flag that asks the driver to return metadata without copying frame data.
pub const CVI_CAMERA_REQUEST_FLAG_QUERY_ONLY: u32 = 1 << 0;
/// All request flag bits understood by version 1.
pub const CVI_CAMERA_REQUEST_FLAGS_V1: u32 = CVI_CAMERA_REQUEST_FLAG_QUERY_ONLY;

/// Decode at the JPEG coded resolution.
pub const CVI_CAMERA_SCALE_FULL: u32 = 0;
/// Decode at one half of the coded width and height.
pub const CVI_CAMERA_SCALE_HALF: u32 = 1;
/// Decode at one quarter of the coded width and height.
pub const CVI_CAMERA_SCALE_QUARTER: u32 = 2;
/// Decode at one eighth of the coded width and height.
pub const CVI_CAMERA_SCALE_EIGHTH: u32 = 3;

/// Planar YUV 4:2:0 output.
pub const CVI_CAMERA_FORMAT_YUV420_PLANAR: u32 = 1;
/// Planar YUV 4:2:2 output with horizontal chroma subsampling.
pub const CVI_CAMERA_FORMAT_YUV422_HORIZONTAL_PLANAR: u32 = 2;
/// Planar YUV 4:2:2 output with vertical chroma subsampling.
pub const CVI_CAMERA_FORMAT_YUV422_VERTICAL_PLANAR: u32 = 3;
/// Planar YUV 4:4:4 output.
pub const CVI_CAMERA_FORMAT_YUV444_PLANAR: u32 = 4;
/// Single-plane grayscale output.
pub const CVI_CAMERA_FORMAT_GRAYSCALE: u32 = 5;

/// The color range was not reported.
pub const CVI_CAMERA_COLOR_RANGE_UNSPECIFIED: u32 = 0;
/// Full-range component values.
pub const CVI_CAMERA_COLOR_RANGE_FULL: u32 = 1;
/// Studio-range component values.
pub const CVI_CAMERA_COLOR_RANGE_LIMITED: u32 = 2;

/// The color conversion matrix was not reported.
pub const CVI_CAMERA_COLOR_MATRIX_UNSPECIFIED: u32 = 0;
/// ITU-R BT.601 color conversion matrix.
pub const CVI_CAMERA_COLOR_MATRIX_BT601: u32 = 1;
/// ITU-R BT.709 color conversion matrix.
pub const CVI_CAMERA_COLOR_MATRIX_BT709: u32 = 2;

/// The chroma sample position was not reported.
pub const CVI_CAMERA_CHROMA_SITING_UNSPECIFIED: u32 = 0;
/// Chroma samples are centered between luma samples.
pub const CVI_CAMERA_CHROMA_SITING_CENTER: u32 = 1;
/// Chroma samples are horizontally co-sited with the left luma sample.
pub const CVI_CAMERA_CHROMA_SITING_LEFT: u32 = 2;
/// Chroma samples are co-sited with the top-left luma sample.
pub const CVI_CAMERA_CHROMA_SITING_TOP_LEFT: u32 = 3;

/// Fixed-width request header shared by all version 1 commands.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct CviCameraRequestHeaderV1 {
    /// UAPI version, which must be [`CVI_CAMERA_UAPI_VERSION_V1`].
    pub version: u32,
    /// Size in bytes of the complete [`CviCameraRequestV1`].
    pub size: u32,
    /// Bitwise OR of `CVI_CAMERA_REQUEST_FLAG_*` constants.
    pub flags: u32,
    /// One of the `CVI_CAMERA_SCALE_*` constants.
    pub scale: u32,
}

/// Fixed-width two-dimensional extent in pixels or component samples.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct CviCameraExtentV1 {
    /// Horizontal extent.
    pub width: u32,
    /// Vertical extent.
    pub height: u32,
}

impl CviCameraExtentV1 {
    const fn is_zero(self) -> bool {
        self.width == 0 && self.height == 0
    }

    const fn is_nonzero(self) -> bool {
        self.width != 0 && self.height != 0
    }

    const fn fits_within(self, outer: Self) -> bool {
        self.width <= outer.width && self.height <= outer.height
    }
}

/// Byte layout and visible dimensions of one output component plane.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct CviCameraPlaneV1 {
    /// Byte offset from the beginning of the output buffer.
    pub offset: u64,
    /// Plane byte length including row-stride padding but excluding inter-plane gaps.
    pub len: u64,
    /// Distance in bytes between consecutive component rows.
    pub stride: u32,
    /// Stored component dimensions in samples.
    pub storage: CviCameraExtentV1,
    /// Meaningful component dimensions in samples.
    pub visible: CviCameraExtentV1,
    /// Must be zero.
    pub reserved: u32,
}

impl CviCameraPlaneV1 {
    const fn is_zero(self) -> bool {
        self.offset == 0
            && self.len == 0
            && self.stride == 0
            && self.storage.is_zero()
            && self.visible.is_zero()
            && self.reserved == 0
    }
}

/// Checked metadata describing the complete decoded output buffer.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct CviCameraFrameLayoutV1 {
    /// Total output allocation in bytes, including plane gaps and trailing alignment.
    pub data_len: u64,
    /// One of the `CVI_CAMERA_FORMAT_*` constants.
    pub format: u32,
    /// Number of populated component descriptors: one for grayscale or three for YUV.
    pub plane_count: u32,
    /// One of the `CVI_CAMERA_COLOR_RANGE_*` constants.
    pub color_range: u32,
    /// One of the `CVI_CAMERA_COLOR_MATRIX_*` constants.
    pub color_matrix: u32,
    /// One of the `CVI_CAMERA_CHROMA_SITING_*` constants.
    pub chroma_siting: u32,
    /// Must be zero.
    pub reserved: u32,
    /// Dimensions read from the JPEG frame header.
    pub source: CviCameraExtentV1,
    /// Meaningful output dimensions after scaling.
    pub visible: CviCameraExtentV1,
    /// Source dimensions rounded up to whole JPEG MCU blocks.
    pub source_aligned: CviCameraExtentV1,
    /// Hardware output dimensions before storage rounding.
    pub coded: CviCameraExtentV1,
    /// Stored luma dimensions including coded padding.
    pub storage: CviCameraExtentV1,
    /// Luma plane descriptor.
    pub y: CviCameraPlaneV1,
    /// Blue-difference chroma plane descriptor, or all zero for grayscale.
    pub cb: CviCameraPlaneV1,
    /// Red-difference chroma plane descriptor, or all zero for grayscale.
    pub cr: CviCameraPlaneV1,
}

/// Fixed-width version 1 payload used by both versioned camera ioctl commands.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct CviCameraRequestV1 {
    /// Versioned request header.
    pub header: CviCameraRequestHeaderV1,
    /// User virtual address of a submitted JPEG, or zero for camera capture.
    pub jpeg_ptr: u64,
    /// Submitted JPEG length in bytes, or zero for camera capture.
    pub jpeg_len: u64,
    /// User virtual address of the decoded output buffer.
    pub data_ptr: u64,
    /// Writable output buffer capacity in bytes.
    pub capacity: u64,
    /// Output layout populated by the driver.
    pub layout: CviCameraFrameLayoutV1,
    /// Must be all zero.
    pub reserved: [u64; 2],
}

/// Wire size of [`CviCameraRequestV1`].
pub const CVI_CAMERA_REQUEST_V1_SIZE: u32 = core::mem::size_of::<CviCameraRequestV1>() as u32;

const IOC_NRSHIFT: u32 = 0;
const IOC_TYPESHIFT: u32 = 8;
const IOC_SIZESHIFT: u32 = 16;
const IOC_DIRSHIFT: u32 = 30;
const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;

const fn iowr<T>(ty: u8, nr: u8) -> u32 {
    ((IOC_READ | IOC_WRITE) << IOC_DIRSHIFT)
        | ((ty as u32) << IOC_TYPESHIFT)
        | ((nr as u32) << IOC_NRSHIFT)
        | ((core::mem::size_of::<T>() as u32) << IOC_SIZESHIFT)
}

/// Versioned scaled-capture command encoded as `_IOWR('C', 0x10, CviCameraRequestV1)`.
pub const CVI_CAMERA_IOCTL_CAPTURE_SCALED: u32 =
    iowr::<CviCameraRequestV1>(CVI_CAMERA_IOCTL_MAGIC, CVI_CAMERA_IOCTL_CAPTURE_SCALED_NR);
/// Versioned submitted-JPEG command encoded as `_IOWR('C', 0x11, CviCameraRequestV1)`.
pub const CVI_CAMERA_IOCTL_DECODE_JPEG: u32 =
    iowr::<CviCameraRequestV1>(CVI_CAMERA_IOCTL_MAGIC, CVI_CAMERA_IOCTL_DECODE_JPEG_NR);

/// Error returned when a version 1 request or returned layout violates the wire contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CviCameraValidationError {
    /// The request version is not supported by this crate.
    #[error("unsupported camera UAPI version")]
    UnsupportedVersion,
    /// The request size does not match the frozen version 1 size.
    #[error("invalid camera UAPI request size")]
    InvalidSize,
    /// The request contains an unknown flag bit.
    #[error("invalid camera UAPI request flags")]
    InvalidFlags,
    /// The requested scale is not a version 1 scale constant.
    #[error("invalid camera scale")]
    InvalidScale,
    /// A field reserved for future versions is nonzero.
    #[error("reserved camera UAPI field is nonzero")]
    ReservedNotZero,
    /// A capture request unexpectedly carries a submitted JPEG.
    #[error("capture request must not contain submitted JPEG input")]
    UnexpectedJpegInput,
    /// A submitted-JPEG request has no readable JPEG buffer.
    #[error("submitted-JPEG request is missing its input buffer")]
    MissingJpegInput,
    /// A non-query request has no writable output buffer.
    #[error("camera request is missing its output buffer")]
    MissingDataBuffer,
    /// A user pointer plus its length cannot be represented as a `u64` range.
    #[error("camera user buffer range overflows")]
    UserRangeOverflow,
    /// The supplied output buffer is shorter than `layout.data_len`.
    #[error("camera output buffer is too short")]
    BufferTooShort,
    /// The returned output data length is zero.
    #[error("camera output data length is zero")]
    InvalidDataLength,
    /// The returned pixel format is not a version 1 format constant.
    #[error("invalid camera output format")]
    InvalidFormat,
    /// The returned number of planes does not match the pixel format.
    #[error("invalid camera output plane count")]
    InvalidPlaneCount,
    /// The returned color range is not a version 1 range constant.
    #[error("invalid camera output color range")]
    InvalidColorRange,
    /// The returned color matrix is not a version 1 matrix constant.
    #[error("invalid camera output color matrix")]
    InvalidColorMatrix,
    /// The returned chroma siting is not a version 1 siting constant.
    #[error("invalid camera output chroma siting")]
    InvalidChromaSiting,
    /// A returned frame extent is empty or violates its containing extent.
    #[error("invalid camera output extent")]
    InvalidExtent,
    /// A plane's dimensions do not agree with the frame format and dimensions.
    #[error("invalid camera output plane dimensions")]
    InvalidPlaneDimensions,
    /// A present plane has a stride shorter than one stored row.
    #[error("invalid camera output plane stride")]
    InvalidPlaneStride,
    /// A present plane length does not equal `stride * storage.height`.
    #[error("invalid camera output plane length")]
    InvalidPlaneLength,
    /// A plane offset plus length overflows `u64`.
    #[error("camera output plane range overflows")]
    PlaneRangeOverflow,
    /// A plane extends beyond `layout.data_len`.
    #[error("camera output plane is out of bounds")]
    PlaneOutOfBounds,
    /// Two populated output planes overlap.
    #[error("camera output planes overlap")]
    PlaneOverlap,
    /// A grayscale layout contains a nonzero chroma descriptor.
    #[error("grayscale camera output contains a chroma plane")]
    UnexpectedChromaPlane,
}

impl CviCameraRequestV1 {
    const fn base(scale: u32, data_ptr: u64, capacity: u64) -> Self {
        Self {
            header: CviCameraRequestHeaderV1 {
                version: CVI_CAMERA_UAPI_VERSION_V1,
                size: CVI_CAMERA_REQUEST_V1_SIZE,
                flags: 0,
                scale,
            },
            jpeg_ptr: 0,
            jpeg_len: 0,
            data_ptr,
            capacity,
            layout: CviCameraFrameLayoutV1 {
                data_len: 0,
                format: 0,
                plane_count: 0,
                color_range: 0,
                color_matrix: 0,
                chroma_siting: 0,
                reserved: 0,
                source: CviCameraExtentV1 {
                    width: 0,
                    height: 0,
                },
                visible: CviCameraExtentV1 {
                    width: 0,
                    height: 0,
                },
                source_aligned: CviCameraExtentV1 {
                    width: 0,
                    height: 0,
                },
                coded: CviCameraExtentV1 {
                    width: 0,
                    height: 0,
                },
                storage: CviCameraExtentV1 {
                    width: 0,
                    height: 0,
                },
                y: CviCameraPlaneV1 {
                    offset: 0,
                    len: 0,
                    stride: 0,
                    storage: CviCameraExtentV1 {
                        width: 0,
                        height: 0,
                    },
                    visible: CviCameraExtentV1 {
                        width: 0,
                        height: 0,
                    },
                    reserved: 0,
                },
                cb: CviCameraPlaneV1 {
                    offset: 0,
                    len: 0,
                    stride: 0,
                    storage: CviCameraExtentV1 {
                        width: 0,
                        height: 0,
                    },
                    visible: CviCameraExtentV1 {
                        width: 0,
                        height: 0,
                    },
                    reserved: 0,
                },
                cr: CviCameraPlaneV1 {
                    offset: 0,
                    len: 0,
                    stride: 0,
                    storage: CviCameraExtentV1 {
                        width: 0,
                        height: 0,
                    },
                    visible: CviCameraExtentV1 {
                        width: 0,
                        height: 0,
                    },
                    reserved: 0,
                },
            },
            reserved: [0; 2],
        }
    }

    /// Creates a scaled-capture request backed by the supplied output buffer.
    pub const fn new_capture(scale: u32, data_ptr: u64, capacity: u64) -> Self {
        Self::base(scale, data_ptr, capacity)
    }

    /// Creates a metadata-only scaled-capture request.
    pub const fn new_capture_query(scale: u32) -> Self {
        let mut request = Self::base(scale, 0, 0);
        request.header.flags = CVI_CAMERA_REQUEST_FLAG_QUERY_ONLY;
        request
    }

    /// Creates a request that decodes a submitted JPEG into the supplied output buffer.
    pub const fn new_decode(
        jpeg_ptr: u64,
        jpeg_len: u64,
        scale: u32,
        data_ptr: u64,
        capacity: u64,
    ) -> Self {
        let mut request = Self::base(scale, data_ptr, capacity);
        request.jpeg_ptr = jpeg_ptr;
        request.jpeg_len = jpeg_len;
        request
    }

    /// Creates a metadata-only request for a submitted JPEG.
    pub const fn new_decode_query(jpeg_ptr: u64, jpeg_len: u64, scale: u32) -> Self {
        let mut request = Self::new_decode(jpeg_ptr, jpeg_len, scale, 0, 0);
        request.header.flags = CVI_CAMERA_REQUEST_FLAG_QUERY_ONLY;
        request
    }

    /// Returns whether the request asks for metadata without an output copy.
    pub const fn is_query_only(&self) -> bool {
        self.header.flags & CVI_CAMERA_REQUEST_FLAG_QUERY_ONLY != 0
    }

    /// Validates the common version, size, flag, and scale header fields.
    pub fn validate_header(&self) -> Result<(), CviCameraValidationError> {
        if self.header.version != CVI_CAMERA_UAPI_VERSION_V1 {
            return Err(CviCameraValidationError::UnsupportedVersion);
        }
        if self.header.size != CVI_CAMERA_REQUEST_V1_SIZE {
            return Err(CviCameraValidationError::InvalidSize);
        }
        if self.header.flags & !CVI_CAMERA_REQUEST_FLAGS_V1 != 0 {
            return Err(CviCameraValidationError::InvalidFlags);
        }
        if !is_valid_scale(self.header.scale) {
            return Err(CviCameraValidationError::InvalidScale);
        }
        Ok(())
    }

    /// Validates all fields supplied to the scaled-capture command.
    pub fn validate_capture(&self) -> Result<(), CviCameraValidationError> {
        self.validate_common_input()?;
        if self.jpeg_ptr != 0 || self.jpeg_len != 0 {
            return Err(CviCameraValidationError::UnexpectedJpegInput);
        }
        Ok(())
    }

    /// Validates all fields supplied to the submitted-JPEG decode command.
    pub fn validate_decode(&self) -> Result<(), CviCameraValidationError> {
        self.validate_common_input()?;
        if self.jpeg_ptr == 0 || self.jpeg_len == 0 {
            return Err(CviCameraValidationError::MissingJpegInput);
        }
        checked_user_range(self.jpeg_ptr, self.jpeg_len)?;
        Ok(())
    }

    fn validate_common_input(&self) -> Result<(), CviCameraValidationError> {
        self.validate_header()?;
        if self.reserved != [0; 2]
            || self.layout.reserved != 0
            || self.layout.y.reserved != 0
            || self.layout.cb.reserved != 0
            || self.layout.cr.reserved != 0
        {
            return Err(CviCameraValidationError::ReservedNotZero);
        }

        if self.data_ptr == 0 || self.capacity == 0 {
            if self.is_query_only() && self.data_ptr == 0 && self.capacity == 0 {
                return Ok(());
            }
            return Err(CviCameraValidationError::MissingDataBuffer);
        }
        checked_user_range(self.data_ptr, self.capacity)
    }
}

impl CviCameraFrameLayoutV1 {
    /// Validates this layout against an accessible output buffer length.
    pub fn validate_for_buffer_len(&self, buffer_len: u64) -> Result<(), CviCameraValidationError> {
        if self.reserved != 0
            || self.y.reserved != 0
            || self.cb.reserved != 0
            || self.cr.reserved != 0
        {
            return Err(CviCameraValidationError::ReservedNotZero);
        }
        if self.data_len == 0 {
            return Err(CviCameraValidationError::InvalidDataLength);
        }
        if self.data_len > buffer_len {
            return Err(CviCameraValidationError::BufferTooShort);
        }
        if !is_valid_format(self.format) {
            return Err(CviCameraValidationError::InvalidFormat);
        }
        let expected_planes = if self.format == CVI_CAMERA_FORMAT_GRAYSCALE {
            1
        } else {
            3
        };
        if self.plane_count != expected_planes {
            return Err(CviCameraValidationError::InvalidPlaneCount);
        }
        if !matches!(
            self.color_range,
            CVI_CAMERA_COLOR_RANGE_UNSPECIFIED
                | CVI_CAMERA_COLOR_RANGE_FULL
                | CVI_CAMERA_COLOR_RANGE_LIMITED
        ) {
            return Err(CviCameraValidationError::InvalidColorRange);
        }
        if !matches!(
            self.color_matrix,
            CVI_CAMERA_COLOR_MATRIX_UNSPECIFIED
                | CVI_CAMERA_COLOR_MATRIX_BT601
                | CVI_CAMERA_COLOR_MATRIX_BT709
        ) {
            return Err(CviCameraValidationError::InvalidColorMatrix);
        }
        if !matches!(
            self.chroma_siting,
            CVI_CAMERA_CHROMA_SITING_UNSPECIFIED
                | CVI_CAMERA_CHROMA_SITING_CENTER
                | CVI_CAMERA_CHROMA_SITING_LEFT
                | CVI_CAMERA_CHROMA_SITING_TOP_LEFT
        ) {
            return Err(CviCameraValidationError::InvalidChromaSiting);
        }

        if !self.source.is_nonzero()
            || !self.visible.is_nonzero()
            || !self.source_aligned.is_nonzero()
            || !self.coded.is_nonzero()
            || !self.storage.is_nonzero()
            || !self.source.fits_within(self.source_aligned)
            || !self.visible.fits_within(self.coded)
            || !self.coded.fits_within(self.storage)
        {
            return Err(CviCameraValidationError::InvalidExtent);
        }

        if self.y.storage != self.storage || self.y.visible != self.visible {
            return Err(CviCameraValidationError::InvalidPlaneDimensions);
        }
        let y_end = validate_plane(&self.y, self.data_len)?;

        if self.format == CVI_CAMERA_FORMAT_GRAYSCALE {
            if !self.cb.is_zero() || !self.cr.is_zero() {
                return Err(CviCameraValidationError::UnexpectedChromaPlane);
            }
            return Ok(());
        }

        let (chroma_storage, chroma_visible) = self.expected_chroma_extents();
        if self.cb.storage != chroma_storage
            || self.cr.storage != chroma_storage
            || self.cb.visible != chroma_visible
            || self.cr.visible != chroma_visible
        {
            return Err(CviCameraValidationError::InvalidPlaneDimensions);
        }
        let cb_end = validate_plane(&self.cb, self.data_len)?;
        let cr_end = validate_plane(&self.cr, self.data_len)?;

        if ranges_overlap(self.y.offset, y_end, self.cb.offset, cb_end)
            || ranges_overlap(self.y.offset, y_end, self.cr.offset, cr_end)
            || ranges_overlap(self.cb.offset, cb_end, self.cr.offset, cr_end)
        {
            return Err(CviCameraValidationError::PlaneOverlap);
        }
        Ok(())
    }

    fn expected_chroma_extents(&self) -> (CviCameraExtentV1, CviCameraExtentV1) {
        match self.format {
            CVI_CAMERA_FORMAT_YUV420_PLANAR => {
                (ceil_half_both(self.storage), ceil_half_both(self.visible))
            }
            CVI_CAMERA_FORMAT_YUV422_HORIZONTAL_PLANAR => {
                (ceil_half_width(self.storage), ceil_half_width(self.visible))
            }
            CVI_CAMERA_FORMAT_YUV422_VERTICAL_PLANAR => (
                ceil_half_height(self.storage),
                ceil_half_height(self.visible),
            ),
            CVI_CAMERA_FORMAT_YUV444_PLANAR => (self.storage, self.visible),
            _ => unreachable!(),
        }
    }
}

const fn is_valid_scale(scale: u32) -> bool {
    matches!(
        scale,
        CVI_CAMERA_SCALE_FULL
            | CVI_CAMERA_SCALE_HALF
            | CVI_CAMERA_SCALE_QUARTER
            | CVI_CAMERA_SCALE_EIGHTH
    )
}

const fn is_valid_format(format: u32) -> bool {
    matches!(
        format,
        CVI_CAMERA_FORMAT_YUV420_PLANAR
            | CVI_CAMERA_FORMAT_YUV422_HORIZONTAL_PLANAR
            | CVI_CAMERA_FORMAT_YUV422_VERTICAL_PLANAR
            | CVI_CAMERA_FORMAT_YUV444_PLANAR
            | CVI_CAMERA_FORMAT_GRAYSCALE
    )
}

fn checked_user_range(address: u64, len: u64) -> Result<(), CviCameraValidationError> {
    address
        .checked_add(len)
        .ok_or(CviCameraValidationError::UserRangeOverflow)?;
    Ok(())
}

fn validate_plane(
    plane: &CviCameraPlaneV1,
    data_len: u64,
) -> Result<u64, CviCameraValidationError> {
    if !plane.storage.is_nonzero()
        || !plane.visible.is_nonzero()
        || !plane.visible.fits_within(plane.storage)
    {
        return Err(CviCameraValidationError::InvalidPlaneDimensions);
    }
    if plane.stride < plane.storage.width {
        return Err(CviCameraValidationError::InvalidPlaneStride);
    }
    let expected_len = u64::from(plane.stride) * u64::from(plane.storage.height);
    if plane.len != expected_len {
        return Err(CviCameraValidationError::InvalidPlaneLength);
    }
    let end = plane
        .offset
        .checked_add(plane.len)
        .ok_or(CviCameraValidationError::PlaneRangeOverflow)?;
    if end > data_len {
        return Err(CviCameraValidationError::PlaneOutOfBounds);
    }
    Ok(end)
}

const fn ranges_overlap(a_start: u64, a_end: u64, b_start: u64, b_end: u64) -> bool {
    a_start < b_end && b_start < a_end
}

const fn ceil_half(value: u32) -> u32 {
    (value / 2) + (value % 2)
}

const fn ceil_half_both(extent: CviCameraExtentV1) -> CviCameraExtentV1 {
    CviCameraExtentV1 {
        width: ceil_half(extent.width),
        height: ceil_half(extent.height),
    }
}

const fn ceil_half_width(extent: CviCameraExtentV1) -> CviCameraExtentV1 {
    CviCameraExtentV1 {
        width: ceil_half(extent.width),
        height: extent.height,
    }
}

const fn ceil_half_height(extent: CviCameraExtentV1) -> CviCameraExtentV1 {
    CviCameraExtentV1 {
        width: extent.width,
        height: ceil_half(extent.height),
    }
}

#[cfg(test)]
mod tests {
    use core::mem::{align_of, offset_of, size_of};

    use super::*;

    fn extent(width: u32, height: u32) -> CviCameraExtentV1 {
        CviCameraExtentV1 { width, height }
    }

    fn plane(
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

    fn valid_yuv420_layout() -> CviCameraFrameLayoutV1 {
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

    #[test]
    fn wire_sizes_alignments_and_offsets_are_frozen() {
        assert_eq!(size_of::<CviCameraRequestHeaderV1>(), 16);
        assert_eq!(align_of::<CviCameraRequestHeaderV1>(), 4);
        assert_eq!(offset_of!(CviCameraRequestHeaderV1, version), 0);
        assert_eq!(offset_of!(CviCameraRequestHeaderV1, size), 4);
        assert_eq!(offset_of!(CviCameraRequestHeaderV1, flags), 8);
        assert_eq!(offset_of!(CviCameraRequestHeaderV1, scale), 12);

        assert_eq!(size_of::<CviCameraExtentV1>(), 8);
        assert_eq!(align_of::<CviCameraExtentV1>(), 4);
        assert_eq!(offset_of!(CviCameraExtentV1, width), 0);
        assert_eq!(offset_of!(CviCameraExtentV1, height), 4);

        assert_eq!(size_of::<CviCameraPlaneV1>(), 40);
        assert_eq!(align_of::<CviCameraPlaneV1>(), 8);
        assert_eq!(offset_of!(CviCameraPlaneV1, offset), 0);
        assert_eq!(offset_of!(CviCameraPlaneV1, len), 8);
        assert_eq!(offset_of!(CviCameraPlaneV1, stride), 16);
        assert_eq!(offset_of!(CviCameraPlaneV1, storage), 20);
        assert_eq!(offset_of!(CviCameraPlaneV1, visible), 28);
        assert_eq!(offset_of!(CviCameraPlaneV1, reserved), 36);

        assert_eq!(size_of::<CviCameraFrameLayoutV1>(), 192);
        assert_eq!(align_of::<CviCameraFrameLayoutV1>(), 8);
        assert_eq!(offset_of!(CviCameraFrameLayoutV1, data_len), 0);
        assert_eq!(offset_of!(CviCameraFrameLayoutV1, format), 8);
        assert_eq!(offset_of!(CviCameraFrameLayoutV1, plane_count), 12);
        assert_eq!(offset_of!(CviCameraFrameLayoutV1, color_range), 16);
        assert_eq!(offset_of!(CviCameraFrameLayoutV1, color_matrix), 20);
        assert_eq!(offset_of!(CviCameraFrameLayoutV1, chroma_siting), 24);
        assert_eq!(offset_of!(CviCameraFrameLayoutV1, reserved), 28);
        assert_eq!(offset_of!(CviCameraFrameLayoutV1, source), 32);
        assert_eq!(offset_of!(CviCameraFrameLayoutV1, visible), 40);
        assert_eq!(offset_of!(CviCameraFrameLayoutV1, source_aligned), 48);
        assert_eq!(offset_of!(CviCameraFrameLayoutV1, coded), 56);
        assert_eq!(offset_of!(CviCameraFrameLayoutV1, storage), 64);
        assert_eq!(offset_of!(CviCameraFrameLayoutV1, y), 72);
        assert_eq!(offset_of!(CviCameraFrameLayoutV1, cb), 112);
        assert_eq!(offset_of!(CviCameraFrameLayoutV1, cr), 152);

        assert_eq!(size_of::<CviCameraRequestV1>(), 256);
        assert_eq!(align_of::<CviCameraRequestV1>(), 8);
        assert_eq!(offset_of!(CviCameraRequestV1, header), 0);
        assert_eq!(offset_of!(CviCameraRequestV1, jpeg_ptr), 16);
        assert_eq!(offset_of!(CviCameraRequestV1, jpeg_len), 24);
        assert_eq!(offset_of!(CviCameraRequestV1, data_ptr), 32);
        assert_eq!(offset_of!(CviCameraRequestV1, capacity), 40);
        assert_eq!(offset_of!(CviCameraRequestV1, layout), 48);
        assert_eq!(offset_of!(CviCameraRequestV1, reserved), 240);
    }

    #[test]
    fn ioctl_numbers_and_legacy_commands_are_frozen() {
        assert_eq!(CVI_CAMERA_IOCTL_INIT, 1);
        assert_eq!(CVI_CAMERA_IOCTL_GET_INFO, 2);
        assert_eq!(CVI_CAMERA_IOCTL_GET_FRAME, 3);
        assert_eq!(CVI_CAMERA_IOCTL_GET_YUV_FRAME, 4);
        assert_eq!(CVI_CAMERA_IOCTL_CAPTURE_SCALED, 0xC100_4310);
        assert_eq!(CVI_CAMERA_IOCTL_DECODE_JPEG, 0xC100_4311);
    }

    #[test]
    fn constructors_create_valid_capture_decode_and_query_requests() {
        let capture = CviCameraRequestV1::new_capture(CVI_CAMERA_SCALE_HALF, 0x1000, 4096);
        assert_eq!(capture.header.version, CVI_CAMERA_UAPI_VERSION_V1);
        assert_eq!(capture.header.size, 256);
        assert_eq!(capture.header.flags, 0);
        assert_eq!(capture.header.scale, CVI_CAMERA_SCALE_HALF);
        assert_eq!((capture.jpeg_ptr, capture.jpeg_len), (0, 0));
        assert_eq!((capture.data_ptr, capture.capacity), (0x1000, 4096));
        assert_eq!(capture.validate_capture(), Ok(()));

        let query = CviCameraRequestV1::new_capture_query(CVI_CAMERA_SCALE_EIGHTH);
        assert!(query.is_query_only());
        assert_eq!((query.data_ptr, query.capacity), (0, 0));
        assert_eq!(query.validate_capture(), Ok(()));

        let decode =
            CviCameraRequestV1::new_decode(0x2000, 1024, CVI_CAMERA_SCALE_QUARTER, 0x4000, 8192);
        assert_eq!((decode.jpeg_ptr, decode.jpeg_len), (0x2000, 1024));
        assert_eq!(decode.validate_decode(), Ok(()));

        let decode_query =
            CviCameraRequestV1::new_decode_query(0x2000, 1024, CVI_CAMERA_SCALE_FULL);
        assert!(decode_query.is_query_only());
        assert_eq!(decode_query.validate_decode(), Ok(()));
    }

    #[test]
    fn invalid_version_size_flags_and_scale_are_rejected() {
        let mut request = CviCameraRequestV1::new_capture(CVI_CAMERA_SCALE_FULL, 1, 1);
        request.header.version += 1;
        assert_eq!(
            request.validate_capture(),
            Err(CviCameraValidationError::UnsupportedVersion)
        );

        let mut request = CviCameraRequestV1::new_capture(CVI_CAMERA_SCALE_FULL, 1, 1);
        request.header.size -= 1;
        assert_eq!(
            request.validate_capture(),
            Err(CviCameraValidationError::InvalidSize)
        );

        let mut request = CviCameraRequestV1::new_capture(CVI_CAMERA_SCALE_FULL, 1, 1);
        request.header.flags = CVI_CAMERA_REQUEST_FLAG_QUERY_ONLY << 1;
        assert_eq!(
            request.validate_capture(),
            Err(CviCameraValidationError::InvalidFlags)
        );

        let mut request = CviCameraRequestV1::new_capture(CVI_CAMERA_SCALE_FULL, 1, 1);
        request.header.scale = 4;
        assert_eq!(
            request.validate_capture(),
            Err(CviCameraValidationError::InvalidScale)
        );
    }

    #[test]
    fn capture_and_decode_validate_their_input_buffers() {
        let mut capture = CviCameraRequestV1::new_capture(CVI_CAMERA_SCALE_FULL, 1, 1);
        capture.jpeg_ptr = 0x1000;
        capture.jpeg_len = 16;
        assert_eq!(
            capture.validate_capture(),
            Err(CviCameraValidationError::UnexpectedJpegInput)
        );

        let decode = CviCameraRequestV1::new_decode(0, 0, CVI_CAMERA_SCALE_FULL, 1, 1);
        assert_eq!(
            decode.validate_decode(),
            Err(CviCameraValidationError::MissingJpegInput)
        );

        let capture = CviCameraRequestV1::new_capture(CVI_CAMERA_SCALE_FULL, 0, 0);
        assert_eq!(
            capture.validate_capture(),
            Err(CviCameraValidationError::MissingDataBuffer)
        );
    }

    #[test]
    fn input_pointer_ranges_are_checked_for_overflow() {
        let capture = CviCameraRequestV1::new_capture(CVI_CAMERA_SCALE_FULL, u64::MAX - 7, 16);
        assert_eq!(
            capture.validate_capture(),
            Err(CviCameraValidationError::UserRangeOverflow)
        );

        let decode = CviCameraRequestV1::new_decode(u64::MAX - 7, 16, CVI_CAMERA_SCALE_FULL, 1, 1);
        assert_eq!(
            decode.validate_decode(),
            Err(CviCameraValidationError::UserRangeOverflow)
        );
    }

    #[test]
    fn all_fixed_reserved_fields_must_be_zero() {
        let mut request = CviCameraRequestV1::new_capture(CVI_CAMERA_SCALE_FULL, 1, 1);
        request.reserved[1] = 1;
        assert_eq!(
            request.validate_capture(),
            Err(CviCameraValidationError::ReservedNotZero)
        );

        let mut request = CviCameraRequestV1::new_capture(CVI_CAMERA_SCALE_FULL, 1, 1);
        request.layout.reserved = 1;
        assert_eq!(
            request.validate_capture(),
            Err(CviCameraValidationError::ReservedNotZero)
        );

        let mut request = CviCameraRequestV1::new_capture(CVI_CAMERA_SCALE_FULL, 1, 1);
        request.layout.cb.reserved = 1;
        assert_eq!(
            request.validate_capture(),
            Err(CviCameraValidationError::ReservedNotZero)
        );
    }

    #[test]
    fn layout_rejects_short_buffers() {
        let layout = valid_yuv420_layout();
        assert_eq!(
            layout.validate_for_buffer_len(layout.data_len - 1),
            Err(CviCameraValidationError::BufferTooShort)
        );
        assert_eq!(layout.validate_for_buffer_len(layout.data_len), Ok(()));
    }

    #[test]
    fn layout_rejects_plane_range_overflow() {
        let mut layout = valid_yuv420_layout();
        layout.data_len = u64::MAX;
        layout.cr.offset = u64::MAX - 255;
        layout.cr.len = 512;
        assert_eq!(
            layout.validate_for_buffer_len(u64::MAX),
            Err(CviCameraValidationError::PlaneRangeOverflow)
        );
    }

    #[test]
    fn layout_rejects_invalid_stride_and_length() {
        let mut layout = valid_yuv420_layout();
        layout.y.stride = layout.y.storage.width - 1;
        assert_eq!(
            layout.validate_for_buffer_len(layout.data_len),
            Err(CviCameraValidationError::InvalidPlaneStride)
        );

        let mut layout = valid_yuv420_layout();
        layout.y.len -= 1;
        assert_eq!(
            layout.validate_for_buffer_len(layout.data_len),
            Err(CviCameraValidationError::InvalidPlaneLength)
        );
    }

    #[test]
    fn layout_rejects_planes_outside_data_len() {
        let mut layout = valid_yuv420_layout();
        layout.cr.offset = layout.data_len - 256;
        assert_eq!(
            layout.validate_for_buffer_len(layout.data_len),
            Err(CviCameraValidationError::PlaneOutOfBounds)
        );
    }

    #[test]
    fn layout_rejects_overlapping_planes() {
        let mut layout = valid_yuv420_layout();
        layout.cb.offset = layout.y.len - 8;
        assert_eq!(
            layout.validate_for_buffer_len(layout.data_len),
            Err(CviCameraValidationError::PlaneOverlap)
        );
    }

    #[test]
    fn yuv420_storage_chroma_uses_ceil_division_for_odd_extents() {
        let mut layout = valid_yuv420_layout();
        layout.storage = extent(65, 33);
        layout.y.storage = layout.storage;
        layout.y.stride = 65;
        layout.y.len = 65 * 33;
        layout.cb.offset = layout.y.len;
        layout.cb.storage = extent(33, 17);
        layout.cb.stride = 33;
        layout.cb.len = 33 * 17;
        layout.cr.offset = layout.cb.offset + layout.cb.len;
        layout.cr.storage = extent(33, 17);
        layout.cr.stride = 33;
        layout.cr.len = 33 * 17;
        layout.data_len = layout.cr.offset + layout.cr.len;

        assert_eq!(layout.validate_for_buffer_len(layout.data_len), Ok(()));
    }

    #[test]
    fn grayscale_requires_one_plane_and_zero_chroma_descriptors() {
        let mut layout = valid_yuv420_layout();
        layout.format = CVI_CAMERA_FORMAT_GRAYSCALE;
        layout.plane_count = 1;
        layout.cb = CviCameraPlaneV1::default();
        layout.cr = CviCameraPlaneV1::default();
        layout.data_len = layout.y.len;
        assert_eq!(layout.validate_for_buffer_len(layout.data_len), Ok(()));

        layout.plane_count = 3;
        assert_eq!(
            layout.validate_for_buffer_len(layout.data_len),
            Err(CviCameraValidationError::InvalidPlaneCount)
        );
    }
}
