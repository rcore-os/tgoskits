#![cfg_attr(target_os = "none", no_std)]
#![cfg(not(target_os = "none"))]

use std::path::PathBuf;

use crab_uvc::{UncompressedFormat, VideoFormat, VideoFormatType};
use ffmpeg_next::{Stream, codec::Context};
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use tokio::fs;

#[derive(Debug, Deserialize, Serialize)]
pub struct VideoInfo {
    pub width: usize,
    pub height: usize,
    pub fps: usize,
    pub pixel: Pixel,
}

#[derive(Debug, Deserialize, Serialize)]
pub enum Pixel {
    Yuy2,
    Nv12,
    Rgb24,
    Rgb32,
    Mjpeg,
    H264,
}

pub struct Parser {
    input_dir: PathBuf,
    output_dir: PathBuf,
}

impl Parser {
    pub async fn new(input_dir: PathBuf, output_dir: PathBuf) -> Self {
        // 创建输出目录
        if let Err(e) = fs::create_dir_all(&output_dir).await {
            warn!("Failed to create output directory: {e:?}");
        }
        if let Err(e) = fs::create_dir_all(&input_dir).await {
            warn!("Failed to create input directory: {e:?}");
        }

        Self {
            input_dir,
            output_dir,
        }
    }

    /// 保存原始帧数据到文件
    pub async fn save_frame_to_file(
        &self,
        frame: &crab_uvc::frame::FrameEvent,
        frame_number: u32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use tokio::{fs::File, io::AsyncWriteExt};

        // 保存为原始数据文件，便于后续处理
        let filename = self
            .input_dir
            .join(format!("frame_{:06}.raw", frame_number));
        let mut file = File::create(&filename).await?;
        file.write_all(&frame.data).await?;
        debug!("Saved frame {} to {:?}", frame_number, filename);
        Ok(())
    }

    /// 写入视频格式信息文件为 TOML 格式
    pub async fn write_format_info(
        &self,
        video_format: &VideoFormat,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (width, height, pixel) = match video_format {
            VideoFormat {
                width,
                height,
                format_type: VideoFormatType::Uncompressed(format_type),
                ..
            } => {
                let pixel = match format_type {
                    UncompressedFormat::Yuy2 => Pixel::Yuy2,
                    UncompressedFormat::Nv12 => Pixel::Nv12,
                    UncompressedFormat::Rgb24 => Pixel::Rgb24,
                    UncompressedFormat::Rgb32 => Pixel::Rgb32,
                };
                (*width as usize, *height as usize, pixel)
            }
            VideoFormat {
                width,
                height,
                format_type: VideoFormatType::Mjpeg,
                ..
            } => (*width as usize, *height as usize, Pixel::Mjpeg),
            VideoFormat {
                width,
                height,
                format_type: VideoFormatType::H264,
                ..
            } => (*width as usize, *height as usize, Pixel::H264),
        };

        // 获取帧率，优先使用参数传入的值，否则从 video_format 中获取
        let default_fps = video_format.frame_rate as f32;
        let fps_value = default_fps;

        let video_info = VideoInfo {
            width,
            height,
            fps: fps_value as usize,
            pixel,
        };

        // 序列化为 TOML 格式
        let toml_content = toml::to_string_pretty(&video_info)?;

        let filename = self.input_dir.join("video_info.toml");
        tokio::fs::write(&filename, toml_content).await?;

        info!(
            "Video info written to {:?}: {}x{}, {:?}, {}fps",
            filename, video_info.width, video_info.height, video_info.pixel, fps_value
        );

        Ok(())
    }

    /// 从 TOML 文件读取视频格式信息
    pub async fn read_format_info(&self) -> Result<VideoInfo, Box<dyn std::error::Error>> {
        let filename = self.input_dir.join("video_info.toml");
        let content = tokio::fs::read_to_string(&filename).await?;
        let video_info: VideoInfo = toml::from_str(&content)?;

        info!("Video info loaded from {:?}: {:?}", filename, video_info);
        Ok(video_info)
    }

