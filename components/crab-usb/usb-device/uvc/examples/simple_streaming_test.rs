/// 简化的 UVC 流传输测试
/// 用于诊断 isochronous 传输问题
use crab_usb::USBHost;
use crab_uvc::*;
use log::{debug, info, warn};
use std::{hint::spin_loop, thread};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::builder()
        .filter_level(log::LevelFilter::Debug)
        .init();

    info!("Starting simple UVC streaming test");

    // 创建 USB 主机
    let mut host = USBHost::new_libusb();
    let event_handler = host.event_handler();
    thread::spawn(move || {
        while event_handler.handle_event() {
            spin_loop();
        }
    });

    info!("Getting device list...");
    // 查找 UVC 设备
    let mut uvc_device = None;
    let devices: Vec<_> = host.device_list().await?.collect();
    info!("Found {} devices total", devices.len());

    for mut device_info in devices {
        let vid = device_info.vendor_id();
        let pid = device_info.product_id();
        debug!("Checking device: VID={:04x}, PID={:04x}", vid, pid);

        if UvcDevice::check(&device_info) {
            info!("Found UVC device: VID={:04x}, PID={:04x}", vid, pid);
            let device = device_info.open().await?;
            uvc_device = Some(UvcDevice::new(device).await?);
            break;
        }
    }

    let mut device = match uvc_device {
        Some(dev) => dev,
        None => {
            warn!("No UVC device found");
            return Ok(());
        }
    };

    info!("Getting supported formats...");
    let formats = device.get_supported_formats().await?;
    if formats.is_empty() {
        warn!("No supported formats found");
        return Ok(());
    }

    info!("Found {} formats", formats.len());

    // 选择最简单的格式
    let format = formats
        .into_iter()
        .find(|f| {
            matches!(
                f,
                VideoFormat::Mjpeg {
                    width: 320,
                    height: 240,
                    ..
                }
            )
        })
        .or_else(|| {
            Some(VideoFormat::Mjpeg {
                width: 640,
                height: 480,
                frame_rate: 30,
            })
        })
        .unwrap();

    info!("Testing with format: {:?}", format);

    // 设置格式
    device.set_format(format).await?;
    info!("Format set successfully");

    // 开始流传输
    let mut stream = device.start_streaming().await?;
    info!("Streaming started");

    let frame = stream.recv().await?;

    // // 尝试几次简单的传输
    // for attempt in 0..10 {
    //     match device.recv_frame().await {
    //         Ok(Some(frame)) => {
    //             info!(
    //                 "SUCCESS! Received frame {}: {} bytes",
    //                 frame.frame_number,
    //                 frame.data.len()
    //             );
    //             // 成功接收到一帧就退出
    //             break;
    //         }
    //         Ok(None) => {
    //             debug!("Attempt {}: No frame received", attempt);
    //         }
    //         Err(e) => {
    //             warn!("Attempt {}: Error: {:?}", attempt, e);

    //             // 如果是队列满，等待一下
    //             if format!("{:?}", e).contains("QueueFull") {
    //                 tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    //             }
    //         }
    //     }
    // }

    info!("Test completed");
    Ok(())
}
