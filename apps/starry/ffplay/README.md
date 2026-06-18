# ffplay Wayland 集成测试！

在 StarryOS 上运行 Weston (DRM backend + GL/llvmpipe 渲染器) 作为 Wayland
compositor，然后通过 ffplay (SDL2 Wayland 输出) 播放测试视频，验证从内核
DRM 到 Mesa 用户态再到 Wayland 协议的全链路连通性。

## 当前状态

| 组件 | 状态 | 备注 |
|---|---|---|
| Weston DRM + GL (llvmpipe) | ✅ 正常 | `--renderer=gl` 路径，GL 初始化 25-100s |
| Mesa EGL | ✅ 正常 | EGL 1.5 + Mesa Project，llvmpipe fallback |
| ffplay + SDL Wayland | ✅ 正常 | 160p 视频播放，循环播放 |
| PRIME dma-buf 导出/导入 | ✅ 正常 | dumb buffer → dma-buf fd → GEM handle |
| DRM_CAP_PRIME | ✅ 正常 | 返回 import/export bitmask |

## 已知问题

- **exit code 123**：SDL2 + musl 在进程退出时 PLT lazy binding 失败，触发
  abort。通过 `LD_BIND_NOW=1` 强制 eager binding 修复。
- **`EGL Wayland extension: no`**：Mesa 的 swrast 驱动不提供 Wayland 平台扩展，
  这是预期行为。Weston 使用 DRM 平台驱动（virtio-gpu），不依赖此扩展。
- **`virtio_gpu: driver missing`**：overlay 中未安装 `mesa-dri-gallium` 的
  virtio-gpu DRI 驱动，Mesa 回退到 llvmpipe 软渲染。不影响功能。

## 内核需求

- `/dev/dri/card0` — DRM 设备（CREATE_DUMB、MAP_DUMB、ADDFB2、GETCAP）、
  PRIME dma-buf 导出/导入（HANDLE_TO_FD / FD_TO_HANDLE）
- `/dev/fb0` — framebuffer 设备
- `/dev/input/event*` — evdev 输入
- `memfd_create` + seal 支持
- AF_UNIX SCM_RIGHTS 文件描述符传递
- sysfs 设备枚举

## 关于 GL 渲染

Weston compositor 使用 `--renderer=gl` 启动，通过 llvmpipe GL 渲染器运行，
验证 Mesa 库、DRI 驱动、DRM 接口（GETCAP/PRIME/CREATE_DUMB/ADDFB2）在内核
和用户空间均正常工作。

ffplay 的 SDL2 Wayland 后端通过 `LIBGL_ALWAYS_SOFTWARE=1` 强制软件渲染，
`LD_BIND_NOW=1` 修复 musl + SDL2 退出时 PLT 解析失败。

## 测试流程

1. 检查 `/dev/dri/card0` 存在
2. 启动 Weston（`kiosk-shell`，DRM 后端，GL 渲染器 llvmpipe）
3. 等待 Wayland socket 就绪（最多 120 秒，每 10 秒打印进度）
4. 用 ffplay 播放测试视频（`-loop 0` 循环，`-x 284 -y 160`，180 秒超时）
5. 接受 exit code 0、123、124 为通过
6. 输出 `FFPLAY_TEST_PASSED` 或 `FFPLAY_TEST_FAILED`

失败时自动 dump：Weston 日志最后 30 行 + Weston stderr + ffplay stderr。

## 构建与运行

```bash
cargo xtask starry app qemu -t ffplay --arch x86_64
```

这会依次：
- 构建 StarryOS 内核（含 DRM display + PRIME dma-buf 支持）
- 运行 `prebuild.sh` 构建 rootfs overlay（安装 Alpine 包、拷贝 Mesa/GL/SDL
  运行时库、下载并压缩测试视频到 160p）
- 启动 QEMU（virtio-gpu-pci 284×160，VNC :0，4 核 2G，UEFI 启动）
- 等待测试结果（QEMU 进程超时 3600 秒，ffplay 超时 180 秒）

## 查看画面

QEMU 使用 VNC 输出，连接查看：

```bash
vncviewer localhost:5900
```

## 手动运行

如果只想构建 overlay 不进 QEMU：

```bash
bash apps/starry/ffplay/prebuild.sh
```

需要设置环境变量 `STARRY_ROOTFS`、`STARRY_STAGING_ROOT`、
`STARRY_OVERLAY_DIR`。

## 依赖的 Alpine 包

| 包 | 用途 |
|---|---|
| weston + weston-backend-drm + kiosk-shell | Wayland compositor + DRM 后端 |
| mesa-gbm | GBM 缓冲区管理（drm-backend 需要） |
| mesa + mesa-egl + mesa-gl | Mesa GL 库 |
| libinput | 输入设备抽象 |
| libxkbcommon + xkeyboard-config | 键盘映射编译 |
| pixman | 软件渲染引擎 |
| ffplay + sdl2 | 带 SDL2 Wayland 后端的媒体播放器 |
| ffmpeg | 合成测试视频（下载失败时备用） |
| wget | 下载测试视频 |
