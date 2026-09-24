//! Checked planar YUV420 input for the SG2002 JPU preprocessing path.

use std::{error::Error, fmt, time::Instant};

use crate::image_bridge::{
    ImagePreprocessTiming, InterpolationCache, clear_letterbox_padding, elapsed_us,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageSize {
    pub width: u32,
    pub height: u32,
}

impl ImageSize {
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlaneLayout {
    pub offset: usize,
    pub len: usize,
    pub stride: usize,
    pub visible: ImageSize,
    pub storage: ImageSize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Yuv420Layout {
    pub source: ImageSize,
    pub visible: ImageSize,
    pub storage: ImageSize,
    pub y: PlaneLayout,
    pub cb: PlaneLayout,
    pub cr: PlaneLayout,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Yuv420Error {
    InvalidLayout(&'static str),
    DestinationTooSmall,
}

impl fmt::Display for Yuv420Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLayout(message) => write!(f, "invalid YUV420 layout: {message}"),
            Self::DestinationTooSmall => f.write_str("destination tensor buffer is too small"),
        }
    }
}

impl Error for Yuv420Error {}

#[derive(Clone, Copy, Debug)]
pub struct PlanarYuv420<'a> {
    bytes: &'a [u8],
    layout: Yuv420Layout,
}

impl<'a> PlanarYuv420<'a> {
    pub fn new(bytes: &'a [u8], layout: Yuv420Layout) -> Result<Self, Yuv420Error> {
        validate_frame_extents(layout)?;
        validate_plane_extents(layout)?;
        let y_range = validate_plane(bytes, layout.y, "Y")?;
        let cb_range = validate_plane(bytes, layout.cb, "Cb")?;
        let cr_range = validate_plane(bytes, layout.cr, "Cr")?;
        if ranges_overlap(&y_range, &cb_range)
            || ranges_overlap(&y_range, &cr_range)
            || ranges_overlap(&cb_range, &cr_range)
        {
            return Err(Yuv420Error::InvalidLayout("frame planes overlap"));
        }
        Ok(Self { bytes, layout })
    }

    pub const fn layout(self) -> Yuv420Layout {
        self.layout
    }
}

/// Reusable, allocation-free conversion from JPU YUV420 output to the model's
/// letterboxed planar RGB tensor.
#[derive(Default)]
pub struct Yuv420Preprocessor {
    x_cache: InterpolationCache,
    y_cache: InterpolationCache,
}

impl Yuv420Preprocessor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn preprocess_into(
        &mut self,
        frame: PlanarYuv420<'_>,
        dst: &mut [u8],
        dst_w: i32,
        dst_h: i32,
        decode_us: i64,
    ) -> Result<ImagePreprocessTiming, Yuv420Error> {
        let (dst_w, dst_h) = checked_destination(dst, dst_w, dst_h)?;
        let channel_size = checked_area(dst_w, dst_h)?;
        let layout = frame.layout;
        let src_w = i32::try_from(layout.source.width)
            .map_err(|_| Yuv420Error::InvalidLayout("source width exceeds i32"))?;
        let src_h = i32::try_from(layout.source.height)
            .map_err(|_| Yuv420Error::InvalidLayout("source height exceeds i32"))?;
        // Use the original JPEG aspect ratio so hardware scale rounding (for
        // example 1279 -> 640 at Half) cannot shift the model letterbox by a
        // pixel relative to the software baseline.
        let scale = (dst_w as f64 / layout.source.width as f64)
            .min(dst_h as f64 / layout.source.height as f64);
        let resized_w = ((layout.source.width as f64 * scale) as u32).max(1);
        let resized_h = ((layout.source.height as f64 * scale) as u32).max(1);
        let pad_left = (dst_w - resized_w) / 2;
        let pad_top = (dst_h - resized_h) / 2;

        let started = Instant::now();
        clear_letterbox_padding(
            dst,
            dst_w,
            dst_h,
            resized_w,
            resized_h,
            channel_size,
            pad_left,
            pad_top,
        );
        let x_table = self.x_cache.ensure(layout.visible.width, resized_w);
        let y_table = self.y_cache.ensure(layout.visible.height, resized_h);
        let (r_plane, rest) = dst.split_at_mut(channel_size);
        let (g_plane, b_plane) = rest.split_at_mut(channel_size);

        for (dy, y) in y_table.iter().enumerate() {
            let dst_row = (pad_top as usize + dy) * dst_w as usize + pad_left as usize;
            for (dx, x) in x_table.iter().enumerate() {
                let rgb00 = sample_rgb(frame, x.start, y.start);
                let rgb01 = sample_rgb(frame, x.end, y.start);
                let rgb10 = sample_rgb(frame, x.start, y.end);
                let rgb11 = sample_rgb(frame, x.end, y.end);
                let dst_index = dst_row + dx;
                r_plane[dst_index] =
                    interpolate_values(rgb00[0], rgb01[0], rgb10[0], rgb11[0], x.weight, y.weight);
                g_plane[dst_index] =
                    interpolate_values(rgb00[1], rgb01[1], rgb10[1], rgb11[1], x.weight, y.weight);
                b_plane[dst_index] =
                    interpolate_values(rgb00[2], rgb01[2], rgb10[2], rgb11[2], x.weight, y.weight);
            }
        }

        Ok(ImagePreprocessTiming {
            src_w,
            src_h,
            decode_us,
            resize_us: elapsed_us(started),
        })
    }
}

