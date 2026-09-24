use std::{error::Error, fmt, fs::File, path::Path, time::Instant};

use image::{
    DynamicImage, ExtendedColorType, ImageFormat, Rgb, RgbImage, codecs::jpeg::JpegEncoder,
};
use zune_core::{bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions};
use zune_jpeg::JpegDecoder;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ImagePreprocessTiming {
    pub src_w: i32,
    pub src_h: i32,
    /// JPEG decode microseconds.
    pub decode_us: i64,
    /// Resize, letterbox clear, and planar pack microseconds.
    pub resize_us: i64,
}

#[derive(Debug)]
pub enum ImageBridgeError {
    InvalidInput(&'static str),
    JpegDecode(zune_jpeg::errors::DecodeErrors),
    ImageDecode(image::ImageError),
    Save(image::ImageError),
    Io(std::io::Error),
}

impl fmt::Display for ImageBridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(message) => f.write_str(message),
            Self::JpegDecode(err) => write!(f, "JPEG decode failed: {err}"),
            Self::ImageDecode(err) => write!(f, "image decode failed: {err}"),
            Self::Save(err) => write!(f, "image save failed: {err}"),
            Self::Io(err) => write!(f, "image I/O failed: {err}"),
        }
    }
}

impl Error for ImageBridgeError {}

/// Reusable MJPEG preprocessor for the TPU hot path.
///
/// The buffers and interpolation tables are kept across frames to avoid
/// per-frame allocation. Keep one instance per model/input stream.
#[derive(Default)]
pub struct ImagePreprocessor {
    decode_buffer: Vec<u8>,
    x_cache: InterpolationCache,
    y_cache: InterpolationCache,
}

impl ImagePreprocessor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mjpeg_to_rgb_planar(
        &mut self,
        jpeg: &[u8],
        dst: &mut [u8],
        dst_w: i32,
        dst_h: i32,
    ) -> Result<ImagePreprocessTiming, ImageBridgeError> {
        if jpeg.is_empty() {
            return Err(ImageBridgeError::InvalidInput("empty JPEG input"));
        }
        let (dst_w, dst_h) = valid_dimensions(dst_w, dst_h)?;
        let channel_size = checked_image_len(dst_w, dst_h, 1)?;
        let required_len = checked_image_len(dst_w, dst_h, 3)?;
        if dst.len() < required_len {
            return Err(ImageBridgeError::InvalidInput(
                "destination tensor buffer is too small",
            ));
        }

        let t0 = Instant::now();
        let (src_w, src_h) = decode_jpeg_rgb_into(jpeg, &mut self.decode_buffer)?;
        let decode_us = elapsed_us(t0);

        let src_w_i32 = i32::try_from(src_w)
            .map_err(|_| ImageBridgeError::InvalidInput("source image width exceeds i32"))?;
        let src_h_i32 = i32::try_from(src_h)
            .map_err(|_| ImageBridgeError::InvalidInput("source image height exceeds i32"))?;

        let scale = (dst_w as f64 / src_w as f64).min(dst_h as f64 / src_h as f64);
        let resized_w = ((src_w as f64 * scale) as u32).max(1);
        let resized_h = ((src_h as f64 * scale) as u32).max(1);
        let pad_left = (dst_w - resized_w) / 2;
        let pad_top = (dst_h - resized_h) / 2;

        let t1 = Instant::now();
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
        resize_pack_rgb_planar(
            &self.decode_buffer,
            src_w,
            src_h,
            dst,
            dst_w,
            resized_w,
            resized_h,
            channel_size,
            pad_left,
            pad_top,
            &mut self.x_cache,
            &mut self.y_cache,
        );
        let resize_us = elapsed_us(t1);

        Ok(ImagePreprocessTiming {
            src_w: src_w_i32,
            src_h: src_h_i32,
            decode_us,
            resize_us,
        })
    }
}

pub fn draw_detections(
    image: &[u8],
    detections: &[crate::detector::Detection],
    out_path: &Path,
) -> Result<(), ImageBridgeError> {
    if image.is_empty() {
        return Err(ImageBridgeError::InvalidInput("empty image input"));
    }

    let mut rgb = image::load_from_memory(image)
        .map_err(ImageBridgeError::ImageDecode)?
        .into_rgb8();
    for detection in detections {
        draw_detection(&mut rgb, detection);
    }
    save_rgb_image(&rgb, out_path)
}

