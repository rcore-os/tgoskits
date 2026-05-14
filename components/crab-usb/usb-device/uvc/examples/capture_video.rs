use crab_usb::USBHost;
use crab_uvc::{UncompressedFormat, UvcDevice, VideoControlEvent, VideoFormat};
use env_logger;
use ffmpeg_next as ffmpeg;
use log::{debug, error, info, warn};
use std::{hint::spin_loop, sync::Arc, thread, time::Duration};
use tokio::fs;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::builder()
        .filter_level(log::LevelFilter::Debug)
        .init();

    info!("Starting UVC video capture example");

    // 创建 USB 主机
    let mut host = USBHost::new_libusb();
    let event_handler = host.event_handler();
    thread::spawn(move || {
        while event_handler.handle_event() {
            spin_loop();
        }
    });

    // 扫描连接的设备
    let devices = host.device_list().await?;

    // 查找 UVC 设备
    let mut uvc_device = None;
    for mut device_info in devices {
        info!(
            "Checking device: VID={:04x}, PID={:04x}",
            device_info.vendor_id(),
            device_info.product_id()
        );

        if UvcDevice::check(&device_info) {
            info!("Found UVC device!");
            let device = device_info.open().await?;
            uvc_device = Some(UvcDevice::new(device).await?);
            break;
        }
    }

    let mut uvc = match uvc_device {
        Some(device) => device,
        None => {
            warn!("No UVC device found. Make sure a USB camera is connected.");
            return Ok(());
        }
    };

    // 获取设备信息
    let device_info = uvc.get_device_info().await?;
    info!("Device info: {}", device_info);

    // 获取支持的视频格式
    let formats = uvc.get_supported_formats().await?;
    info!("Supported formats:");
    for format in &formats {
        info!("  {:?}", format);
    }

    // 设置视频格式 (选择第一个可用格式)
    if let Some(format) = formats.first() {
        info!("Setting format: {:?}", format);
        uvc.set_format(format.clone()).await?;
    } else {
        error!("No supported formats available");
        return Ok(());
    }

    // 开始视频流
    info!("Starting video streaming...");
    let mut stream = uvc.start_streaming().await?;

    // 获取当前视频格式信息
    let current_format = stream.vedio_format.clone();
    info!("Current video format: {:?}", current_format);

    // 将格式信息写入文件，供脚本使用
    if let Err(e) = write_format_info(&current_format).await {
        warn!("Failed to write format info: {:?}", e);
    }

    // 设置一些控制参数的示例
    info!("Setting video controls...");

    // 尝试设置亮度（如果失败也继续）
    if let Err(e) = uvc
        .send_control_command(VideoControlEvent::BrightnessChanged(100))
        .await
    {
        warn!("Failed to set brightness: {:?}", e);
    }

    // 尝试设置对比度（如果失败也继续）
    // if let Err(e) = uvc
    //     .send_control_command(VideoControlEvent::ContrastChanged(50))
    //     .await
    // {
    //     warn!("Failed to set contrast: {:?}", e);
    // }

    let start_time = std::time::Instant::now();

    // 捕获视频帧 (运行30秒)
    info!("Capturing video frames for 30 seconds...");
    let capture_duration = Duration::from_secs(6);
    let frame_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let frame_count_clone = frame_count.clone();

    let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let running_clone = running.clone();

    let mut last_err = String::new();

    // 创建输出目录
    let output_dir = "frames";
    if let Err(e) = fs::create_dir_all(output_dir).await {
        warn!("Failed to create output directory: {:?}", e);
    }

    let saved_frames = Arc::new(std::sync::Mutex::new(Vec::new()));
    let saved_frames_clone = saved_frames.clone();

    let handle = tokio::spawn(async move {
        // 处理设备事件
        while running_clone.load(std::sync::atomic::Ordering::Relaxed) {
            let data = stream.recv().await;
            match data {
                Ok(frames) => {
                    for frame in frames {
                        let frame_number =
                            frame_count_clone.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        debug!("Received frame data of {} bytes", frame.data.len());

                        // 保存帧数据
                        if let Err(e) = save_frame_to_file(&frame, frame_number as u32).await {
                            warn!("Failed to save frame {}: {:?}", frame_number, e);
                        } else {
                            saved_frames_clone.lock().unwrap().push(frame_number as u32);
                        }
                    }
                }
                Err(e) => {
                    if e.to_string() != last_err {
                        warn!("Error receiving frame: {:?}", e);
                        last_err = e.to_string();
                    }
                }
            }
            ()
        }
    });

    tokio::time::sleep(capture_duration).await;

    running.store(false, std::sync::atomic::Ordering::Relaxed);
    handle.await.unwrap();

    let frame_count = frame_count.load(std::sync::atomic::Ordering::Acquire);
    let saved_frame_numbers = saved_frames.lock().unwrap().clone();

    let avg_fps = frame_count as f32 / start_time.elapsed().as_secs_f32();
    info!(
        "Capture completed. Total frames: {}, Average FPS: {:.2}",
        frame_count, avg_fps
    );

    // 生成视频文件
    if !saved_frame_numbers.is_empty() {
        info!("Converting frames to video...");
        if let Err(e) =
            create_video_from_frames(&saved_frame_numbers, avg_fps, &current_format).await
        {
            error!("Failed to create video: {:?}", e);
        } else {
            info!("Video saved as output.mp4");
        }

        // 转换每帧为图片
        info!("Converting frames to images...");
        if let Err(e) = convert_frames_to_images(&saved_frame_numbers, &current_format).await {
            error!("Failed to convert frames to images: {:?}", e);
        } else {
            info!("Images saved to images/ directory");
        }
    }

    Ok(())
}

