# USB UVC (Video Class) Device Library

基于 `crab-usb` 的 USB Video Class (UVC) 设备驱动库，支持 USB 摄像头和视频设备的控制与数据流传输。

## 功能特性

### 核心功能

- **UVC 设备检测与枚举**: 自动识别和初始化 USB 视频设备
- **多种视频格式支持**: 支持 MJPEG、H.264 和未压缩格式 (YUY2, NV12, RGB24, RGB32)
- **视频流控制**: 启动、停止和管理视频数据流传输
- **设备状态管理**: 跟踪设备配置和流传输状态
- **异步数据传输**: 基于 async/await 的非阻塞视频帧接收

### 视频控制

- **图像参数调节**: 亮度、对比度、色调、饱和度控制
- **格式协商**: 动态设置和切换视频分辨率、帧率
- **错误处理**: 完善的错误检测和恢复机制

## 使用方法

### 基本使用

```rust
use crab_usb::host::Host;
use crab_uvc::{UvcDevice, VideoFormat};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 创建 USB 主机
    let mut host = Host::new_libusb().await?;
    
    // 扫描并查找 UVC 设备
    let devices = host.probe().await?;
    for device in devices {
        let info = device.info();
        if UvcDevice::check(&info) {
            let mut uvc = UvcDevice::new(device).await?;
            
            // 获取支持的格式
            let formats = uvc.get_supported_formats().await?;
            
            // 设置视频格式
            if let Some(format) = formats.first() {
                uvc.set_format(format.clone()).await?;
            }
            
            // 开始流传输
            uvc.start_streaming().await?;
            
            // 接收视频帧
            while let Ok(Some(frame)) = uvc.recv_frame().await {
                println!("Received frame: {} bytes", frame.data.len());
                // 处理视频帧数据...
            }
            
            // 停止流传输
            uvc.stop_streaming().await?;
            break;
        }
    }
    
    Ok(())
}
```

### 设置视频控制参数

```rust
use crab_uvc::VideoControlEvent;

// 调整图像参数
uvc.send_control_command(VideoControlEvent::BrightnessChanged(100)).await?;
uvc.send_control_command(VideoControlEvent::ContrastChanged(50)).await?;
uvc.send_control_command(VideoControlEvent::HueChanged(0)).await?;
uvc.send_control_command(VideoControlEvent::SaturationChanged(80)).await?;
```

### 支持的视频格式

```rust
use crab_uvc::{VideoFormat, UncompressedFormat};

// MJPEG 压缩格式
let mjpeg_format = VideoFormat::Mjpeg {
    width: 1920,
    height: 1080,
    frame_rate: 30,
};

// 未压缩格式
let yuy2_format = VideoFormat::Uncompressed {
    width: 640,
    height: 480,
    frame_rate: 30,
    format_type: UncompressedFormat::Yuy2,
};

// H.264 压缩格式
let h264_format = VideoFormat::H264 {
    width: 1280,
    height: 720,
    frame_rate: 60,
};
```

## 示例程序

### 视频捕获示例

```bash
cargo run --example capture_video
```

这个示例演示了如何：

- 检测和连接 UVC 设备
- 设置视频格式和控制参数
- 捕获视频帧并保存到文件
- 计算帧率统计信息

## 测试

运行单元测试：

```bash
cargo test
```

运行集成测试（需要连接 UVC 设备）：

```bash
cargo test --test integration_tests
```

## UVC 协议支持

### Video Control Interface (VCI)

- 设备枚举和能力查询
- 处理单元 (Processing Unit) 控制
- 摄像头终端 (Camera Terminal) 控制
- 状态中断端点处理

### Video Streaming Interface (VSI)

- 格式和帧描述符解析
- 同步 (Isochronous) 数据传输
- UVC 载荷头解析
- 帧边界检测

### 控制请求

- `GET_CUR`, `SET_CUR`: 获取/设置当前值
- `GET_MIN`, `GET_MAX`: 获取参数范围
- `GET_RES`: 获取分辨率步长
- `GET_INFO`: 获取控制信息

## 依赖项

- `crab-usb`: USB 主机驱动核心库
- `log`: 日志输出
- `tokio`: 异步运行时 (示例程序)

## 架构设计

```
┌─────────────────┐    ┌──────────────────┐    ┌─────────────────┐
│   Application   │◄──►│   UVC Library    │◄──►│   crab-usb      │
│                 │    │                  │    │                 │
└─────────────────┘    └──────────────────┘    └─────────────────┘
                              │                          │
                              ▼                          ▼
                       ┌──────────────┐         ┌──────────────┐
                       │ Video Control│         │ USB Transfer │
                       │ & Streaming  │         │   Engine     │
                       └──────────────┘         └──────────────┘
```

## 注意事项

1. **权限要求**: 在 Linux 系统上可能需要 root 权限或配置 udev 规则来访问 USB 设备
2. **带宽限制**: USB 2.0 和 3.0 的带宽限制会影响支持的最大分辨率和帧率
3. **设备兼容性**: 不同厂商的 UVC 设备可能有轻微的实现差异
4. **内存使用**: 高分辨率视频流会消耗大量内存，建议合理设置缓冲区大小

## 相关资源

- [USB Video Class 1.5 规范](https://www.usb.org/document-library/video-class-v15-document-set)
- [UVC 实现指南](https://docs.microsoft.com/en-us/windows-hardware/drivers/stream/usb-video-class-driver)
- [crab-usb 项目](https://github.com/drivercraft/CrabUSB)

## 许可证

本项目使用与 CrabUSB 相同的许可证。