fn checked_destination(dst: &[u8], dst_w: i32, dst_h: i32) -> Result<(u32, u32), Yuv420Error> {
    if dst_w <= 0 || dst_h <= 0 {
        return Err(Yuv420Error::InvalidLayout(
            "destination dimensions must be positive",
        ));
    }
    let dst_w = dst_w as u32;
    let dst_h = dst_h as u32;
    let required = checked_area(dst_w, dst_h)?
        .checked_mul(3)
        .ok_or(Yuv420Error::InvalidLayout(
            "destination dimensions overflow usize",
        ))?;
    if dst.len() < required {
        return Err(Yuv420Error::DestinationTooSmall);
    }
    Ok((dst_w, dst_h))
}

fn checked_area(width: u32, height: u32) -> Result<usize, Yuv420Error> {
    (width as usize)
        .checked_mul(height as usize)
        .ok_or(Yuv420Error::InvalidLayout(
            "destination dimensions overflow usize",
        ))
}

fn sample_rgb(frame: PlanarYuv420<'_>, x: u32, y: u32) -> [u8; 3] {
    let y_value = sample_plane(frame.bytes, frame.layout.y, x, y);
    let cb = sample_centered_chroma(frame.bytes, frame.layout.cb, x, y);
    let cr = sample_centered_chroma(frame.bytes, frame.layout.cr, x, y);
    yuv_to_rgb(y_value, cb, cr)
}

fn sample_plane(bytes: &[u8], plane: PlaneLayout, x: u32, y: u32) -> u8 {
    bytes[plane.offset + y as usize * plane.stride + x as usize]
}

fn sample_centered_chroma(bytes: &[u8], plane: PlaneLayout, x: u32, y: u32) -> u8 {
    let x = centered_chroma_axis(x, plane.visible.width);
    let y = centered_chroma_axis(y, plane.visible.height);
    let value00 = sample_plane(bytes, plane, x.0, y.0);
    let value01 = sample_plane(bytes, plane, x.1, y.0);
    let value10 = sample_plane(bytes, plane, x.0, y.1);
    let value11 = sample_plane(bytes, plane, x.1, y.1);
    interpolate_values(value00, value01, value10, value11, x.2, y.2)
}

/// Map an integer luma coordinate to JPEG/JFIF centered 2:1 chroma samples.
fn centered_chroma_axis(luma: u32, chroma_len: u32) -> (u32, u32, u32) {
    debug_assert!(chroma_len > 0);
    if luma == 0 || chroma_len == 1 {
        return (0, 0, 0);
    }
    // Chroma coordinate is (luma - 0.5) / 2. Express it in quarters to
    // avoid floating point and retain the existing 8-bit interpolation grid.
    let quarter_units = luma.saturating_mul(2) - 1;
    let start = quarter_units / 4;
    let last = chroma_len - 1;
    if start >= last {
        return (last, last, 0);
    }
    (start, start + 1, quarter_units % 4 * 64)
}

fn interpolate_values(value00: u8, value01: u8, value10: u8, value11: u8, wx: u32, wy: u32) -> u8 {
    let inverse_x = 256 - wx;
    let inverse_y = 256 - wy;
    let value = value00 as u32 * inverse_x * inverse_y
        + value01 as u32 * wx * inverse_y
        + value10 as u32 * inverse_x * wy
        + value11 as u32 * wx * wy;
    ((value + 32_768) >> 16) as u8
}