fn valid_dimensions(width: i32, height: i32) -> Result<(u32, u32), ImageBridgeError> {
    if width <= 0 || height <= 0 {
        return Err(ImageBridgeError::InvalidInput(
            "image dimensions must be positive",
        ));
    }
    Ok((width as u32, height as u32))
}

fn checked_image_len(width: u32, height: u32, channels: usize) -> Result<usize, ImageBridgeError> {
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(channels))
        .ok_or(ImageBridgeError::InvalidInput(
            "image dimensions overflow usize",
        ))
}

fn decode_jpeg_rgb_into(jpeg: &[u8], out: &mut Vec<u8>) -> Result<(u32, u32), ImageBridgeError> {
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(jpeg), options);
    decoder
        .decode_headers()
        .map_err(ImageBridgeError::JpegDecode)?;
    let info = decoder
        .info()
        .ok_or(ImageBridgeError::InvalidInput("JPEG metadata is missing"))?;
    let output_size = decoder
        .output_buffer_size()
        .ok_or(ImageBridgeError::InvalidInput("JPEG output size overflow"))?;
    out.resize(output_size, 0);
    decoder
        .decode_into(out)
        .map_err(ImageBridgeError::JpegDecode)?;
    Ok((info.width as u32, info.height as u32))
}

pub(crate) fn resize_pack_rgb_planar(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    dst: &mut [u8],
    dst_w: u32,
    resized_w: u32,
    resized_h: u32,
    channel_size: usize,
    pad_left: u32,
    pad_top: u32,
    x_cache: &mut InterpolationCache,
    y_cache: &mut InterpolationCache,
) {
    if src_w == resized_w && src_h == resized_h {
        pack_rgb_planar_copy(
            src,
            src_w,
            src_h,
            dst,
            dst_w,
            channel_size,
            pad_left,
            pad_top,
        );
        return;
    }

    let x_table = x_cache.ensure(src_w, resized_w);
    let y_table = y_cache.ensure(src_h, resized_h);
    let (r_plane, rest) = dst.split_at_mut(channel_size);
    let (g_plane, b_plane) = rest.split_at_mut(channel_size);

    for (dy, y) in y_table.iter().enumerate() {
        let dst_row = (pad_top as usize + dy) * dst_w as usize + pad_left as usize;
        let src_row0 = y.start as usize * src_w as usize * 3;
        let src_row1 = y.end as usize * src_w as usize * 3;
        for (dx, x) in x_table.iter().enumerate() {
            let dst_idx = dst_row + dx;
            let src00 = src_row0 + x.start as usize * 3;
            let src01 = src_row0 + x.end as usize * 3;
            let src10 = src_row1 + x.start as usize * 3;
            let src11 = src_row1 + x.end as usize * 3;

            r_plane[dst_idx] =
                interpolate_channel(src, src00, src01, src10, src11, x.weight, y.weight, 0);
            g_plane[dst_idx] =
                interpolate_channel(src, src00, src01, src10, src11, x.weight, y.weight, 1);
            b_plane[dst_idx] =
                interpolate_channel(src, src00, src01, src10, src11, x.weight, y.weight, 2);
        }
    }
}

fn pack_rgb_planar_copy(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    dst: &mut [u8],
    dst_w: u32,
    channel_size: usize,
    pad_left: u32,
    pad_top: u32,
) {
    let src_w = src_w as usize;
    let src_h = src_h as usize;
    let dst_w = dst_w as usize;
    let pad_left = pad_left as usize;
    let pad_top = pad_top as usize;
    let (r_plane, rest) = dst.split_at_mut(channel_size);
    let (g_plane, b_plane) = rest.split_at_mut(channel_size);

    for y in 0..src_h {
        let src_row = y * src_w * 3;
        let dst_row = (pad_top + y) * dst_w + pad_left;
        for x in 0..src_w {
            let src_idx = src_row + x * 3;
            let dst_idx = dst_row + x;
            r_plane[dst_idx] = src[src_idx];
            g_plane[dst_idx] = src[src_idx + 1];
            b_plane[dst_idx] = src[src_idx + 2];
        }
    }
}

