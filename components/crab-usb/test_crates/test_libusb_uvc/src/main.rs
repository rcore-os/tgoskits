#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]
#![cfg(not(target_os = "none"))]

use crab_usb::USBHost;
use crab_uvc::{UvcDevice, VideoControlEvent};
use log::{debug, error, info, warn};
use std::{sync::Arc, time::Duration};
use uvc_frame_parser::Parser;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .init();

    info!("Starting UVC video capture example");

    // 创建 USB 主机
    let mut host = USBHost::new_libusb().unwrap();
    host.init().await.unwrap();
    info!("usb host init ok");

    // 扫描连接的设备
    let devices = host.probe_devices().await?;

    // 查找 UVC 设备
    let mut uvc_device = None;
    for probed in devices {
        let Some(device_info) = probed.into_device_info() else {
            continue;
        };
        info!(
            "Checking device: VID={:04x}, PID={:04x}",
            device_info.vendor_id(),
            device_info.product_id()
        );

        if UvcDevice::check(&device_info) {
            info!("Found UVC device!");
            let device = host.open_device(&device_info).await?;
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
    let Some(format) = formats.first() else {
        error!("No supported formats available");
        return Ok(());
    };

    info!("Setting format: {:?}", format);
    uvc.set_format(format.clone()).await?;

    // 开始视频流
    info!("Starting video streaming...");
    let mut stream = uvc.start_streaming().await?;

    // 获取当前视频格式信息
    let current_format = stream.vedio_format.clone();
    info!("Current video format: {:?}", current_format);

    // 创建帧解析器
    let parser = Arc::new(Parser::new("target/frames".into(), "target/output".into()).await);

    // 将格式信息写入文件，供脚本使用
    if let Err(e) = parser.write_format_info(&current_format).await {
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

    let saved_frames = Arc::new(std::sync::Mutex::new(Vec::new()));
    let saved_frames_clone = saved_frames.clone();
    let parser_clone = parser.clone();

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
                        if let Err(e) = parser_clone
                            .save_frame_to_file(&frame, frame_number as u32)
                            .await
                        {
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
        if let Err(e) = parser
            .create_video_from_frames(&saved_frame_numbers, avg_fps, &current_format)
            .await
        {
            error!("Failed to create video: {:?}", e);
        } else {
            info!("Video saved as output.mp4");
        }

        // 转换每帧为图片
        info!("Converting frames to images...");
        if let Err(e) = parser
            .convert_frames_to_images(&saved_frame_numbers, &current_format)
            .await
        {
            error!("Failed to convert frames to images: {:?}", e);
        } else {
            info!("Images saved to images/ directory");
        }
    }

    Ok(())
}
