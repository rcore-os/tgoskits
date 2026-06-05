# Starry Wayland/Weston 应用

本应用用例在 StarryOS 上通过 QEMU virtio GPU 和输入设备运行 Weston
（Wayland 参考合成器）。自动化测试用于证明合成器可以通过 DRM 后端启动并
接受 Wayland 客户端连接。下面的手动流程会用 VNC 暴露同一套图形栈，便于交互式
运行 `gtk4-demo`。

## 宿主机前置条件

- 安装需要使用的 QEMU system emulator。
- 安装 Rust nightly 和本仓库常规构建依赖。
- 安装 e2fsprogs 的 `debugfs`。macOS + Homebrew 可使用：

```bash
brew install e2fsprogs
export PATH="/opt/homebrew/opt/e2fsprogs/sbin:$PATH"
```

以下命令都在仓库根目录执行。

## 自动化测试

通过 `xtask` 运行 Starry app 测试：

```bash
cargo xtask starry app qemu -t wayland --arch riscv64
cargo xtask starry app qemu -t wayland --arch x86_64
```

成功输出会包含两个标记：

```text
WAYLAND_TEST_RESULT PASSED
WAYLAND_TEST_PASSED
```

客体内脚本是 [`wayland-test.sh`](wayland-test.sh)。它会从 Alpine apk 安装
`weston`、`weston-backend-drm` 和 `weston-shell-desktop`，检查
`/dev/dri/card0` 是否存在，检查 `/dev/input/event*`，用 DRM/pixman 后端启动
Weston，等待 `/tmp/wayland-*` socket 出现，在 `weston-info` 可用时连接一个
客户端，扫描 Weston 日志中的明显启动错误，然后干净关闭合成器。

自动化测试覆盖的内核路径：

| 子系统 | 设备 / 系统调用 | 说明 |
|--------|------------------|------|
| DRM/KMS | `/dev/dri/card0` | Dumb buffer、modeset、page flip |
| 输入 | `/dev/input/event*` | evdev 协议、libinput 探测 |
| memfd | `memfd_create` | Wayland SHM buffer 后端存储 |
| eventfd | `eventfd` | 合成器事件循环信号 |
| Unix sockets | `bind` / `sendmsg` / `SCM_RIGHTS` | Wayland socket 和 fd 传递 |

## 使用 VNC 手动复现

手动流程刻意绕过 app 测试里的 `shell_init_cmd`，直接启动同一个内核和 Alpine
rootfs。这样可以在 StarryOS shell 中手动输入命令，并通过 VNC 与 GTK 窗口交互。
两个架构进入客体后的 Weston 和 GTK 命令完全相同，只有宿主机侧的 QEMU 启动命令不同。

### 第一步：构建内核和 rootfs

先对要复现的架构运行一次自动化测试。该步骤会生成后续直接启动 QEMU 所需的内核镜像
和 rootfs 镜像。

```bash
export PATH="/opt/homebrew/opt/e2fsprogs/sbin:$PATH"
ARCH=riscv64   # 或：x86_64
cargo xtask starry app qemu -t wayland --arch "$ARCH"
```

### 第二步：复制手动会话使用的 rootfs

使用副本，避免手动安装包和调试操作污染 app runner 使用的 rootfs。

```bash
mkdir -p tmp/wayland-manual
cp "tmp/axbuild/rootfs/rootfs-${ARCH}-alpine.img" "tmp/wayland-manual/${ARCH}.img"
```

### 第三步：用 VNC 显示启动 QEMU

选择与 `ARCH` 对应的启动命令。这里的差异只有 QEMU binary、machine type 和 kernel
镜像路径。VNC display number 可以任选一个空闲值；对应 TCP 端口是
`5900 + VNC_DISPLAY`。

```bash
VNC_DISPLAY=30  # 示例；可改成任意空闲的 QEMU VNC display number
VNC_PORT=$((5900 + VNC_DISPLAY))
```

RISC-V：

```bash
qemu-system-riscv64 \
  -machine virt \
  -kernel target/riscv64gc-unknown-none-elf/release/starryos.bin \
  -m 1G \
  -cpu rv64 \
  -serial stdio \
  -monitor none \
  -vnc "127.0.0.1:${VNC_DISPLAY}" \
  -device virtio-gpu-pci \
  -device virtio-keyboard-pci \
  -device virtio-mouse-pci \
  -device virtio-blk-pci,drive=disk0 \
  -drive id=disk0,if=none,format=raw,file=tmp/wayland-manual/riscv64.img \
  -device virtio-net-pci,netdev=net0 \
  -netdev user,id=net0
```