pub(crate) fn clear_letterbox_padding(
    dst: &mut [u8],
    dst_w: u32,
    dst_h: u32,
    resized_w: u32,
    resized_h: u32,
    channel_size: usize,
    pad_left: u32,
    pad_top: u32,
) {
    let dst_w = dst_w as usize;
    let dst_h = dst_h as usize;
    let resized_w = resized_w as usize;
    let resized_h = resized_h as usize;
    let pad_left = pad_left as usize;
    let pad_top = pad_top as usize;
    let pad_bottom = pad_top + resized_h;
    let pad_right = pad_left + resized_w;

    for plane in dst[..channel_size * 3].chunks_exact_mut(channel_size) {
        plane[..pad_top * dst_w].fill(0);
        plane[pad_bottom * dst_w..dst_h * dst_w].fill(0);
        if pad_left == 0 && pad_right == dst_w {
            continue;
        }
        for row in pad_top..pad_bottom {
            let row_start = row * dst_w;
            plane[row_start..row_start + pad_left].fill(0);
            plane[row_start + pad_right..row_start + dst_w].fill(0);
        }
    }
}

#[derive(Default)]
pub(crate) struct InterpolationCache {
    table: Vec<Interpolation>,
    key: Option<(u32, u32)>,
}

impl InterpolationCache {
    pub(crate) fn ensure(&mut self, src_len: u32, dst_len: u32) -> &[Interpolation] {
        if self.key == Some((src_len, dst_len)) {
            return &self.table;
        }

        self.key = Some((src_len, dst_len));
        self.table.clear();
        self.table.reserve(dst_len as usize);
        let scale = src_len as f64 / dst_len as f64;
        let last = src_len - 1;
        self.table.extend((0..dst_len).map(|dst| {
            let src = (dst as f64 + 0.5) * scale - 0.5;
            if src <= 0.0 {
                return Interpolation {
                    start: 0,
                    end: 0,
                    weight: 0,
                };
            }
            let start = src.floor() as u32;
            if start >= last {
                return Interpolation {
                    start: last,
                    end: last,
                    weight: 0,
                };
            }
            Interpolation {
                start,
                end: start + 1,
                weight: ((src - start as f64) * 256.0).round() as u32,
            }
        }));
        &self.table
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Interpolation {
    pub(crate) start: u32,
    pub(crate) end: u32,
    pub(crate) weight: u32,
}

fn interpolate_channel(
    src: &[u8],
    src00: usize,
    src01: usize,
    src10: usize,
    src11: usize,
    wx: u32,
    wy: u32,
    channel: usize,
) -> u8 {
    let iw_x = 256 - wx;
    let iw_y = 256 - wy;
    let value = src[src00 + channel] as u32 * iw_x * iw_y
        + src[src01 + channel] as u32 * wx * iw_y
        + src[src10 + channel] as u32 * iw_x * wy
        + src[src11 + channel] as u32 * wx * wy;
    ((value + 32_768) >> 16) as u8
}

fn draw_detection(image: &mut RgbImage, detection: &crate::detector::Detection) {
    let cx = detection.bbox.x;
    let cy = detection.bbox.y;
    let w = detection.bbox.w;
    let h = detection.bbox.h;
    let x1 = (cx - w / 2.0) as i32;
    let y1 = (cy - h / 2.0) as i32;
    let x2 = (cx + w / 2.0) as i32;
    let y2 = (cy + h / 2.0) as i32;

    let green = Rgb([0, 255, 0]);
    draw_hollow_rect(image, x1, y1, x2, y2, green, 2);

    let label = format!("{} {:.2}", detection.cls, detection.score);
    let (text_w, text_h) = text_size(&label, 2);
    let label_top = (y1 - text_h as i32 - 2).max(0);
    draw_filled_rect(
        image,
        x1,
        label_top,
        x1 + text_w as i32 + 2,
        label_top + text_h as i32 + 2,
        green,
    );
    draw_text(image, x1 + 1, label_top + 1, &label, Rgb([0, 0, 0]), 2);
}

fn draw_hollow_rect(
    image: &mut RgbImage,
    x1: i32,
    y1: i32,
    x2: i32,
    y2: i32,
    color: Rgb<u8>,
    thickness: i32,
) {
    if x1 > x2 || y1 > y2 || thickness <= 0 {
        return;
    }
    for offset in 0..thickness {
        draw_horizontal_line(image, x1, x2, y1 + offset, color);
        draw_horizontal_line(image, x1, x2, y2 - offset, color);
        draw_vertical_line(image, x1 + offset, y1, y2, color);
        draw_vertical_line(image, x2 - offset, y1, y2, color);
    }
}

fn draw_horizontal_line(image: &mut RgbImage, x1: i32, x2: i32, y: i32, color: Rgb<u8>) {
    if y < 0 || y >= image.height() as i32 {
        return;
    }
    let start = x1.max(0) as u32;
    let end = x2.min(image.width() as i32 - 1);
    if end < start as i32 {
        return;
    }
    for x in start..=end as u32 {
        image.put_pixel(x, y as u32, color);
    }
}

fn draw_vertical_line(image: &mut RgbImage, x: i32, y1: i32, y2: i32, color: Rgb<u8>) {
    if x < 0 || x >= image.width() as i32 {
        return;
    }
    let start = y1.max(0) as u32;
    let end = y2.min(image.height() as i32 - 1);
    if end < start as i32 {
        return;
    }
    for y in start..=end as u32 {
        image.put_pixel(x as u32, y, color);
    }
}

fn draw_filled_rect(image: &mut RgbImage, x1: i32, y1: i32, x2: i32, y2: i32, color: Rgb<u8>) {
    let x_start = x1.max(0) as u32;
    let y_start = y1.max(0) as u32;
    let x_end = x2.min(image.width() as i32 - 1);
    let y_end = y2.min(image.height() as i32 - 1);
    if x_end < x_start as i32 || y_end < y_start as i32 {
        return;
    }
    for y in y_start..=y_end as u32 {
        for x in x_start..=x_end as u32 {
            image.put_pixel(x, y, color);
        }
    }
}

fn text_size(text: &str, scale: i32) -> (u32, u32) {
    let glyph_width = (FONT_WIDTH + 1) * scale;
    let width = text.chars().count() as i32 * glyph_width;
    ((width.max(0)) as u32, (FONT_HEIGHT * scale) as u32)
}

fn draw_text(image: &mut RgbImage, x: i32, y: i32, text: &str, color: Rgb<u8>, scale: i32) {
    let mut cursor_x = x;
    for ch in text.chars() {
        draw_glyph(image, cursor_x, y, ch, color, scale);
        cursor_x += (FONT_WIDTH + 1) * scale;
    }
}

fn draw_glyph(image: &mut RgbImage, x: i32, y: i32, ch: char, color: Rgb<u8>, scale: i32) {
    let glyph = glyph_rows(ch);
    for (row_idx, row) in glyph.iter().enumerate() {
        for col in 0..FONT_WIDTH {
            if row & (1 << (FONT_WIDTH - 1 - col)) == 0 {
                continue;
            }
            let px = x + col * scale;
            let py = y + row_idx as i32 * scale;
            draw_filled_rect(image, px, py, px + scale - 1, py + scale - 1, color);
        }
    }
}

fn glyph_rows(ch: char) -> [u8; FONT_HEIGHT as usize] {
    match ch {
        '0' => [
            0b11111, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b11111,
        ],
        '1' => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        '2' => [
            0b11110, 0b00001, 0b00001, 0b11110, 0b10000, 0b10000, 0b11111,
        ],
        '3' => [
            0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        '4' => [
            0b10010, 0b10010, 0b10010, 0b11111, 0b00010, 0b00010, 0b00010,
        ],
        '5' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b00001, 0b00001, 0b11110,
        ],
        '6' => [
            0b01111, 0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        '7' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        '8' => [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        '9' => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001, 0b11110,
        ],
        '-' => [
            0b00000, 0b00000, 0b00000, 0b11110, 0b00000, 0b00000, 0b00000,
        ],
        '.' => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b01100,
        ],
        ' ' => [0; FONT_HEIGHT as usize],
        _ => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b00100, 0b00000, 0b00100,
        ],
    }
}

fn save_rgb_image(image: &RgbImage, out_path: &Path) -> Result<(), ImageBridgeError> {
    let format = ImageFormat::from_path(out_path).map_err(ImageBridgeError::Save)?;
    if format == ImageFormat::Jpeg {
        let file = File::create(out_path).map_err(ImageBridgeError::Io)?;
        JpegEncoder::new_with_quality(file, 95)
            .encode(
                image.as_raw(),
                image.width(),
                image.height(),
                ExtendedColorType::Rgb8,
            )
            .map_err(ImageBridgeError::Save)?;
        return Ok(());
    }
    DynamicImage::ImageRgb8(image.clone())
        .save_with_format(out_path, format)
        .map_err(ImageBridgeError::Save)
}

pub(crate) fn elapsed_us(start: Instant) -> i64 {
    start.elapsed().as_micros() as i64
}

const FONT_WIDTH: i32 = 5;
const FONT_HEIGHT: i32 = 7;

#[cfg(test)]
mod tests {
    use image::{ExtendedColorType, Rgb, RgbImage, codecs::jpeg::JpegEncoder};