/// JPEG/JFIF YCbCr (full range, BT.601 coefficients) to 8-bit RGB.
fn yuv_to_rgb(y: u8, cb: u8, cr: u8) -> [u8; 3] {
    const SHIFT: i32 = 14;
    const HALF: i32 = 1 << (SHIFT - 1);
    let y = i32::from(y) << SHIFT;
    let cb = i32::from(cb) - 128;
    let cr = i32::from(cr) - 128;
    let red = (y + 22_970 * cr + HALF) >> SHIFT;
    let green = (y - 5_638 * cb - 11_700 * cr + HALF) >> SHIFT;
    let blue = (y + 29_032 * cb + HALF) >> SHIFT;
    [clamp_u8(red), clamp_u8(green), clamp_u8(blue)]
}

fn clamp_u8(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

fn validate_frame_extents(layout: Yuv420Layout) -> Result<(), Yuv420Error> {
    for size in [layout.source, layout.visible, layout.storage] {
        if size.width == 0 || size.height == 0 {
            return Err(Yuv420Error::InvalidLayout(
                "frame dimensions must be non-zero",
            ));
        }
    }
    if layout.visible.width > layout.storage.width || layout.visible.height > layout.storage.height
    {
        return Err(Yuv420Error::InvalidLayout(
            "visible frame exceeds storage dimensions",
        ));
    }
    Ok(())
}

fn validate_plane_extents(layout: Yuv420Layout) -> Result<(), Yuv420Error> {
    if layout.y.visible != layout.visible || layout.y.storage != layout.storage {
        return Err(Yuv420Error::InvalidLayout(
            "Y dimensions do not match the frame",
        ));
    }
    let chroma_visible = ImageSize::new(
        layout.visible.width.div_ceil(2),
        layout.visible.height.div_ceil(2),
    );
    let chroma_storage = ImageSize::new(
        layout.storage.width.div_ceil(2),
        layout.storage.height.div_ceil(2),
    );
    for plane in [layout.cb, layout.cr] {
        if plane.visible != chroma_visible || plane.storage != chroma_storage {
            return Err(Yuv420Error::InvalidLayout(
                "chroma dimensions do not match YUV420 subsampling",
            ));
        }
    }
    Ok(())
}

fn validate_plane(
    bytes: &[u8],
    plane: PlaneLayout,
    name: &'static str,
) -> Result<core::ops::Range<usize>, Yuv420Error> {
    let storage_width = usize::try_from(plane.storage.width)
        .map_err(|_| Yuv420Error::InvalidLayout("plane width exceeds usize"))?;
    let storage_height = usize::try_from(plane.storage.height)
        .map_err(|_| Yuv420Error::InvalidLayout("plane height exceeds usize"))?;
    if plane.stride < storage_width {
        return Err(Yuv420Error::InvalidLayout(match name {
            "Y" => "Y stride is too small",
            "Cb" => "Cb stride is too small",
            _ => "Cr stride is too small",
        }));
    }
    let required_len = plane
        .stride
        .checked_mul(storage_height)
        .ok_or(Yuv420Error::InvalidLayout("plane length overflows usize"))?;
    if plane.len < required_len {
        return Err(Yuv420Error::InvalidLayout(match name {
            "Y" => "Y plane is too short",
            "Cb" => "Cb plane is too short",
            _ => "Cr plane is too short",
        }));
    }
    let end = plane
        .offset
        .checked_add(plane.len)
        .ok_or(Yuv420Error::InvalidLayout("plane range overflows usize"))?;
    if end > bytes.len() {
        return Err(Yuv420Error::InvalidLayout("plane exceeds frame buffer"));
    }
    Ok(plane.offset..end)
}

fn ranges_overlap(left: &core::ops::Range<usize>, right: &core::ops::Range<usize>) -> bool {
    left.start < right.end && right.start < left.end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_padded_odd_visible_frame() {
        let layout = odd_padded_layout();
        let bytes = vec![0u8; 64];

        let frame = PlanarYuv420::new(&bytes, layout);

        assert!(frame.is_ok());
    }

    #[test]
    fn rejects_stride_smaller_than_storage_width() {
        let mut layout = odd_padded_layout();
        layout.y.stride = 5;
        let bytes = vec![0u8; 64];

        let error = PlanarYuv420::new(&bytes, layout).unwrap_err();

        assert_eq!(error, Yuv420Error::InvalidLayout("Y stride is too small"));
    }

    #[test]
    fn rejects_overlapping_planes() {
        let mut layout = odd_padded_layout();
        layout.cb.offset = 30;
        let bytes = vec![0u8; 64];

        let error = PlanarYuv420::new(&bytes, layout).unwrap_err();

        assert_eq!(error, Yuv420Error::InvalidLayout("frame planes overlap"));
    }

    #[test]
    fn rejects_plane_shorter_than_padded_storage() {
        let mut layout = odd_padded_layout();
        layout.cb.len = 7;
        let bytes = vec![0u8; 64];

        let error = PlanarYuv420::new(&bytes, layout).unwrap_err();

        assert_eq!(error, Yuv420Error::InvalidLayout("Cb plane is too short"));
    }

    #[test]
    fn converts_neutral_chroma_without_limited_range_offset() {
        let layout = tightly_packed_layout(ImageSize::new(2, 2));
        let bytes = [16, 64, 128, 235, 128, 128];
        let frame = PlanarYuv420::new(&bytes, layout).unwrap();
        let mut dst = [0u8; 12];
        let mut preprocessor = Yuv420Preprocessor::new();

        preprocessor
            .preprocess_into(frame, &mut dst, 2, 2, 17)
            .unwrap();

        assert_eq!(&dst[..4], &[16, 64, 128, 235]);
        assert_eq!(&dst[4..8], &[16, 64, 128, 235]);
        assert_eq!(&dst[8..], &[16, 64, 128, 235]);
    }

    #[test]
    fn converts_bt601_full_range_red_and_blue_vectors() {
        assert_rgb_close(yuv_to_rgb(76, 85, 255), [254, 0, 0], 1);
        assert_rgb_close(yuv_to_rgb(29, 255, 107), [0, 0, 254], 1);
    }

    #[test]
    fn storage_padding_does_not_affect_visible_output() {
        let layout = odd_padded_layout();
        let mut first = vec![0x11; 64];
        let mut second = vec![0xee; 64];
        write_visible_samples(&mut first, layout);
        write_visible_samples(&mut second, layout);
        let mut first_dst = vec![0; 5 * 3 * 3];
        let mut second_dst = vec![0; 5 * 3 * 3];
        let mut preprocessor = Yuv420Preprocessor::new();

        preprocessor
            .preprocess_into(
                PlanarYuv420::new(&first, layout).unwrap(),
                &mut first_dst,
                5,
                3,
                0,
            )
            .unwrap();
        preprocessor
            .preprocess_into(
                PlanarYuv420::new(&second, layout).unwrap(),
                &mut second_dst,
                5,
                3,
                0,
            )
            .unwrap();

        assert_eq!(first_dst, second_dst);
    }

    #[test]
    fn fused_resize_matches_rgb_intermediate_oracle() {
        use crate::image_bridge::{
            InterpolationCache, clear_letterbox_padding, resize_pack_rgb_planar,
        };

        let layout = odd_padded_layout();
        let mut bytes = vec![0x77; 64];
        write_visible_samples(&mut bytes, layout);
        let frame = PlanarYuv420::new(&bytes, layout).unwrap();
        let mut fused = vec![0; 7 * 7 * 3];
        let mut preprocessor = Yuv420Preprocessor::new();
        preprocessor
            .preprocess_into(frame, &mut fused, 7, 7, 0)
            .unwrap();

        let mut rgb = Vec::with_capacity(5 * 3 * 3);
        for y in 0..layout.visible.height {
            for x in 0..layout.visible.width {
                rgb.extend_from_slice(&sample_rgb(frame, x, y));
            }
        }
        let mut oracle = vec![0xff; 7 * 7 * 3];
        let channel_size = 7 * 7;
        let resized_w = 7;
        let resized_h = 4;
        let pad_left = 0;
        let pad_top = 1;
        clear_letterbox_padding(
            &mut oracle,
            7,
            7,
            resized_w,
            resized_h,
            channel_size,
            pad_left,
            pad_top,
        );
        resize_pack_rgb_planar(
            &rgb,
            5,
            3,
            &mut oracle,
            7,
            resized_w,
            resized_h,
            channel_size,
            pad_left,
            pad_top,
            &mut InterpolationCache::default(),
            &mut InterpolationCache::default(),
        );

        assert_eq!(fused, oracle);
    }

    #[test]
    fn centered_chroma_uses_quarter_and_three_quarter_weights() {
        assert_eq!(centered_chroma_axis(0, 3), (0, 0, 0));
        assert_eq!(centered_chroma_axis(1, 3), (0, 1, 64));
        assert_eq!(centered_chroma_axis(2, 3), (0, 1, 192));
        assert_eq!(centered_chroma_axis(3, 3), (1, 2, 64));
        assert_eq!(centered_chroma_axis(5, 3), (2, 2, 0));
    }

    #[test]
    fn letterbox_geometry_uses_original_source_aspect() {
        let visible = ImageSize::new(3, 4);
        let mut layout = tightly_packed_layout(visible);
        layout.source = ImageSize::new(5, 7);
        let bytes = vec![128u8; 3 * 4 + 2 * 2 * 2];
        let frame = PlanarYuv420::new(&bytes, layout).unwrap();
        let mut dst = vec![0xff; 8 * 8 * 3];

        Yuv420Preprocessor::new()
            .preprocess_into(frame, &mut dst, 8, 8, 0)
            .unwrap();

        let first_row = &dst[..8];
        assert_eq!(first_row, &[0, 128, 128, 128, 128, 128, 0, 0]);
    }

    #[test]
    fn rejects_destination_that_cannot_hold_three_planes() {
        let layout = tightly_packed_layout(ImageSize::new(2, 2));
        let bytes = [128u8; 6];
        let frame = PlanarYuv420::new(&bytes, layout).unwrap();
        let mut dst = [0u8; 11];
        let mut preprocessor = Yuv420Preprocessor::new();

        let error = preprocessor
            .preprocess_into(frame, &mut dst, 2, 2, 0)
            .unwrap_err();

        assert_eq!(error, Yuv420Error::DestinationTooSmall);
    }

    #[test]
    fn rejects_plane_range_overflow() {
        let mut layout = tightly_packed_layout(ImageSize::new(2, 2));
        layout.cr.offset = usize::MAX;
        let bytes = [128u8; 6];

        let error = PlanarYuv420::new(&bytes, layout).unwrap_err();

        assert_eq!(
            error,
            Yuv420Error::InvalidLayout("plane range overflows usize")
        );
    }

    fn odd_padded_layout() -> Yuv420Layout {
        Yuv420Layout {
            source: ImageSize::new(5, 3),
            visible: ImageSize::new(5, 3),
            storage: ImageSize::new(6, 4),
            y: PlaneLayout {
                offset: 0,
                len: 32,
                stride: 8,
                visible: ImageSize::new(5, 3),
                storage: ImageSize::new(6, 4),
            },
            cb: PlaneLayout {
                offset: 32,
                len: 16,
                stride: 4,
                visible: ImageSize::new(3, 2),
                storage: ImageSize::new(3, 2),
            },
            cr: PlaneLayout {
                offset: 48,
                len: 16,
                stride: 4,
                visible: ImageSize::new(3, 2),
                storage: ImageSize::new(3, 2),
            },
        }
    }

    fn tightly_packed_layout(visible: ImageSize) -> Yuv420Layout {
        let chroma = ImageSize::new(visible.width.div_ceil(2), visible.height.div_ceil(2));
        let y_len = visible.width as usize * visible.height as usize;
        let chroma_len = chroma.width as usize * chroma.height as usize;
        Yuv420Layout {
            source: visible,
            visible,
            storage: visible,
            y: PlaneLayout {
                offset: 0,
                len: y_len,
                stride: visible.width as usize,
                visible,
                storage: visible,
            },
            cb: PlaneLayout {
                offset: y_len,
                len: chroma_len,
                stride: chroma.width as usize,
                visible: chroma,
                storage: chroma,
            },
            cr: PlaneLayout {
                offset: y_len + chroma_len,
                len: chroma_len,
                stride: chroma.width as usize,
                visible: chroma,
                storage: chroma,
            },
        }
    }

    fn write_visible_samples(bytes: &mut [u8], layout: Yuv420Layout) {
        for y in 0..layout.y.visible.height as usize {
            for x in 0..layout.y.visible.width as usize {
                bytes[layout.y.offset + y * layout.y.stride + x] = (32 + x + y * 7) as u8;
            }
        }
        for plane in [layout.cb, layout.cr] {
            for y in 0..plane.visible.height as usize {
                for x in 0..plane.visible.width as usize {
                    bytes[plane.offset + y * plane.stride + x] = (96 + x * 13 + y * 5) as u8;
                }
            }
        }
    }

    fn assert_rgb_close(actual: [u8; 3], expected: [u8; 3], tolerance: u8) {
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!(
                actual.abs_diff(expected) <= tolerance,
                "{actual} != {expected}"
            );
        }
    }
}