    /// 从帧数据创建视频
    pub async fn create_video_from_frames(
        &self,
        frame_numbers: &[u32],
        fps: f32,
        video_format: &VideoFormat,
    ) -> Result<(), Box<dyn std::error::Error>> {
        info!(
            "Creating video with {} frames at {:.2} fps",
            frame_numbers.len(),
            fps
        );
        info!("Video format: {:?}", video_format);

        // 根据 VideoFormat 确定 FFmpeg 参数
        let (width, height, pixel_format) = match video_format {
            VideoFormat {
                width,
                height,
                format_type: VideoFormatType::Uncompressed(format_type),
                ..
            } => {
                let ffmpeg_format = match format_type {
                    UncompressedFormat::Yuy2 => "yuyv422",
                    UncompressedFormat::Nv12 => "nv12",
                    UncompressedFormat::Rgb24 => "rgb24",
                    UncompressedFormat::Rgb32 => "rgba",
                };
                (*width, *height, ffmpeg_format)
            }
            VideoFormat {
                width,
                height,
                format_type: VideoFormatType::Mjpeg,
                ..
            } => {
                // MJPEG 数据直接从帧解码，不需要指定像素格式
                return self
                    .create_video_from_mjpeg_frames(frame_numbers, fps, *width, *height)
                    .await;
            }
            VideoFormat {
                width,
                height,
                format_type: VideoFormatType::H264,
                ..
            } => {
                // H.264 数据直接从帧解码
                return self
                    .create_video_from_h264_frames(frame_numbers, fps, *width, *height)
                    .await;
            }
        };

        info!(
            "Using FFmpeg parameters: {}x{}, format: {}",
            width, height, pixel_format
        );

        let input_dir = self.input_dir.clone();
        let output_dir = self.output_dir.clone();

        // 使用 ffmpeg-next 从原始帧创建视频
        match tokio::task::spawn_blocking(
            move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
                use ffmpeg_next::{
                    Rational, codec, encoder,
                    format::{Pixel, output},
                };

                ffmpeg_next::init()?;

                // 创建输出上下文
                let output_path = output_dir.join("output.mp4");
                let mut output_ctx = output(output_path.to_str().unwrap())?;
                let mut output_stream = output_ctx.add_stream(encoder::find(codec::Id::H264))?;
                let mut encoder = codec(&output_stream)?.encoder().video()?;

                // 设置编码器参数
                encoder.set_width(width as u32);
                encoder.set_height(height as u32);
                encoder.set_format(Pixel::YUV420P);
                encoder.set_time_base(Rational(1, (fps as i32).max(1)));
                encoder.set_frame_rate(Some(Rational((fps as i32).max(1), 1)));

                let encoder = encoder.open_as(encoder::find(codec::Id::H264))?;
                output_stream.set_parameters(&encoder);

                output_ctx.write_header()?;

                // 由于原始视频格式需要特殊处理，我们需要手动读取和解码数据
                // 这里简化处理，实际上需要根据 pixel_format 来正确解码原始数据

                // 读取原始帧数据并编码
                for i in 0..100u32 {
                    // 假设最多100帧
                    let frame_path = input_dir.join(format!("frame_{:06}.raw", i));
                    if frame_path.exists() {
                        // 这里需要根据实际的像素格式来处理原始数据
                        // 由于复杂性，可能需要外部工具或更复杂的处理
                        info!("Processing frame: {:?}", frame_path);
                    } else {
                        break;
                    }
                }

                output_ctx.write_trailer()?;
                Ok(())
            },
        )
        .await
        {
            Ok(Ok(())) => {
                info!("Video created successfully using ffmpeg-next!");
            }
            Ok(Err(e)) => {
                warn!("ffmpeg-next failed: {:?}", e);

                // 如果直接转换失败，尝试另一种方法：先转换为图片再合成视频
                info!("Trying alternative approach: convert to images first");
                self.convert_raw_to_images(frame_numbers, video_format)
                    .await?;
                self.create_video_from_images(fps).await?;
            }
            Err(e) => {
                warn!("Task failed: {:?}", e);

                // 如果直接转换失败，尝试另一种方法：先转换为图片再合成视频
                info!("Trying alternative approach: convert to images first");
                self.convert_raw_to_images(frame_numbers, video_format)
                    .await?;
            }
        }