    use super::{ImagePreprocessor, draw_detection, pack_rgb_planar_copy, text_size};
    use crate::detector::{Box2d, Detection};

    #[test]
    fn letterboxes_and_packs_rgb_planes() {
        let jpeg = encode_test_jpeg(4, 2, |x, y| Rgb([x as u8 * 40, y as u8 * 70, 200]));
        let mut dst = vec![255; 6 * 6 * 3];
        let mut preprocessor = ImagePreprocessor::new();
        let timing = preprocessor
            .mjpeg_to_rgb_planar(&jpeg, &mut dst, 6, 6)
            .unwrap();

        assert_eq!(timing.src_w, 4);
        assert_eq!(timing.src_h, 2);

        let plane = 6 * 6;
        assert_eq!(dst[0], 0);
        assert_eq!(dst[plane], 0);
        assert_eq!(dst[plane * 2], 0);
        assert!(dst[2 * 6 + 3] > 0);
        assert!(dst[plane + 2 * 6 + 3] > 0);
        assert!(dst[plane * 2 + 2 * 6 + 3] > 0);
    }

    #[test]
    fn identity_path_packs_without_interpolation() {
        let src = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120];
        let mut dst = vec![255; 4 * 4 * 3];
        let channel_size = 4 * 4;

        pack_rgb_planar_copy(&src, 2, 2, &mut dst, 4, channel_size, 1, 1);

