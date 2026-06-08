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
cargo xtask starry app qemu -t wayland --arch aarch64
cargo xtask starry app qemu -t wayland --arch loongarch64
```

成功输出会包含两个标记：

```text
WAYLAND_TEST_RESULT PASSED
WAYLAND_TEST_PASSED
```

app 的 prebuild 步骤会先在宿主机下载较大的 `llvm21-libs` APK 依赖闭包，并把这些
APK 注入 rootfs overlay。客体内脚本是 [`wayland-test.sh`](wayland-test.sh)。它会
先安装预取的 APK，然后通过带超时和 HTTP 镜像 fallback 的 Alpine apk 安装
`weston`、`weston-backend-drm` 和 `weston-shell-desktop`，检查 `/dev/dri/card0`
是否存在，检查 `/dev/input/event*`，用 DRM/pixman 后端启动 Weston，等待
`/tmp/wayland-*` socket 出现，在 `weston-info` 可用时连接一个客户端，扫描 Weston
日志中的明显启动错误，然后干净关闭合成器。

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
直接启动的 riscv64 和 x86_64 流程进入客体后的 Weston 和 GTK 命令完全相同，只有
宿主机侧的 QEMU 启动命令不同。aarch64 请使用后文 aarch64 说明中的辅助脚本。

### 第一步：构建内核和已经 provision 的 rootfs

先对要复现的架构运行一次自动化测试。该步骤会生成后续直接启动 QEMU 所需的内核镜像
和 rootfs 镜像，并把宿主机预取的 Weston 依赖闭包安装进该 rootfs，所以手动会话
不需要客体内网络。

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
  -drive id=disk0,if=none,format=raw,file=tmp/wayland-manual/riscv64.img
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

### 第五步：确认用户态包

如果第一步已经成功完成，复制出的 rootfs 已经包含 Weston 及其运行时依赖。在
`root@starry:` 提示符下执行：

```sh
command -v weston
ls /usr/lib/libweston-*/drm-backend.so
ls /usr/lib/weston/desktop-shell.so
```

只有在手动镜像里还没有 `gtk4-demo` 时才安装：

```sh
if ! command -v gtk4-demo >/dev/null 2>&1; then
apk_branch="$(sed -n 's#.*/\(v[0-9][0-9.]*\)/main#\1#p' /etc/apk/repositories | head -1)"
[ -n "$apk_branch" ] || apk_branch=v3.22

for mirror in \
  http://mirrors.huaweicloud.com/alpine \
  http://dl-cdn.alpinelinux.org/alpine \
  http://mirrors.aliyun.com/alpine \
  http://mirrors.tuna.tsinghua.edu.cn/alpine \
  http://mirrors.cernet.edu.cn/alpine
do
  printf '%s/%s/main\n%s/%s/community\n' \
    "$mirror" "$apk_branch" "$mirror" "$apk_branch" >/etc/apk/repositories
  apk add --no-cache weston weston-backend-drm weston-shell-desktop gtk4.0-demo font-dejavu && break
done
fi
```

这段安装循环只用于手动 rootfs 尚未包含 GTK demo 包、且内核/QEMU 启动方式提供可用
客体网络的情况。标准 Wayland app build 刻意走离线流程，不挂载 virtio-net 设备。
循环会安装 Weston、DRM 后端插件、desktop shell 插件、GTK4、Mesa、libdrm、
libinput、可用的 GTK 字体以及相关运行时依赖。
这里故意使用 HTTP 镜像，以避开客体 RTC 初始日期不正确时触发的 TLS 证书信任问题。
如果某个镜像卡住或下载到截断的包，重新执行这段循环即可继续尝试下一个镜像。

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
export GDK_BACKEND=wayland
export GSK_RENDERER=cairo
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

自动化 Wayland app 测试不需要客体内网络：较大的 APK 依赖闭包会先在宿主机预取，
再注入 rootfs overlay。使用这个离线流程时，aarch64 app 测试和其他架构一样通过
`cargo xtask starry app qemu -t wayland --arch aarch64` 运行。

Cocoa/VNC 辅助脚本保留在 [`run-hvf.sh`](run-hvf.sh)。它同样使用宿主机预取
APK 的方式准备 Weston 和 `gtk4-demo`，会扩展手动 rootfs，首次运行时离线
provision，之后复用已经 provision 好的镜像：

```bash
./apps/starry/wayland/run-hvf.sh --no-build --provision-only
STARRY_VNC=9 ./apps/starry/wayland/run-hvf.sh --no-build --vnc-only
```

使用 `--reprovision` 可以丢弃并重新创建
`tmp/axbuild/rootfs/rootfs-aarch64-wayland.img`。如果默认 4096 MiB 的手动镜像
不合适，可以设置 `STARRY_WAYLAND_ROOTFS_MB`。辅助脚本需要宿主机提供 `debugfs`、
`e2fsck`、`resize2fs`、`python3` 和 `qemu-system-aarch64`；在 macOS Homebrew
环境下，脚本会自动加入常见 Homebrew 路径。
辅助脚本的常规路径不会挂载客体 virtio-net 设备，因为 Wayland build config
刻意走离线流程，也没有启用网络驱动。
需要在终端里操作串口、同时通过 VNC 查看图形界面时，使用 `--vnc-only`，避免
Cocoa 抢占终端焦点。

## 内核侧依赖

本应用需要：

- 支持 dumb buffer 分配的 DRM `/dev/dri/card0`。
- QEMU 配置中启用 virtio GPU、keyboard、mouse 和 block 设备。
- 支持 libinput 枚举的 evdev `/dev/input/event*`。
- Wayland SHM 所需的 `memfd_create` 和 Unix socket fd 传递。
- 合成器事件循环使用的 `eventfd`。
- libinput 设备发现所需的 `/run/udev/data/` udev seed。
- app build config 中启用 `starry-kernel/input` 和 `ax-feat/display`。

如果复制出的 rootfs 中还没有这些用户态包，可选的手动安装包流程还需要启用了可用
客体网络的内核/QEMU 启动方式。