        Ok(())
    }

    /// 从MJPEG帧创建视频
    async fn create_video_from_mjpeg_frames(
        &self,
        _frame_numbers: &[u32],
        fps: f32,
        width: u16,
        height: u16,
    ) -> Result<(), Box<dyn std::error::Error>> {
        info!("Creating video from MJPEG frames: {}x{}", width, height);

        let input_dir = self.input_dir.clone();
        let output_dir = self.output_dir.clone();

        // 使用 ffmpeg-next 处理 MJPEG 帧
        match tokio::task::spawn_blocking(
            move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
                use std::{fs::File, io::Read};

                use ffmpeg_next::{
                    Rational, codec, encoder,
                    format::{Pixel, output},
                };

                ffmpeg_next::init()?;

                // 创建输出上下文
                let output_path = output_dir.join("output_mjpeg.mp4");
                let mut output_ctx = output(output_path.to_str().unwrap())?;
                let mut output_stream = output_ctx.add_stream(encoder::find(codec::Id::H264))?;
                let mut encoder = codec(&output_stream)?.encoder().video()?;

                // 设置编码器参数
                encoder.set_width(width as u32);
                encoder.set_height(height as u32);
                encoder.set_format(Pixel::YUV420P);
                encoder.set_time_base(Rational(1, (fps as i32).max(1)));
                encoder.set_frame_rate(Some(Rational((fps as i32).max(1), 1)));

                let mut encoder = encoder.open_as(encoder::find(codec::Id::H264))?;
                output_stream.set_parameters(&encoder);

                output_ctx.write_header()?;

                // 处理每个MJPEG帧文件
                let mut frame_count = 0i64;
                for i in 0u32..100 {
                    // 假设最多100帧
                    let frame_path = input_dir.join(format!("frame_{:06}.raw", i));
                    if let Ok(mut file) = File::open(&frame_path) {
                        let mut buffer = Vec::new();
                        if file.read_to_end(&mut buffer).is_ok() && !buffer.is_empty() {
                            // 检查这是否是JPEG数据（以FF D8开头）
                            if buffer.len() >= 2 && buffer[0] == 0xFF && buffer[1] == 0xD8 {
                                // 这是JPEG数据，我们需要解码它
                                // 创建临时文件来解码JPEG
                                let temp_jpeg_path = format!("/tmp/temp_frame_{}.jpg", i);
                                std::fs::write(&temp_jpeg_path, &buffer)?;

                                // 使用ffmpeg解码JPEG
                                use ffmpeg_next::format::input;
                                let mut input_ctx = input(&temp_jpeg_path)?;
                                let input_stream_index = {
                                    let input_stream = input_ctx
                                        .streams()
                                        .best(ffmpeg_next::media::Type::Video)
                                        .ok_or("No video stream found")?;
                                    input_stream.index()
                                };

                                let mut decoder = {
                                    let input_stream =
                                        input_ctx.stream(input_stream_index).unwrap();
                                    codec(&input_stream)?.decoder().video()?
                                };

                                for (stream, packet) in input_ctx.packets() {
                                    if stream.index() == input_stream_index {
                                        decoder.send_packet(&packet)?;
                                        let mut decoded =
                                            ffmpeg_next::util::frame::video::Video::empty();
                                        while decoder.receive_frame(&mut decoded).is_ok() {
                                            decoded.set_pts(Some(frame_count));
                                            frame_count += 1;

                                            let mut encoded = ffmpeg_next::Packet::empty();
                                            encoder.send_frame(&decoded)?;
                                            while encoder.receive_packet(&mut encoded).is_ok() {
                                                encoded.set_stream(0);
                                                encoded.write_interleaved(&mut output_ctx)?;
                                            }
                                        }
                                    }
                                }

                                // 清理临时文件
                                let _ = std::fs::remove_file(&temp_jpeg_path);
                            }
                        }
                    } else {
                        break; // 没有更多帧文件
                    }
                }

                // 刷新编码器
                encoder.send_eof()?;
                let mut encoded = ffmpeg_next::Packet::empty();
                while encoder.receive_packet(&mut encoded).is_ok() {
                    encoded.set_stream(0);
                    encoded.write_interleaved(&mut output_ctx)?;
                }

                output_ctx.write_trailer()?;
                Ok(())
            },
        )
        .await
        {
            Ok(Ok(())) => {
                info!("MJPEG video created successfully using ffmpeg-next!");
                Ok(())
            }
            Ok(Err(e)) => Err(format!("ffmpeg-next failed for MJPEG: {:?}", e).into()),
            Err(e) => Err(format!("Task failed for MJPEG: {:?}", e).into()),
        }
    }

    /// 从H.264帧创建视频
    async fn create_video_from_h264_frames(
        &self,
        _frame_numbers: &[u32],
        fps: f32,
        width: u16,
        height: u16,
    ) -> Result<(), Box<dyn std::error::Error>> {
        info!("Creating video from H.264 frames: {}x{}", width, height);

        let input_dir = self.input_dir.clone();
        let output_dir = self.output_dir.clone();

        // 使用 ffmpeg-next 处理 H.264 帧
        match tokio::task::spawn_blocking(
            move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
                use std::{fs::File, io::Read};

                use ffmpeg_next::{Packet, Rational, codec, encoder, format::output};

                ffmpeg_next::init()?;

                // 对于 H.264 原始帧，我们需要读取每个帧文件并创建 MP4 容器
                let output_path = output_dir.join("output_h264.mp4");
                let mut octx = output(output_path.to_str().unwrap())?;

                // 添加视频流 - H.264 编码流，用于复制模式
                let stream_index = {
                    let mut stream = octx.add_stream(encoder::find(codec::Id::H264))?;

                    // 设置流参数（对于原始 H.264 流复制）
                    stream.set_time_base(Rational(1, (fps as i32).max(1)));

                    stream.index()
                };

                octx.write_header()?;

                // 读取每个原始 H.264 帧文件并作为数据包写入
                for i in 0u32..100 {
                    // 假设最多100帧
                    let frame_path = input_dir.join(format!("frame_{:06}.raw", i));
                    if let Ok(mut file) = File::open(&frame_path) {
                        let mut buffer = Vec::new();
                        if file.read_to_end(&mut buffer).is_ok() && !buffer.is_empty() {
                            // 使用 copy 方法创建包含数据的包
                            let mut packet = Packet::copy(&buffer);
                            packet.set_stream(stream_index);
                            packet.set_pts(Some(i as i64));
                            packet.set_dts(Some(i as i64));

                            // 使用 write_interleaved 而不是 write_frame
                            packet.write_interleaved(&mut octx)?;
                        }
                    } else {
                        break; // 没有更多帧文件
                    }
                }

                octx.write_trailer()?;
                Ok(())
            },
        )
        .await
        {
            Ok(Ok(())) => {
                info!("H.264 video created successfully using ffmpeg-next!");
                Ok(())
            }
            Ok(Err(e)) => Err(format!("ffmpeg-next failed for H.264: {:?}", e).into()),
            Err(e) => Err(format!("Task failed for H.264: {:?}", e).into()),
        }
    }

    /// 将原始帧数据转换为图片
    pub async fn convert_raw_to_images(
        &self,
        frame_numbers: &[u32],
        video_format: &VideoFormat,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use tokio::fs;

        // 创建图片输出目录
        let images_dir = self.output_dir.join("images");
        fs::create_dir_all(&images_dir).await?;

        let (width, height, format_info) = match video_format {
            VideoFormat {
                width,
                height,
                format_type: VideoFormatType::Uncompressed(format_type),
                ..
            } => (*width, *height, format!("{:?}", format_type)),
            VideoFormat {
                width,
                height,
                format_type: VideoFormatType::Mjpeg,
                ..
            } => (*width, *height, "MJPEG".to_string()),
            VideoFormat {
                width,
                height,
                format_type: VideoFormatType::H264,
                ..
            } => (*width, *height, "H264".to_string()),
        };

        info!(
            "Converting frames to images: {}x{}, format: {}",
            width, height, format_info
        );

        for &frame_num in frame_numbers {
            let raw_file = self.input_dir.join(format!("frame_{:06}.raw", frame_num));
            let png_file = images_dir.join(format!("frame_{:06}.png", frame_num));

            // 读取原始数据
            if let Ok(raw_data) = fs::read(&raw_file).await {
                // 这里需要根据实际的图像格式进行转换
                info!(
                    "Converting frame {} to PNG (size: {} bytes)",
                    frame_num,
                    raw_data.len()
                );

                if let Err(e) = self
                    .convert_raw_to_png(
                        &raw_data,
                        &png_file.to_string_lossy(),
                        width,
                        height,
                        video_format,
                    )
                    .await
                {
                    warn!("Failed to convert frame {}: {:?}", frame_num, e);
                }
            }
        }

        Ok(())
    }

    /// 将原始数据转换为PNG格式
    async fn convert_raw_to_png(
        &self,
        raw_data: &[u8],
        output_path: &str,
        width: u16,
        height: u16,
        video_format: &VideoFormat,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use tokio::{fs::File, io::AsyncWriteExt};

        match video_format {
            VideoFormat {
                format_type: VideoFormatType::Uncompressed(format_type),
                ..
            } => {
                // 对于未压缩格式，我们可以尝试使用 image crate 进行转换
                match format_type {
                    UncompressedFormat::Yuy2 => {
                        // YUY2 (YUYV) 到 RGB 的转换
                        if let Ok(rgb_data) =
                            self.convert_yuyv_to_rgb(raw_data, width as usize, height as usize)
                        {
                            self.save_rgb_as_png(
                                &rgb_data,
                                output_path,
                                width as u32,
                                height as u32,
                            )
                            .await?;
                        } else {
                            // 如果转换失败，保存原始数据
                            let mut file = File::create(output_path).await?;
                            file.write_all(raw_data).await?;
                        }
                    }
                    UncompressedFormat::Rgb24 => {
                        // RGB24 直接转 PNG
                        self.save_rgb_as_png(raw_data, output_path, width as u32, height as u32)
                            .await?;
                    }
                    _ => {
                        // 其他格式暂时保存为原始数据
                        let mut file = File::create(output_path).await?;
                        file.write_all(raw_data).await?;
                    }
                }
            }
            VideoFormat {
                format_type: VideoFormatType::Mjpeg,
                ..
            } => {
                // MJPEG 数据可以直接保存为 JPEG 文件
                let jpeg_path = output_path.replace(".png", ".jpg");
                let mut file = File::create(&jpeg_path).await?;
                file.write_all(raw_data).await?;

                // 尝试用 FFmpeg 转换为 PNG
                if self
                    .convert_jpeg_to_png(&jpeg_path, output_path)
                    .await
                    .is_err()
                {
                    // 如果转换失败，至少我们有 JPEG 文件
                    debug!("Kept JPEG file: {}", jpeg_path);
                }
            }
            VideoFormat {
                format_type: VideoFormatType::H264,
                ..
            } => {
                // H.264 帧需要特殊处理，暂时保存原始数据
                let mut file = File::create(output_path).await?;
                file.write_all(raw_data).await?;
            }
        }

        Ok(())
    }

    /// YUYV转RGB格式转换
    fn convert_yuyv_to_rgb(
        &self,
        yuyv_data: &[u8],
        width: usize,
        height: usize,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        if yuyv_data.len() < width * height * 2 {
            return Err("YUYV data too short".into());
        }

        let mut rgb_data = Vec::with_capacity(width * height * 3);

        for chunk in yuyv_data.as_chunks::<4>().0 {
            let y1 = chunk[0] as f32;
            let u = chunk[1] as f32 - 128.0;
            let y2 = chunk[2] as f32;
            let v = chunk[3] as f32 - 128.0;

            // YUV to RGB conversion
            for y in [y1, y2] {
                let r = (y + 1.402 * v).clamp(0.0, 255.0) as u8;
                let g = (y - 0.344136 * u - 0.714136 * v).clamp(0.0, 255.0) as u8;
                let b = (y + 1.772 * u).clamp(0.0, 255.0) as u8;

                rgb_data.push(r);
                rgb_data.push(g);
                rgb_data.push(b);
            }
        }

        Ok(rgb_data)
    }

    /// 保存RGB数据为PNG文件
    async fn save_rgb_as_png(
        &self,
        rgb_data: &[u8],
        output_path: &str,
        width: u32,
        height: u32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use image::{ImageBuffer, Rgb};

        if rgb_data.len() < (width * height * 3) as usize {
            return Err("RGB data too short".into());
        }

        let img = ImageBuffer::<Rgb<u8>, _>::from_raw(width, height, rgb_data)
            .ok_or("Failed to create image buffer")?;

        img.save(output_path)?;
        Ok(())
    }

    /// JPEG转PNG
    async fn convert_jpeg_to_png(
        &self,
        jpeg_path: &str,
        png_path: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let jpeg_path = jpeg_path.to_string();
        let png_path = png_path.to_string();

        match tokio::task::spawn_blocking(
            move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
                use ffmpeg_next::{
                    format::{input, output},
                    util::frame::video::Video,
                };

                ffmpeg_next::init()?;

                // 使用 ffmpeg-next API 转换 JPEG 到 PNG
                let mut input_ctx = input(&jpeg_path)?;
                let input_stream_index = {
                    let input_stream = input_ctx
                        .streams()
                        .best(ffmpeg_next::media::Type::Video)
                        .ok_or("No video stream found")?;
                    input_stream.index()
                };
                let mut decoder = {
                    let input_stream = input_ctx.stream(input_stream_index).unwrap();
                    codec(&input_stream)?.decoder().video()?
                };

                // 创建输出上下文
                let mut output_ctx = output(&png_path)?;
                let mut output_stream = output_ctx
                    .add_stream(ffmpeg_next::encoder::find(ffmpeg_next::codec::Id::PNG))?;
                let mut encoder = codec(&output_stream)?.encoder().video()?;

                // 设置编码器参数
                encoder.set_width(decoder.width());
                encoder.set_height(decoder.height());
                encoder.set_format(decoder.format());
                encoder.set_time_base(decoder.time_base());

                let mut encoder =
                    encoder.open_as(ffmpeg_next::encoder::find(ffmpeg_next::codec::Id::PNG))?;
                output_stream.set_parameters(&encoder);

                output_ctx.write_header()?;

                // 解码和编码
                for (stream, packet) in input_ctx.packets() {
                    if stream.index() == input_stream_index {
                        decoder.send_packet(&packet)?;
                        let mut decoded = Video::empty();
                        while decoder.receive_frame(&mut decoded).is_ok() {
                            let mut encoded = ffmpeg_next::Packet::empty();
                            encoder.send_frame(&decoded)?;
                            while encoder.receive_packet(&mut encoded).is_ok() {
                                encoded.set_stream(0);
                                encoded.write_interleaved(&mut output_ctx)?;
                            }
                        }
                    }
                }

                // 刷新编码器
                encoder.send_eof()?;
                let mut encoded = ffmpeg_next::Packet::empty();
                while encoder.receive_packet(&mut encoded).is_ok() {
                    encoded.set_stream(0);
                    encoded.write_interleaved(&mut output_ctx)?;
                }

                output_ctx.write_trailer()?;
                Ok(())
            },
        )
        .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(format!("ffmpeg-next failed: {:?}", e).into()),
            Err(e) => Err(format!("Task failed: {:?}", e).into()),
        }
    }

    /// 从图片序列创建视频
    pub async fn create_video_from_images(
        &self,
        fps: f32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let images_dir = self.output_dir.join("images");
        let output_dir = self.output_dir.clone();

        match tokio::task::spawn_blocking(
            move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
                use ffmpeg_next::{
                    Rational, codec, encoder,
                    format::{Pixel, input, output},
                    media,
                };

                ffmpeg_next::init()?;

                // 使用 ffmpeg-next API 从图片序列创建视频
                let pattern = images_dir.join("frame_%06d.png");

                // 创建输入上下文（用于图片序列）
                let mut input_ctx = input(pattern.to_str().unwrap())?;
                let input_stream_index = {
                    let input_stream = input_ctx
                        .streams()
                        .best(media::Type::Video)
                        .ok_or("No video stream found")?;
                    input_stream.index()
                };

                let mut decoder = {
                    let input_stream = input_ctx.stream(input_stream_index).unwrap();
                    codec(&input_stream)?.decoder().video()?
                };

                // 创建输出上下文
                let output_path = output_dir.join("output_from_images.mp4");
                let mut output_ctx = output(output_path.to_str().unwrap())?;
                let mut output_stream = output_ctx.add_stream(encoder::find(codec::Id::H264))?;
                let mut encoder = codec(&output_stream)?.encoder().video()?;

                // 设置编码器参数
                encoder.set_width(decoder.width());
                encoder.set_height(decoder.height());
                encoder.set_format(Pixel::YUV420P);
                encoder.set_time_base(Rational(1, (fps as i32).max(1)));
                encoder.set_frame_rate(Some(Rational((fps as i32).max(1), 1)));

                let mut encoder = encoder.open_as(encoder::find(codec::Id::H264))?;
                output_stream.set_parameters(&encoder);

                output_ctx.write_header()?;

                // 处理帧
                let mut frame_count = 0i64;
                for (stream, packet) in input_ctx.packets() {
                    if stream.index() == input_stream_index {
                        decoder.send_packet(&packet)?;
                        let mut decoded = ffmpeg_next::util::frame::video::Video::empty();
                        while decoder.receive_frame(&mut decoded).is_ok() {
                            decoded.set_pts(Some(frame_count));
                            frame_count += 1;

                            let mut encoded = ffmpeg_next::Packet::empty();
                            encoder.send_frame(&decoded)?;
                            while encoder.receive_packet(&mut encoded).is_ok() {
                                encoded.set_stream(0);
                                encoded.write_interleaved(&mut output_ctx)?;
                            }
                        }
                    }
                }

                // 刷新编码器
                encoder.send_eof()?;
                let mut encoded = ffmpeg_next::Packet::empty();
                while encoder.receive_packet(&mut encoded).is_ok() {
                    encoded.set_stream(0);
                    encoded.write_interleaved(&mut output_ctx)?;
                }

                output_ctx.write_trailer()?;
                Ok(())
            },
        )
        .await
        {
            Ok(Ok(())) => {
                info!("Video from images created successfully using ffmpeg-next!");
                Ok(())
            }
            Ok(Err(e)) => Err(format!("ffmpeg-next failed: {:?}", e).into()),
            Err(e) => Err(format!("Task failed: {:?}", e).into()),
        }
    }

    /// 将帧转换为图片
    pub async fn convert_frames_to_images(
        &self,
        frame_numbers: &[u32],
        video_format: &VideoFormat,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let frame_numbers = frame_numbers.to_vec();
        let video_format = video_format.clone();

        match &video_format {
            VideoFormat {
                format_type: VideoFormatType::Mjpeg,
                ..
            } => {
                info!("Converting MJPEG frames to JPEG images...");
                self.convert_mjpeg_frames_to_images(frame_numbers).await
            }
            VideoFormat {
                format_type: VideoFormatType::Uncompressed(format_type),
                ..
            } => {
                info!(
                    "Converting uncompressed frames ({:?}) to PNG images...",
                    format_type
                );
                self.convert_raw_frames_to_images(frame_numbers, format_type, &video_format)
                    .await
            }
            VideoFormat {
                format_type: VideoFormatType::H264,
                ..
            } => {
                warn!("H264 format is not supported for frame-to-image conversion");
                Ok(())
            }
        }
    }

    /// 转换MJPEG帧为图片
    async fn convert_mjpeg_frames_to_images(
        &self,
        frame_numbers: Vec<u32>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let input_dir = self.input_dir.clone();
        let output_dir = self.output_dir.clone();

        match tokio::task::spawn_blocking(
            move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
                use std::fs;

                let images_dir = output_dir.join("images");
                fs::create_dir_all(&images_dir)?;

                for frame_number in frame_numbers {
                    let input_path = input_dir.join(format!("frame_{:06}.raw", frame_number));
                    let output_path = images_dir.join(format!("frame_{:06}.jpg", frame_number));

                    if let Ok(data) = fs::read(&input_path) {
                        // 检查这是否是JPEG数据（以FF D8开头）
                        if data.len() >= 2 && data[0] == 0xFF && data[1] == 0xD8 {
                            // 直接保存为JPEG文件
                            fs::write(&output_path, &data)?;
                            println!("Converted frame {} to {:?}", frame_number, output_path);
                        } else {
                            println!("Skipping frame {} - not valid JPEG data", frame_number);
                        }
                    }
                }
                Ok(())
            },
        )
        .await
        {
            Ok(Ok(())) => {
                info!("MJPEG frames converted to JPEG images successfully!");
                Ok(())
            }
            Ok(Err(e)) => Err(format!("Conversion failed: {:?}", e).into()),
            Err(e) => Err(format!("Task failed: {:?}", e).into()),
        }
    }

    /// 转换原始帧为图片
    async fn convert_raw_frames_to_images(
        &self,
        frame_numbers: Vec<u32>,
        _format_type: &UncompressedFormat,
        video_format: &VideoFormat,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let video_format = video_format.clone();
        let input_dir = self.input_dir.clone();
        let output_dir = self.output_dir.clone();

        match tokio::task::spawn_blocking(
            move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
                use std::{fs::File, io::Read};

                let images_dir = output_dir.join("images");
                std::fs::create_dir_all(&images_dir)?;

                for frame_number in frame_numbers {
                    let input_path = input_dir.join(format!("frame_{:06}.raw", frame_number));
                    let output_path = images_dir.join(format!("frame_{:06}.png", frame_number));

                    if let Ok(mut file) = File::open(&input_path) {
                        let mut buffer = Vec::new();
                        if file.read_to_end(&mut buffer).is_ok() && !buffer.is_empty() {
                            // 这里需要根据实际的像素格式和尺寸来处理原始数据
                            // 对于 YUY2 格式，我们需要转换为RGB并保存为PNG

                            match &video_format {
                                VideoFormat {
                                    width,
                                    height,
                                    format_type: VideoFormatType::Uncompressed(format_type),
                                    ..
                                } => {
                                    match format_type {
                                        UncompressedFormat::Yuy2 => {
                                            // 使用图像处理库将YUY2转换为PNG
                                            if let Err(e) = convert_yuy2_to_png(
                                                &buffer,
                                                *width,
                                                *height,
                                                output_path.to_str().unwrap(),
                                            ) {
                                                println!(
                                                    "Failed to convert frame {}: {:?}",
                                                    frame_number, e
                                                );
                                            } else {
                                                println!(
                                                    "Converted frame {} to {:?}",
                                                    frame_number, output_path
                                                );
                                            }
                                        }
                                        _ => {
                                            println!("Unsupported format type: {:?}", format_type);
                                        }
                                    }
                                }
                                _ => {
                                    println!("Unexpected video format for raw conversion");
                                }
                            }
                        }
                    }
                }
                Ok(())
            },
        )
        .await
        {
            Ok(Ok(())) => {
                info!("Raw frames converted to PNG images successfully!");
                Ok(())
            }
            Ok(Err(e)) => Err(format!("Conversion failed: {:?}", e).into()),
            Err(e) => Err(format!("Task failed: {:?}", e).into()),
        }
    }
}