        assert_eq!(dst[5], 10);
        assert_eq!(dst[6], 40);
        assert_eq!(dst[9], 70);
        assert_eq!(dst[10], 100);
        assert_eq!(dst[channel_size + 5], 20);
        assert_eq!(dst[channel_size + 6], 50);
        assert_eq!(dst[channel_size * 2 + 5], 30);
        assert_eq!(dst[channel_size * 2 + 10], 120);
    }

    #[test]
    fn rejects_small_destination_buffer() {
        let jpeg = encode_test_jpeg(1, 1, |_, _| Rgb([255, 0, 0]));
        let mut dst = vec![0; 2];
        let mut preprocessor = ImagePreprocessor::new();
        assert!(
            preprocessor
                .mjpeg_to_rgb_planar(&jpeg, &mut dst, 1, 1)
                .is_err()
        );
    }

    #[test]
    fn draws_green_detection_box_and_label_background() {
        let mut image = RgbImage::from_pixel(20, 20, Rgb([0, 0, 0]));
        let detection = Detection {
            bbox: Box2d {
                x: 10.0,
                y: 10.0,
                w: 8.0,
                h: 8.0,
            },
            cls: 0,
            score: 0.9,
            batch_idx: 0,
        };

        draw_detection(&mut image, &detection);

        assert_eq!(*image.get_pixel(6, 6), Rgb([0, 255, 0]));
        assert_eq!(*image.get_pixel(6, 0), Rgb([0, 255, 0]));
        assert!(text_size("0 0.90", 2).0 > 0);
    }

    fn encode_test_jpeg(width: u32, height: u32, pixel: impl Fn(u32, u32) -> Rgb<u8>) -> Vec<u8> {
        let mut image = RgbImage::new(width, height);
        for y in 0..height {
            for x in 0..width {
                image.put_pixel(x, y, pixel(x, y));
            }
        }
        let mut out = Vec::new();
        JpegEncoder::new_with_quality(&mut out, 100)
            .encode(image.as_raw(), width, height, ExtendedColorType::Rgb8)
            .unwrap();
        out
    }
}