async fn save_frame_to_file(
    frame: &crab_uvc::frame::FrameEvent,
    frame_number: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    use tokio::fs::File;
    use tokio::io::AsyncWriteExt;

    // 保存为原始数据文件，便于后续处理
    let filename = format!("frames/frame_{:06}.raw", frame_number);
    let mut file = File::create(&filename).await?;
    file.write_all(&frame.data).await?;
    debug!("Saved frame {} to {}", frame_number, filename);
    Ok(())
}

async fn create_video_from_frames(
    frame_numbers: &[u32],
    fps: f32,
    video_format: &crab_uvc::VideoFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    info!(
        "Creating video with {} frames at {:.2} fps",
        frame_numbers.len(),
        fps
    );
    info!("Video format: {:?}", video_format);

    // 根据 VideoFormat 确定 FFmpeg 参数
    let (width, height, pixel_format) = match video_format {
        crab_uvc::VideoFormat::Uncompressed {
            width,
            height,
            format_type,
            ..
        } => {
            let ffmpeg_format = match format_type {
                crab_uvc::UncompressedFormat::Yuy2 => "yuyv422",
                crab_uvc::UncompressedFormat::Nv12 => "nv12",
                crab_uvc::UncompressedFormat::Rgb24 => "rgb24",
                crab_uvc::UncompressedFormat::Rgb32 => "rgba",
            };
            (*width, *height, ffmpeg_format)
        }
        crab_uvc::VideoFormat::Mjpeg { width, height, .. } => {
            // MJPEG 数据直接从帧解码，不需要指定像素格式
            return create_video_from_mjpeg_frames(frame_numbers, fps, *width, *height).await;
        }
        crab_uvc::VideoFormat::H264 { width, height, .. } => {
            // H.264 数据直接从帧解码
            return create_video_from_h264_frames(frame_numbers, fps, *width, *height).await;
        }
    };

    info!(
        "Using FFmpeg parameters: {}x{}, format: {}",
        width, height, pixel_format
    );

    // 使用 ffmpeg-next 从原始帧创建视频
    match tokio::task::spawn_blocking(
        move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            use ffmpeg::format::{Pixel, output};
            use ffmpeg::{Rational, codec, encoder};

            ffmpeg::init()?;

            // 使用 ffmpeg-next API 处理原始视频数据
            let pattern = "frames/frame_%06d.raw";

            // 创建输出上下文
            let mut output_ctx = output("output.mp4")?;
            let mut output_stream = output_ctx.add_stream(encoder::find(codec::Id::H264))?;
            let mut encoder = output_stream.codec().encoder().video()?;

            // 设置编码器参数
            encoder.set_width(width as u32);
            encoder.set_height(height as u32);
            encoder.set_format(Pixel::YUV420P);
            encoder.set_time_base(Rational(1, (fps as i32).max(1)));
            encoder.set_frame_rate(Some(Rational((fps as i32).max(1), 1)));

            let mut encoder = encoder.open_as(encoder::find(codec::Id::H264))?;
            output_stream.set_parameters(&encoder);

            output_ctx.write_header()?;

            // 由于原始视频格式需要特殊处理，我们需要手动读取和解码数据
            // 这里简化处理，实际上需要根据 pixel_format 来正确解码原始数据

            // 读取原始帧数据并编码
            for i in 0..100u32 {
                // 假设最多100帧
                let frame_path = format!("frames/frame_{:06}.raw", i);
                if std::path::Path::new(&frame_path).exists() {
                    // 这里需要根据实际的像素格式来处理原始数据
                    // 由于复杂性，可能需要外部工具或更复杂的处理
                    info!("Processing frame: {}", frame_path);
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
            convert_raw_to_images(frame_numbers, video_format).await?;
            create_video_from_images(fps).await?;
        }
        Err(e) => {
            warn!("Task failed: {:?}", e);

            // 如果直接转换失败，尝试另一种方法：先转换为图片再合成视频
            info!("Trying alternative approach: convert to images first");
            convert_raw_to_images(frame_numbers, video_format).await?;
            create_video_from_images(fps).await?;
        }
    }

    Ok(())
}

async fn create_video_from_mjpeg_frames(
    _frame_numbers: &[u32],
    fps: f32,
    width: u16,
    height: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("Creating video from MJPEG frames: {}x{}", width, height);

    // 使用 ffmpeg-next 处理 MJPEG 帧
    match tokio::task::spawn_blocking(
        move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            use ffmpeg::format::{Pixel, output};
            use ffmpeg::{Rational, codec, encoder};
            use std::fs::File;
            use std::io::Read;

            ffmpeg::init()?;

            // 创建输出上下文
            let mut output_ctx = output("output_mjpeg.mp4")?;
            let mut output_stream = output_ctx.add_stream(encoder::find(codec::Id::H264))?;
            let mut encoder = output_stream.codec().encoder().video()?;

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
                let frame_path = format!("frames/frame_{:06}.raw", i);
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
                            use ffmpeg::format::input;
                            let mut input_ctx = input(&temp_jpeg_path)?;
                            let input_stream_index = {
                                let input_stream = input_ctx
                                    .streams()
                                    .best(ffmpeg::media::Type::Video)
                                    .ok_or("No video stream found")?;
                                input_stream.index()
                            };

                            let mut decoder = {
                                let input_stream = input_ctx.stream(input_stream_index).unwrap();
                                input_stream.codec().decoder().video()?
                            };

                            for (stream, packet) in input_ctx.packets() {
                                if stream.index() == input_stream_index {
                                    decoder.send_packet(&packet)?;
                                    let mut decoded = ffmpeg::util::frame::video::Video::empty();
                                    while decoder.receive_frame(&mut decoded).is_ok() {
                                        decoded.set_pts(Some(frame_count));
                                        frame_count += 1;

                                        let mut encoded = ffmpeg::Packet::empty();
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
            let mut encoded = ffmpeg::Packet::empty();
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

async fn create_video_from_h264_frames(
    _frame_numbers: &[u32],
    fps: f32,
    width: u16,
    height: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("Creating video from H.264 frames: {}x{}", width, height);

    // 使用 ffmpeg-next 处理 H.264 帧
    match tokio::task::spawn_blocking(
        move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            use ffmpeg::format::output;
            use ffmpeg::{Packet, Rational, codec, encoder};
            use std::fs::File;
            use std::io::Read;

            ffmpeg::init()?;

            // 对于 H.264 原始帧，我们需要读取每个帧文件并创建 MP4 容器
            let mut octx = output(&"output_h264.mp4")?;

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
                let frame_path = format!("frames/frame_{:06}.raw", i);
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

async fn convert_raw_to_images(
    frame_numbers: &[u32],
    video_format: &crab_uvc::VideoFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    use tokio::fs;

    // 创建图片输出目录
    fs::create_dir_all("images").await?;

    let (width, height, format_info) = match video_format {
        crab_uvc::VideoFormat::Uncompressed {
            width,
            height,
            format_type,
            ..
        } => (*width, *height, format!("{:?}", format_type)),
        crab_uvc::VideoFormat::Mjpeg { width, height, .. } => {
            (*width, *height, "MJPEG".to_string())
        }
        crab_uvc::VideoFormat::H264 { width, height, .. } => (*width, *height, "H264".to_string()),
    };

    info!(
        "Converting frames to images: {}x{}, format: {}",
        width, height, format_info
    );

    for &frame_num in frame_numbers {
        let raw_file = format!("frames/frame_{:06}.raw", frame_num);
        let png_file = format!("images/frame_{:06}.png", frame_num);

        // 读取原始数据
        if let Ok(raw_data) = fs::read(&raw_file).await {
            // 这里需要根据实际的图像格式进行转换
            info!(
                "Converting frame {} to PNG (size: {} bytes)",
                frame_num,
                raw_data.len()
            );

            if let Err(e) =
                convert_raw_to_png(&raw_data, &png_file, width, height, video_format).await
            {
                warn!("Failed to convert frame {}: {:?}", frame_num, e);
            }
        }
    }

    Ok(())
}

async fn convert_raw_to_png(
    raw_data: &[u8],
    output_path: &str,
    width: u16,
    height: u16,
    video_format: &crab_uvc::VideoFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    use tokio::fs::File;
    use tokio::io::AsyncWriteExt;

    match video_format {
        crab_uvc::VideoFormat::Uncompressed { format_type, .. } => {
            // 对于未压缩格式，我们可以尝试使用 image crate 进行转换
            match format_type {
                crab_uvc::UncompressedFormat::Yuy2 => {
                    // YUY2 (YUYV) 到 RGB 的转换
                    if let Ok(rgb_data) =
                        convert_yuyv_to_rgb(raw_data, width as usize, height as usize)
                    {
                        save_rgb_as_png(&rgb_data, output_path, width as u32, height as u32)
                            .await?;
                    } else {
                        // 如果转换失败，保存原始数据
                        let mut file = File::create(output_path).await?;
                        file.write_all(raw_data).await?;
                    }
                }
                crab_uvc::UncompressedFormat::Rgb24 => {
                    // RGB24 直接转 PNG
                    save_rgb_as_png(raw_data, output_path, width as u32, height as u32).await?;
                }
                _ => {
                    // 其他格式暂时保存为原始数据
                    let mut file = File::create(output_path).await?;
                    file.write_all(raw_data).await?;
                }
            }
        }
        crab_uvc::VideoFormat::Mjpeg { .. } => {
            // MJPEG 数据可以直接保存为 JPEG 文件
            let jpeg_path = output_path.replace(".png", ".jpg");
            let mut file = File::create(&jpeg_path).await?;
            file.write_all(raw_data).await?;

            // 尝试用 FFmpeg 转换为 PNG
            if let Err(_) = convert_jpeg_to_png(&jpeg_path, output_path).await {
                // 如果转换失败，至少我们有 JPEG 文件
                debug!("Kept JPEG file: {}", jpeg_path);
            }
        }
        crab_uvc::VideoFormat::H264 { .. } => {
            // H.264 帧需要特殊处理，暂时保存原始数据
            let mut file = File::create(output_path).await?;
            file.write_all(raw_data).await?;
        }
    }

    Ok(())
}

fn convert_yuyv_to_rgb(
    yuyv_data: &[u8],
    width: usize,
    height: usize,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    if yuyv_data.len() < width * height * 2 {
        return Err("YUYV data too short".into());
    }

    let mut rgb_data = Vec::with_capacity(width * height * 3);

    for chunk in yuyv_data.chunks_exact(4) {
        if chunk.len() < 4 {
            break;
        }

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

async fn save_rgb_as_png(
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

async fn convert_jpeg_to_png(
    jpeg_path: &str,
    png_path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let jpeg_path = jpeg_path.to_string();
    let png_path = png_path.to_string();

    match tokio::task::spawn_blocking(
        move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            use ffmpeg::format::{input, output};

            use ffmpeg::util::frame::video::Video;

            ffmpeg::init()?;

            // 使用 ffmpeg-next API 转换 JPEG 到 PNG
            let mut input_ctx = input(&jpeg_path)?;
            let input_stream_index = {
                let input_stream = input_ctx
                    .streams()
                    .best(ffmpeg::media::Type::Video)
                    .ok_or("No video stream found")?;
                input_stream.index()
            };
            let mut decoder = {
                let input_stream = input_ctx.stream(input_stream_index).unwrap();
                input_stream.codec().decoder().video()?
            };

            // 创建输出上下文
            let mut output_ctx = output(&png_path)?;
            let mut output_stream =
                output_ctx.add_stream(ffmpeg::encoder::find(ffmpeg::codec::Id::PNG))?;
            let mut encoder = output_stream.codec().encoder().video()?;

            // 设置编码器参数
            encoder.set_width(decoder.width());
            encoder.set_height(decoder.height());
            encoder.set_format(decoder.format());
            encoder.set_time_base(decoder.time_base());

            let mut encoder = encoder.open_as(ffmpeg::encoder::find(ffmpeg::codec::Id::PNG))?;
            output_stream.set_parameters(&encoder);

            output_ctx.write_header()?;

            // 解码和编码
            for (stream, packet) in input_ctx.packets() {
                if stream.index() == input_stream_index {
                    decoder.send_packet(&packet)?;
                    let mut decoded = Video::empty();
                    while decoder.receive_frame(&mut decoded).is_ok() {
                        let mut encoded = ffmpeg::Packet::empty();
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
            let mut encoded = ffmpeg::Packet::empty();
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

async fn write_format_info(
    video_format: &crab_uvc::VideoFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    use tokio::fs::File;
    use tokio::io::AsyncWriteExt;

    let (width, height, pixel_format) = match video_format {
        crab_uvc::VideoFormat::Uncompressed {
            width,
            height,
            format_type,
            ..
        } => {
            let ffmpeg_format = match format_type {
                crab_uvc::UncompressedFormat::Yuy2 => "yuyv422",
                crab_uvc::UncompressedFormat::Nv12 => "nv12",
                crab_uvc::UncompressedFormat::Rgb24 => "rgb24",
                crab_uvc::UncompressedFormat::Rgb32 => "rgba",
            };
            (*width, *height, ffmpeg_format)
        }
        crab_uvc::VideoFormat::Mjpeg { width, height, .. } => (*width, *height, "mjpeg"),
        crab_uvc::VideoFormat::H264 { width, height, .. } => (*width, *height, "h264"),
    };

    let format_info = format!(
        "# 视频格式信息 (由 capture_video 自动生成)\nWIDTH={}\nHEIGHT={}\nPIXEL_FORMAT=\"{}\"\n",
        width, height, pixel_format
    );

    let mut file = File::create("video_format_info.txt").await?;
    file.write_all(format_info.as_bytes()).await?;

    info!(
        "Format info written to video_format_info.txt: {}x{}, {}",
        width, height, pixel_format
    );

    Ok(())
}

async fn create_video_from_images(fps: f32) -> Result<(), Box<dyn std::error::Error>> {
    match tokio::task::spawn_blocking(
        move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            use ffmpeg::format::{Pixel, input, output};
            use ffmpeg::{Rational, codec, encoder, media};

            ffmpeg::init()?;

            // 使用 ffmpeg-next API 从图片序列创建视频
            let pattern = "images/frame_%06d.png";

            // 创建输入上下文（用于图片序列）
            let mut input_ctx = input(pattern)?;
            let input_stream_index = {
                let input_stream = input_ctx
                    .streams()
                    .best(media::Type::Video)
                    .ok_or("No video stream found")?;
                input_stream.index()
            };

            let mut decoder = {
                let input_stream = input_ctx.stream(input_stream_index).unwrap();
                input_stream.codec().decoder().video()?
            };

            // 创建输出上下文
            let mut output_ctx = output("output_from_images.mp4")?;
            let mut output_stream = output_ctx.add_stream(encoder::find(codec::Id::H264))?;
            let mut encoder = output_stream.codec().encoder().video()?;

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
                    let mut decoded = ffmpeg::util::frame::video::Video::empty();
                    while decoder.receive_frame(&mut decoded).is_ok() {
                        decoded.set_pts(Some(frame_count));
                        frame_count += 1;

                        let mut encoded = ffmpeg::Packet::empty();
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
            let mut encoded = ffmpeg::Packet::empty();
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

async fn convert_frames_to_images(
    frame_numbers: &[u32],
    video_format: &VideoFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::fs;
    use tokio::task;

    // 创建 images 目录
    fs::create_dir_all("images")?;

    let frame_numbers = frame_numbers.to_vec();
    let video_format = video_format.clone();

    match &video_format {
        VideoFormat::Mjpeg { .. } => {
            info!("Converting MJPEG frames to JPEG images...");
            convert_mjpeg_frames_to_images(frame_numbers).await
        }
        VideoFormat::Uncompressed { format_type, .. } => {
            info!(
                "Converting uncompressed frames ({:?}) to PNG images...",
                format_type
            );
            convert_raw_frames_to_images(frame_numbers, format_type, &video_format).await
        }
        VideoFormat::H264 { .. } => {
            warn!("H264 format is not supported for frame-to-image conversion");
            Ok(())
        }
    }
}

async fn convert_mjpeg_frames_to_images(
    frame_numbers: Vec<u32>,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::fs;

    match tokio::task::spawn_blocking(
        move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            for frame_number in frame_numbers {
                let input_path = format!("frames/frame_{:06}.raw", frame_number);
                let output_path = format!("images/frame_{:06}.jpg", frame_number);

                if let Ok(data) = fs::read(&input_path) {
                    // 检查这是否是JPEG数据（以FF D8开头）
                    if data.len() >= 2 && data[0] == 0xFF && data[1] == 0xD8 {
                        // 直接保存为JPEG文件
                        fs::write(&output_path, &data)?;
                        println!("Converted frame {} to {}", frame_number, output_path);
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

async fn convert_raw_frames_to_images(
    frame_numbers: Vec<u32>,
    format_type: &UncompressedFormat,
    video_format: &VideoFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    let format_type = format_type.clone();
    let video_format = video_format.clone();

    match tokio::task::spawn_blocking(
        move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            use std::fs::File;
            use std::io::Read;

            for frame_number in frame_numbers {
                let input_path = format!("frames/frame_{:06}.raw", frame_number);
                let output_path = format!("images/frame_{:06}.png", frame_number);

                if let Ok(mut file) = File::open(&input_path) {
                    let mut buffer = Vec::new();
                    if file.read_to_end(&mut buffer).is_ok() && !buffer.is_empty() {
                        // 这里需要根据实际的像素格式和尺寸来处理原始数据
                        // 对于 YUY2 格式，我们需要转换为RGB并保存为PNG

                        match &video_format {
                            VideoFormat::Uncompressed {
                                width,
                                height,
                                format_type,
                                ..
                            } => {
                                match format_type {
                                    UncompressedFormat::Yuy2 => {
                                        // 使用图像处理库将YUY2转换为PNG
                                        if let Err(e) = convert_yuy2_to_png(
                                            &buffer,
                                            *width,
                                            *height,
                                            &output_path,
                                        ) {
                                            println!(
                                                "Failed to convert frame {}: {:?}",
                                                frame_number, e
                                            );
                                        } else {
                                            println!(
                                                "Converted frame {} to {}",
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
