# Starry FFmpeg App

This case runs FFmpeg inside StarryOS through the app runner.

## FFmpeg功能介绍

FFmpeg是一套完整的、跨平台的音视频录制、转换和流媒体解决方案。

### 核心功能：
1. **格式转换** - 支持几乎所有音视频格式之间的相互转换
2. **编解码** - 支持 H.264, H.265, VP8, VP9, AV1, MPEG-4 等视频编码；AAC, MP3, Opus, FLAC, Vorbis 等音频编码
3. **容器格式** - 支持 MP4, MKV, AVI, WebM, FLV, MOV, TS 等容器格式
4. **流媒体** - 支持 HTTP, RTMP, RTSP, UDP, TCP 等网络协议
5. **滤镜** - 支持视频缩放、裁剪、旋转、去噪等滤镜操作
6. **多线程** - 支持多线程编解码，充分利用多核CPU
7. **元数据** - 支持提取和修改音视频文件的元数据信息

### 系统调用依赖：
FFmpeg 依赖以下系统调用（按优先级排序）：
- **文件 I/O**: open, read, write, lseek, close, fstat, mmap
- **内存管理**: mmap, munmap, mprotect, brk
- **多线程**: clone, futex, sched_yield, sched_getaffinity
- **时间**: clock_gettime, nanosleep
- **网络**: socket, connect, bind, send, recv, select/poll/epoll
- **信号**: rt_sigaction, rt_sigprocmask
- **进程**: getpid, gettid, prctl

## 运行命令

默认运行全部测试（smoke + basic + thread + codec + network）：

```bash
cargo xtask starry app qemu -t ffmpeg --arch x86_64
```

也可以单独运行某个级别：

```bash
# 仅 Smoke 测试（基本功能验证）
cargo xtask starry app qemu -t ffmpeg --arch x86_64 --qemu-config qemu-x86_64-smoke.toml

# 仅基础测试（格式转换、滤镜等）
cargo xtask starry app qemu -t ffmpeg --arch x86_64 --qemu-config qemu-x86_64-basic.toml

# 仅多线程测试
cargo xtask starry app qemu -t ffmpeg --arch x86_64 --qemu-config qemu-x86_64-thread.toml

# 仅编解码器测试
cargo xtask starry app qemu -t ffmpeg --arch x86_64 --qemu-config qemu-x86_64-codec.toml

# 仅网络测试
cargo xtask starry app qemu -t ffmpeg --arch x86_64 --qemu-config qemu-x86_64-network.toml
```

## 测试内容

### Smoke测试 (ffmpeg-smoke-tests.sh) — 12 个阶段
- ffmpeg -version 输出验证
- ffmpeg -h 帮助信息
- -codecs 编解码器列表
- -formats 格式列表
- -demuxers 解复用器列表
- -muxers 复用器列表
- -protocols 协议列表
- -filters 滤镜列表
- -pix_fmts 像素格式列表
- -sample_fmts 采样格式列表
- -bsfs 位流过滤器列表
- -buildconf 编译配置

### 基础测试 (ffmpeg-basic-tests.sh) — 23 个阶段
- ffprobe 媒体信息探测
- 格式识别（MP4, WAV）
- 流信息提取（视频流、音频流）
- MP4 重封装（MP4 -> MP4）
- MP4 -> MKV 容器转换
- MP4 -> AVI 容器转换
- WAV -> MP3 音频转码
- WAV -> AAC 音频转码
- 视频缩放（160x120 -> 80x60）
- 视频裁剪
- 帧提取（视频 -> PNG）
- 元数据提取（JSON格式）
- 时长裁剪
- 文件拼接（concat demuxer）
- 音频重采样（采样率转换）
- 音频声道转换（单声道 -> 立体声）
- 像素格式转换（yuv420p -> rgb24）
- 图像序列输出（image2）
- GIF 生成
- 错误处理（损坏输入）
- 多流映射（分离视频/音频流）
- 复杂滤镜链（scale + eq）
- 流复制 vs 转码一致性验证

### 多线程测试 (ffmpeg-thread-tests.sh) — 12 个阶段
- 单线程基线编码
- 双线程编码
- 四线程编码
- 输出一致性验证（单线程 vs 多线程分辨率一致）
- 多线程解码
- 同时编解码（pipeline）
- 多线程音频编码
- A/V同步 + 多线程
- 多线程滤镜链（scale + crop）
- 并发流水线（两个并行 ffmpeg 进程）
- 多线程解码为原始帧
- 多线程音频重采样 + 编码