x86_64 需要通过 `xtask` 启动 QEMU，因为当前 dynamic x86_64 平台通过生成的
OVMF/ESP 启动，不能再直接使用 `qemu-system-x86_64 -kernel starryos`。创建一个
手动 VNC QEMU 配置，并把 `<display>` 替换成 `VNC_DISPLAY`：

```toml
args = [
  "-m", "1G",
  "-serial", "stdio",
  "-monitor", "none",
  "-vnc", "127.0.0.1:<display>",
  "-machine", "q35",
  "-device", "virtio-gpu-pci",
  "-device", "virtio-keyboard-pci",
  "-device", "virtio-mouse-pci",
  "-device", "virtio-blk-pci,drive=disk0",
  "-drive", "id=disk0,if=none,format=raw,file=${workspace}/tmp/wayland-manual/x86_64.img",
  "-device", "virtio-net-pci,netdev=net0",
  "-netdev", "user,id=net0",
]
uefi = true
to_bin = true
timeout = 900
fail_regex = ["(?i)\\bpanic(?:ked)?\\b"]
```

然后启动：

```bash
cargo xtask starry app qemu \
  -t wayland \
  --arch x86_64 \
  --qemu-config tmp/wayland-manual/qemu-x86_64-vnc.toml
```

等待串口控制台出现 `root@starry:` 提示符。

### 第四步：打开 VNC 查看器

在宿主机上打开显示：

```bash
open "vnc://127.0.0.1::${VNC_PORT}"
```

有些 VNC 客户端手动输入地址时更喜欢 `127.0.0.1:${VNC_PORT}`。双冒号形式表示显式
TCP 端口，许多命令行 VNC 工具使用这种写法。

### 第五步：在客体内安装用户态包

在 `root@starry:` 提示符下执行：

```sh
apk add weston weston-backend-drm weston-shell-desktop gtk4.0-demo
```

这会安装 Weston、DRM 后端插件、desktop shell 插件、GTK4、Mesa、libdrm、libinput
以及相关运行时依赖。

### 第六步：启动 Weston

继续在 StarryOS 内执行：

```sh
export XDG_RUNTIME_DIR=/tmp
chmod 0700 /tmp
export LIBSEAT_BACKEND=noop
rm -f /tmp/wayland-*

weston \
  --backend=drm-backend.so \
  --renderer=pixman \
  --no-config \
  --idle-time=0 \
  --log=/tmp/weston.log &
```

`/tmp/weston.log` 中应能看到 `Virtual-1` DRM head 和启用的输出。确认 Wayland
socket 已创建：

```sh
ls -l /tmp/wayland-*
```

### 第七步：启动 GTK4 Demo

```sh
export WAYLAND_DISPLAY="$(basename "$(ls /tmp/wayland-* | head -1)")"
gtk4-demo &
ps | grep gtk4-demo
```

GTK4 demo 窗口应出现在 VNC 查看器中。使用 VNC 的鼠标和键盘点击控件、打开 demo
条目、滚动列表、关闭或重新打开 demo 窗口。需要更多合成器证据时可查看：

```sh
tail -100 /tmp/weston.log
```

### 第八步：可选的 SHM 客户端检查

如果镜像中存在 `weston-simple-shm`，可以用它作为很小的 SHM 渲染客户端：

```sh
weston-simple-shm &
```

### 第九步：关闭

```sh
pkill gtk4-demo || true
pkill weston || true
poweroff
```

## aarch64 说明

aarch64 Wayland 运行当前在进入客体 shell 前被另一个 StarryOS 内核问题阻塞：
`plat_dyn = true` 路径上的 `ax_net_ng::init_network()` 会卡住。这个 hang 会阻止自动化
app 脚本和 Cocoa 辅助脚本到达 Weston/GTK 步骤；问题不在 Wayland app case 本身。

实验性的 Cocoa 辅助脚本保留在 [`run-hvf.sh`](run-hvf.sh)：

```bash
./apps/starry/wayland/run-hvf.sh
```

等 aarch64 网络初始化 hang 修复后可继续使用。

## 内核侧依赖

本应用需要：

- 支持 dumb buffer 分配的 DRM `/dev/dri/card0`。
- QEMU 配置中启用 virtio GPU、keyboard、mouse、block 和 network 设备。
- 支持 libinput 枚举的 evdev `/dev/input/event*`。
- Wayland SHM 所需的 `memfd_create` 和 Unix socket fd 传递。
- 合成器事件循环使用的 `eventfd`。
- libinput 设备发现所需的 `/run/udev/data/` udev seed。
- app build config 中启用 `starry-kernel/input` 和 `ax-feat/display`。