/// YUY2转PNG的辅助函数
fn convert_yuy2_to_png(
    yuy2_data: &[u8],
    width: u16,
    height: u16,
    output_path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use image::{ImageBuffer, Rgb};

    let width = width as u32;
    let height = height as u32;

    // YUY2 格式：每4个字节表示2个像素 (Y0 U Y1 V)
    let expected_size = (width * height * 2) as usize;
    if yuy2_data.len() < expected_size {
        return Err(format!(
            "Invalid YUY2 data size: expected {}, got {}",
            expected_size,
            yuy2_data.len()
        )
        .into());
    }

    let mut rgb_buffer = ImageBuffer::new(width, height);

    for y in 0..height {
        for x in 0..(width / 2) {
            let base_idx = ((y * width / 2 + x) * 4) as usize;
            if base_idx + 3 < yuy2_data.len() {
                let y0 = yuy2_data[base_idx] as f32;
                let u = yuy2_data[base_idx + 1] as f32;
                let y1 = yuy2_data[base_idx + 2] as f32;
                let v = yuy2_data[base_idx + 3] as f32;

                // YUV到RGB的转换
                let convert_yuv_to_rgb = |y: f32, u: f32, v: f32| -> (u8, u8, u8) {
                    let c = y - 16.0;
                    let d = u - 128.0;
                    let e = v - 128.0;

                    let r = ((298.0 * c + 409.0 * e + 128.0) / 256.0).clamp(0.0, 255.0) as u8;
                    let g = ((298.0 * c - 100.0 * d - 208.0 * e + 128.0) / 256.0).clamp(0.0, 255.0)
                        as u8;
                    let b = ((298.0 * c + 516.0 * d + 128.0) / 256.0).clamp(0.0, 255.0) as u8;

                    (r, g, b)
                };

                // 转换第一个像素
                let (r0, g0, b0) = convert_yuv_to_rgb(y0, u, v);
                if x * 2 < width {
                    rgb_buffer.put_pixel(x * 2, y, Rgb([r0, g0, b0]));
                }

                // 转换第二个像素
                let (r1, g1, b1) = convert_yuv_to_rgb(y1, u, v);
                if x * 2 + 1 < width {
                    rgb_buffer.put_pixel(x * 2 + 1, y, Rgb([r1, g1, b1]));
                }
            }
        }
    }

    rgb_buffer.save(output_path)?;
    Ok(())
}

fn codec(stream: &Stream) -> Result<Context, ffmpeg_next::Error> {
    Context::from_parameters(stream.parameters())
}

#[cfg(test)]
mod tests;