### 编解码器测试 (ffmpeg-codec-tests.sh) — 29 个阶段
- H.264 (libx264) 编码
- H.264 (libx264) 解码
- MPEG-4 编码
- MPEG-4 解码
- VP8 (libvpx) 编码
- VP8 (libvpx) 解码
- VP9 (libvpx-vp9) 编码
- MJPEG 编码
- MJPEG 解码
- Raw Video 编码
- MP3 (libmp3lame) 编码
- MP3 (libmp3lame) 解码
- AAC 编码
- AAC 解码
- Vorbis (libvorbis) 编码
- Opus (libopus) 编码
- Opus (libopus) 解码
- FLAC 编码
- FLAC 解码
- MKV 容器重封装
- AVI 容器重封装
- WebM 容器编码/重封装
- 跨容器转码（MP4 -> WebM -> MKV）
- H.265 (libx265) 编码
- H.265 (libx265) 解码
- 音频采样格式转换（s16 -> f32）
- 音频码率阶梯（64/128/192/256k）
- 音视频合流（video + audio mux）
- 视频分辨率阶梯（80x60 / 160x120）

### 网络测试 (ffmpeg-network-tests.sh) — 12 个阶段
- 协议支持检测（http, tcp, udp, rtmp, rtsp等）
- file:// 协议访问
- pipe:0/pipe:1 管道协议
- HTTP 服务器搭建（python3 http.server）
- HTTP 输入下载 + 解码
- HTTP 输入 + 转码
- HTTP 音频输入
- HTTP Seek（Range请求）
- TCP 回环传输
- UDP 回环传输
- HTTP 客户端获取验证
- 扩展协议检测（RTMP, RTSP, MMS, RTP, SRT）

## 文件结构

```
apps/starry/ffmpeg/
├── prebuild.sh                    # 构建脚本，安装ffmpeg到rootfs
├── test_ffmpeg.sh                 # 统一入口，按序运行全部测试
├── ffmpeg-ensure-media.sh         # 共享脚本，QEMU内生成测试媒体
├── ffmpeg-smoke-tests.sh          # Smoke测试脚本
├── ffmpeg-basic-tests.sh          # 基础功能测试脚本
├── ffmpeg-thread-tests.sh         # 多线程测试脚本
├── ffmpeg-codec-tests.sh          # 编解码器测试脚本
├── ffmpeg-network-tests.sh        # 网络测试脚本
├── build-*.toml                   # 构建配置
├── qemu-x86_64.toml               # 默认配置（运行全部测试）
├── qemu-x86_64-smoke.toml         # 仅Smoke测试
├── qemu-x86_64-basic.toml         # 仅基础测试
├── qemu-x86_64-thread.toml        # 仅多线程测试
├── qemu-x86_64-codec.toml         # 仅编解码器测试
├── qemu-x86_64-network.toml       # 仅网络测试
└── README.md                      # 本文件
```

## 依赖

### 宿主机构建依赖（prebuild.sh 自动检查/安装）

- `apk-tools`（apk 包管理）
- `e2fsprogs`（debugfs 提取 rootfs）
- `coreutils`（install 命令）
- `binutils`（readelf 依赖分析）

### 客户机运行时依赖（apk 安装到 rootfs）

- ffmpeg（主程序）
- ffmpeg-libs（运行时库）
- python3（网络测试 HTTP 服务器）

## 测试媒体

测试媒体在 QEMU 运行时由 `ffmpeg-ensure-media.sh` 通过客户机自身的 ffmpeg 自动生成（使用 `lavfi` 虚拟输入源），无需宿主机安装 ffmpeg。

| 文件 | 格式 | 用途 |
|------|------|------|
| `test_160x120.mp4` | H.264 MP4 | 基础、编解码、网络测试 |
| `test_audio.mp3` | MP3 | 编解码、网络测试 |
| `test_160x120.mkv` | H.264 MKV | 编解码测试 |
| `test_160x120.avi` | MPEG-4 AVI | 编解码测试 |
| `test_av.mp4` | H.264+AAC MP4 | 基础、多线程测试 |
| `test_audio.wav` | PCM WAV | 基础、多线程测试 |

如果任一媒体文件生成失败，对应测试会立即 **FAIL**。

## 排查建议

如果测试失败，可以按以下顺序排查：

1. **Smoke测试失败** → FFmpeg 基本功能有问题，检查动态链接库是否完整
2. **基础测试失败** → 文件 I/O 相关系统调用缺失（open, read, write, lseek, mmap）
3. **多线程测试失败** → 线程相关系统调用缺失（clone, futex, sched_*）
4. **编解码器测试失败** → 编解码器依赖库缺失（libx264/libvpx/libmp3lame等）或系统调用异常
5. **网络测试失败** → 网络系统调用缺失（socket, connect, bind, send, recv）或 python3/测试媒体缺失
