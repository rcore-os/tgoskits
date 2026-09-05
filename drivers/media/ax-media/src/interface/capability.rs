//! 设备能力（Device Capability）结构与标志

use bitflags::bitflags;

bitflags! {
    /// VIDIOC_QUERYCAP 返回的设备能力标志
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Capabilities: u32 {
        const VIDEO_CAPTURE = 0x0000_0001; // 是视频采集设备
        const VIDEO_OUTPUT = 0x0000_0002; // 支持视频输出
        const VIDEO_OVERLAY = 0x0000_0004; // 支持视频叠加
        const VBI_CAPTURE = 0x0000_0010; // 是原始 VBI 采集设备
        const VBI_OUTPUT = 0x0000_0020; // 是原始 VBI 输出设备
        const SLICED_VBI_CAPTURE = 0x0000_0040; // 是切片 VBI（sliced VBI）采集设备
        const SLICED_VBI_OUTPUT = 0x0000_0080; // 是切片 VBI（sliced VBI）输出设备
        const RDS_CAPTURE = 0x0000_0100; // 支持 RDS 数据采集
        const VIDEO_OUTPUT_OVERLAY = 0x0000_0200; // 支持视频输出叠加
        const HW_FREQ_SEEK = 0x0000_0400; // 支持硬件频率搜索
        const RDS_OUTPUT = 0x0000_0800; // 是 RDS 编码器
        const VIDEO_CAPTURE_MPLANE = 0x0000_1000; // 支持多平面（multiplanar）的视频采集
        const VIDEO_OUTPUT_MPLANE = 0x0000_2000; // 支持多平面（multiplanar）的视频输出
        const VIDEO_M2M_MPLANE = 0x0000_4000; // 支持多平面（multiplanar）的视频内存到内存（mem-to-mem）处理
        const VIDEO_M2M = 0x0000_8000; // 视频内存到内存（mem-to-mem）设备
        const TUNER = 0x0001_0000; // 具有调谐器（Tuner）
        const AUDIO = 0x0002_0000; // 支持音频
        const RADIO = 0x0004_0000; // 是收音机设备
        const MODULATOR = 0x0008_0000; // 具有调制器（Modulator）
        const SDR_CAPTURE = 0x0010_0000; // 是 SDR 采集设备
        const EXT_PIX_FORMAT = 0x0020_0000; // 支持扩展像素格式
        const SDR_OUTPUT = 0x0040_0000; // 是 SDR 输出设备
        const META_CAPTURE = 0x0080_0000; // 是元数据采集设备
        const READWRITE = 0x0100_0000; // 支持 read/write 系统调用
        const STREAMING = 0x0400_0000; // 支持流式 I/O ioctl
        const META_OUTPUT = 0x0800_0000; // 是元数据输出设备
        const TOUCH = 0x1000_0000; // 是触摸设备
        const IO_MC = 0x2000_0000; // I/O 由媒体控制器（media controller）控制
        const EDID = 0x0200_0000; // 是仅支持 EDID 的设备
        const DEVICE_CAPS = 0x8000_0000; // 设置设备能力字段
    }
}

/// VIDIOC_QUERYCAP 返回的设备能力信息
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Capability {
    pub driver: [u8; 16],           // [out] 驱动名称
    pub card: [u8; 32],             // [out] 设备卡名称
    pub bus_info: [u8; 32],         // [out] 总线信息
    pub version: u32,               // [out] 内核版本
    pub capabilities: Capabilities, // [out] 设备能力
    pub device_caps: Capabilities,  // [out] 设备节点能力
    pub reserved: [u32; 3],
}
