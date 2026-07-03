# doomgeneric Wayland 集成测试

在 StarryOS 上运行 Weston (DRM backend + GL/llvmpipe 渲染器) 作为 Wayland
compositor，然后通过 doomgeneric (SDL2 Wayland 输出) 运行 Doom 游戏，
验证从内核 DRM 到用户态渲染的全链路连通性。

## 当前状态

| 组件 | 状态 | 备注 |
|---|---|---|
| Weston DRM + GL (llvmpipe) | ✅ 正常 | `--renderer=gl` 路径，214x120 分辨率 |
| doomgeneric + SDL Wayland | ✅ 正常 | SDL_RENDER_DRIVER=opengles2 + LIBGL_ALWAYS_SOFTWARE=1 |

## 分辨率说明

测试使用 **214x120** 分辨率，与 ffplay 测试的 284x160 不同。
这是为了验证 DRM 驱动支持任意分辨率的模式设置。

- QEMU 配置: `virtio-gpu-pci,xres=214,yres=120`
- Weston: 自动检测 DRM 分辨率

## 内核需求

- `/dev/dri/card0` — DRM 设备（CREATE_DUMB、MAP_DUMB、ADDFB2、GETCAP）、
  PRIME dma-buf 导出/导入（HANDLE_TO_FD / FD_TO_HANDLE）
- `/dev/input/event*` — evdev 输入（鼠标、键盘）
- `memfd_create` + seal 支持
- AF_UNIX SCM_RIGHTS 文件描述符传递
- sysfs 设备枚举

## 测试流程

1. 启动 Weston（`kiosk-shell`，DRM 后端，GL/llvmpipe 渲染器）
2. 等待 Wayland socket 就绪（最多 120 秒）
3. 启动 doomgeneric（SDL_VIDEODRIVER=wayland，SDL_RENDER_DRIVER=opengles2，LIBGL_ALWAYS_SOFTWARE=1）
4. 验证进程存活 35 秒
5. 输出 `DOOMGENERIC_TEST_PASSED` 或 `DOOMGENERIC_TEST_FAILED`

## 构建说明

doomgeneric 从源代码构建，源代码位于 `apps/starry/doom/doomgeneric-master.tar.gz`。

### 构建流程

1. 解压 `doomgeneric-master.tar.gz` 到临时目录
2. 修改 `Makefile.sdl` 使用 `gcc` 而不是 `clang`
3. 使用 `qemu-user` 运行 Alpine GCC，编译 doomgeneric（链接 Alpine musl）
4. 复制编译好的二进制文件到 rootfs overlay

### 主机依赖

构建需要在主机上安装以下工具（Docker 镜像已预装）：

- `debugfs` (e2fsprogs) — 操作 rootfs 镜像
- `install` (coreutils) — 复制文件
- `readelf` (binutils) — 解析动态库依赖
- `qemu-user-static` — 运行 Alpine 工具链

### 依赖的 Alpine 包

| 包 | 用途 |
|---|---|
| weston + weston-backend-drm | Wayland compositor + DRM 后端 |
| mesa + mesa-egl + mesa-gl + mesa-dri-gallium + mesa-gbm | Mesa GL 库和 DRI 驱动 |
| libinput + libxkbcommon + xkeyboard-config | 输入 + 键盘映射 |
| pixman | 软件渲染回退 |
| freedoom | Doom WAD 文件（开源版） |
| sdl2 + sdl2_mixer | SDL2 渲染库（Wayland 后端） |
| gcc + make + musl-dev + sdl2-dev + sdl2_mixer-dev | 编译工具链和头文件 |

## 构建与运行

```bash
cargo xtask starry app qemu -t doom --arch x86_64
```

这会依次：
- 构建 StarryOS 内核（含 DRM display + PRIME dma-buf 支持）
- 运行 `prebuild.sh` 构建 rootfs overlay（安装 Alpine 包、编译 doomgeneric、复制运行时库、复制 Freedoom WAD）
- 启动 QEMU（virtio-gpu-pci，VNC :0，4 核 2G，214x120 分辨率，TCG 加速）
- 等待测试结果（QEMU 进程超时 180 秒）

## 查看画面

QEMU 使用 VNC 显示输出，需要通过 VNC 连接查看：

```bash
vncviewer localhost:5900
```

或浏览器打开 `http://localhost:6080/vnc.html`（需 noVNC）。

## Docker 内运行

```bash
docker run --rm \
  -v $(pwd):/workspace \
  -w /workspace \
  --network host \
  ghcr.io/rcore-os/tgoskits-container:latest \
  cargo xtask starry app qemu -t doom --arch x86_64
```

VNC 端口 `:0` 映射到宿主机 `localhost:5900`，`--network host` 确保 VNC
连接可达。

## 与 ffplay 测试的区别

| 项目 | ffplay | doomgeneric |
|---|---|---|
| 分辨率 | 284x160 | 214x120 |
| Weston 渲染器 | GL (llvmpipe) | GL (llvmpipe) |
| 测试内容 | 视频播放 | 游戏运行 |
| 输入支持 | 无 | 鼠标 + 键盘（virtio-tablet + virtio-keyboard） |

## 故障排除

### 无 DRM 设备
确保 QEMU 配置包含 `virtio-gpu-pci` 设备。

### Weston 启动失败
检查 weston 日志：`/tmp/weston.log` 和 `/tmp/weston-stderr.log`

### doomgeneric 无响应
检查 doomgeneric 日志：`/tmp/doomgeneric_stderr.log`
