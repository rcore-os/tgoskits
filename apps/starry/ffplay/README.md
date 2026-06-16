# ffplay Wayland 集成测试!

在 StarryOS 上运行 Weston (DRM backend + GL/llvmpipe 渲染器) 作为 Wayland
compositor，然后通过 ffplay (SDL2 Wayland 输出) 播放测试视频，验证从内核
DRM 到 Mesa 用户态再到 Wayland 协议的全链路连通性。

## 当前状态

| 组件 | 状态 | 备注 |
|---|---|---|
| Weston DRM + pixman | ✅ 正常 | `--renderer=pixman` 路径 |
| Weston DRM + GL (llvmpipe) | ✅ 正常 | `--renderer=gl` 路径，启动约 5-8s |
| Mesa EGL/eglinfo | ✅ 正常 | EGL 1.5 + Mesa Project，llvmpipe fallback OK |
| ffplay + SDL Wayland | ✅ 正常 | 160p 5fps 视频播放，全栈通过 |
| ffplay + SDL dummy | ✅ 正常 | 视频解码 + 软件渲染完全通过 |

## 当前已知问题

- **DIRTYFB 空操作**（`card0.rs:811`）：Weston 的 pixman 路径需要
  `DRM_IOCTL_MODE_DIRTYFB` 来刷新画面，当前 accept-and-ignore。PR #1160
  已包含修复，正在等待合入。GL 路径（当前测试）不受影响。
- **SCM_RIGHTS fd 泄漏**（`io.rs`）：当 CMSG buffer 空间不足时，`add_file_like`
  在确认空间前就被调用，可能导致 fd 泄漏。

## 内核需求

- `/dev/dri/card0` — DRM 设备（CREATE_DUMB、MAP_DUMB、ADDFB2、GETCAP）、
  PRIME dma-buf 导出/导入（HANDLE_TO_FD / FD_TO_HANDLE）
- `/dev/fb0` — framebuffer 设备
- `/dev/input/event*` — evdev 输入
- `memfd_create` + seal 支持
- AF_UNIX SCM_RIGHTS 文件描述符传递
- sysfs 设备枚举

## 关于 GL 渲染

Weston compositor 使用 `--renderer=gl` 启动，通过 llvmpipe GL
渲染器运行，这验证了 Mesa 库、DRI 驱动（swrast_dri.so）、DRM 接口
（GETCAP/PRIME/CREATE_DUMB/ADDFB2）在内核和用户空间均正常工作。

注意：此测试验证的是 **Weston 侧**的 Mesa/llvmpipe GL 渲染能力（合成器渲染
使用 GL），而非客户端（ffplay）的 GL 渲染。SDL2 的 Wayland 后端在 llvmpipe
上无法获取 `EGL_WL_bind_wayland_display` 扩展（Mesa 上游设计），因此 ffplay
会回退到 Wayland SHM 软件渲染管线播放视频。这一路径同样有用——它验证了
wl_shm 协议、memfd 共享内存和 AF_UNIX 通信的完整性。

## 测试流程

1. 启动 Weston（`kiosk-shell`，DRM 后端，GL 渲染器 llvmpipe）
2. 等待 Wayland socket 就绪（GL 初始化最多等 45 秒）
3. 用 ffplay 播放测试视频（SDL_VIDEODRIVER=wayland，-x 284 -y 160，160p 5fps）
4. 输出 `FFPLAY_TEST_PASSED` 或 `FFPLAY_TEST_FAILED`

## 构建与运行

```bash
cargo xtask starry app qemu -t ffplay --arch x86_64
```

这会依次：
- 构建 StarryOS 内核（含 DRM display + PRIME dma-buf 支持）
- 运行 `prebuild.sh` 构建 rootfs overlay（安装 Alpine 包、拷贝 Mesa/GL/SDL
  运行时库、下载并压缩测试视频到 160p）
- 启动 QEMU（virtio-gpu-pci，VNC :0，4 核 2G）
- 等待测试结果（QEMU 进程超时 3600 秒，ffplay 超时 180 秒）

## 查看画面

QEMU 没有 GTK 窗口，需要通过 VNC 连接查看：

```bash
# 连接 VNC
vncviewer localhost:5900
```

或浏览器打开 `http://localhost:6080/vnc.html`（需 noVNC）。

## 手动运行

如果只想构建 overlay 不进 QEMU：

```bash
bash apps/starry/ffplay/prebuild.sh
```

需要设置环境变量 `STARRY_ROOTFS`、`STARRY_STAGING_ROOT`、
`STARRY_OVERLAY_DIR`。

## Docker 内运行

```bash
docker run --rm \
  -v $(pwd):/workspace \
  -w /workspace \
  --network host \
  ghcr.io/rcore-os/tgoskits-container:latest \
  cargo xtask starry app qemu -t ffplay --arch x86_64
```

VNC 端口 `:0` 映射到宿主机 `localhost:5900`，`--network host` 确保 VNC
连接可达。

## 依赖的 Alpine 包

| 包 | 用途 |
|---|---|
| weston + weston-backend-drm | Wayland compositor + DRM 后端 |
| mesa-gbm | GBM 缓冲区管理（drm-backend 需要） |
| mesa + mesa-egl + mesa-gl + mesa-dri-gallium | Mesa GL 库和 DRI 驱动 |
| libinput | 输入设备抽象 |
| libxkbcommon + xkeyboard-config | 键盘映射编译 |
| pixman | 软件渲染引擎 |
| ffplay + sdl2 | 带 SDL2 Wayland 后端的媒体播放器 |
| ffmpeg | 合成测试视频（下载失败时备用） |
| wget | 下载测试视频 |
