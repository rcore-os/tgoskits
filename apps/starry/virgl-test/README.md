# virgl-test 3D 加速人工验收

在 StarryOS 上运行 Weston (DRM backend + GL 渲染器) 作为 Wayland compositor，
通过 Mesa 的 virgl (gallium) 驱动走 virtio-gpu 3D 命令到达 QEMU host 的
virglrenderer，验证从内核 DRM 到 Mesa 用户态再到 virgl 硬件加速渲染的全链路连通性。

与 ffplay 的 llvmpipe 软渲染不同，virgl-test 的目标是确认真正走 3D 硬件加速
路径：`GL_RENDERER` 必须报告 `virgl`，而不是回退到 `llvmpipe`/`softpipe` 软渲染。

这个应用不是自动判定用例：guest 出现 root 交互登录 shell 时，由
`/etc/profile.d/virgl-autostart.sh` 在后台拉起 `/usr/local/bin/virgl-runner.sh`。
Weston 和一个 `weston-terminal` 窗口持续显示，glmark2 可选地用 strace 记录一次
（内核不支持跟踪时直接运行），之后 runner 一直休眠，由操作者通过本机
Web(VNC/noVNC)、SPICE 或串口画面人工检查，
检查完自己关闭 QEMU。它不会因为成功断言、失败断言、超时、串口输出或运行时间
自动结束，也不会打印 `VIRGL_TEST_PASSED` / `VIRGL_TEST_FAILED` 之类的机器判定标记。

诊断主证据是 `GL_RENDERER=virgl`、Weston 日志和 VNC/SPICE 可见画面；`strace`
只是**可选**诊断，StarryOS 内核跟踪能力不完整时会自动退化为直接运行 glmark2，
此时不会产生跟踪轨迹。

## 人工验收清单

| 组件 | 人工检查什么 | 看哪里 |
|---|---|---|
| Weston DRM + GL | Weston 起来了，日志里创建了 Wayland socket | 串口、`/tmp/weston.log` |
| Mesa virgl 驱动 | glmark2/es2_info 的 `GL_RENDERER` 行含 `virgl`，不是 `llvmpipe`/`softpipe` | 串口、`/tmp/glmark2.log` |
| virtio-gpu 内核 3D | context、resource、submit 与 PRIME 路径没有持续报错 | `/tmp/weston.log`、`/tmp/glmark2.log` |
| 可见窗口 | VNC/SPICE 画面里有一个 `weston-terminal` 窗口 | Web/noVNC 画面 |
| glmark2 | 能跑完并给出 score（只是参考数字；个别场景可能失败，见“已知场景级限制”） | 串口、`/tmp/virgl-runner.log`、`/tmp/glmark2.log` |
| ioctl 轨迹（可选） | 内核支持跟踪时 strace 记录了 glmark2 的 `ioctl/mmap/munmap/openat/close`；不支持则无轨迹 | `/tmp/glmark2.strace*`、`/tmp/glmark2.strace-attempt.log` |

**结论口径**：VNC 画面只是"肉眼可见"的可视证据；判定是否真的走 virgl 硬件加速
要看 `GL_RENDERER=virgl` 以及日志里没有 GPU 提交错误。画面正常但 `GL_RENDERER`
是软渲染，就不算走通 virgl 路径。

## 已知场景级限制：glmark2 `refract`

最终人工验收整体走通，但观察到 glmark2 有一个**场景级**失败，它不代表整条
virgl 链路失败，也不代表所有 glmark2 场景通过：

- 默认场景集 `glmark2-es2-wayland` 能跑完，报告 `GL_RENDERER: virgl`；
  本次 QEMU 人工验收记录到 `glmark2 Score: 43`，VNC（原始 RFB）可见 3D 动画并已录屏。
  分数仅为本次环境的观测值，不作为通过条件。
- 单独运行 `glmark2-es2-wayland -b refract` 时，`refract` 场景在
  `DistanceRenderTarget::setup` 处报 `glCheckFramebufferStatus failed (0x8cdd)`
  与 `Set up failed`；`0x8cdd` 是 `GL_FRAMEBUFFER_UNSUPPORTED`。
- 对照：`glmark2-es2-wayland -b shadow` 通过，`Score 33`；去掉
  `MESA_GL_VERSION_OVERRIDE` / `MESA_GLSL_VERSION_OVERRIDE` 后再跑 `refract`
  仍然失败。所以这不是简单的“深度纹理全部不支持”，也不是 version override 造成的。
- 根因尚未确认：具体是哪个 attachment/format 组合触发
  `GL_FRAMEBUFFER_UNSUPPORTED` 仍缺少 caps/FBO 级证据（需要现场抓取 framebuffer
  完整性检查或 virgl caps 才能定位）。本次验收不改驱动去伪造能力，也不宣称所有
  glmark2 场景都通过。
- 之前出现过的 errno 22 / `EGL_BAD_ALLOC` / segfault 在本次验收中未再出现。

该场景失败只是给人看的诊断：它不会结束 runner 或 QEMU，也不改变“由操作者人工
关闭 VM”的口径——无论 glmark2 成功、失败还是没有输出 renderer，runner 都继续
休眠等待人工关闭。

## 内核需求

- `/dev/dri/card0` + `/dev/dri/renderD128` — DRM 设备（render node 与显示复用）
- virtio-gpu 3D ioctl：GETPARAM、CONTEXT_INIT（capset_id）、GET_CAPS（max_size）、
  RESOURCE_CREATE_3D、EXECBUFFER（64B struct）、RESOURCE_CREATE_BLOB、PRIME
- `/dev/fb0` — framebuffer 设备
- sysfs 设备枚举：`MODALIAS=platform:{driver}` + `drm/{card0,renderD128}`
  子目录（libdrm 的 `drmNodeIsDRM`/`drmParseOFDeviceInfo` 依赖）
- AF_UNIX SCM_RIGHTS 文件描述符传递
- memfd_create + seal 支持

## 关于 virgl 3D 加速

QEMU 使用 `virtio-vga-gl` + `egl-headless,gl=on` + `spice gl=off` 启动。
默认 `blob=off`：对齐 alpine-virgl-vm，走经典路径（RESOURCE_CREATE_3D +
PRIME 导出 GEM）。Mesa 是否用 blob 取决于 GETPARAM 报告的 `RESOURCE_BLOB`
（card0 按真实协商返回，blob=off 时为 0 → Mesa 自动走经典路径）。

guest 内 Mesa virgl 驱动把 GL 调用编码成 virtio-gpu 3D 命令，经
`SUBMIT_3D` 提交给 QEMU host 的 virglrenderer 做真正的 GPU 渲染。因此
`GL_RENDERER=virgl` 是硬件加速生效的直接证据。

## 宿主机 GPU 建议

host 侧 virglrenderer 依赖宿主机 GPU 做真正的 3D 渲染。**建议使用 AMD GPU**
（mesa radeonsi 驱动），virglrenderer 对其支持最完善；**NVIDIA GPU 可能存在问题**
（闭源驱动 + 私有 GLX 路径与 virglrenderer 的交互不一致，可能导致命令被拒或
渲染错误）。

## 登录自启与测试流程

1. root 交互登录 → `/etc/profile.d/virgl-autostart.sh` 在后台幂等启动 runner
   （只启动一次，输出只进 `/tmp/virgl-runner.log`，不阻塞、不夺走交互 shell）
2. runner 启动 Weston（`drm-backend.so`，`--renderer=gl`，`/root/.config/weston.ini`）
3. 等待 Wayland socket 就绪（最多 15 秒）
4. 检查 `/dev/dri/`、`renderD128` 的 sysfs vendor/device/uevent/subsystem
5. 检查 `virtio_gpu_dri.so` 存在，并打印 es2_info / eglinfo 的 GL/EGL 信息
6. 用 `weston-terminal --maximized` 打开一个人工可见窗口
7. 可选：先用 `strace -ff -tt -yy -s 128 -e trace=ioctl,mmap,munmap,openat,close`
   运行 `glmark2`。若内核跟踪能力不完整（例如只打印
   `strace: exec: Function not implemented`）或输出里没有 `GL_RENDERER`，runner
   会记录该诊断并在同一次流程中**直接运行** `glmark2`（无 `timeout`，由程序自然
   结束或由人关闭 VM），保证拿到真实 `GL_RENDERER`/score 和可见动画。
   跟踪可用时轨迹写入 `/tmp/glmark2.strace*`；不可用时不会有轨迹。
8. 打印人工验收提示，然后一直休眠等待操作者关闭 QEMU

关键准备步骤（Weston、`/dev/dri`、`virtio_gpu_dri.so`、weston-terminal）失败时，
脚本会打印 `[virgl-test][BLOCKER]` 说明和对应日志尾部，但**不会**关闭 QEMU——
VM 继续运行，方便操作者连上去继续排查。

登录脚本不依赖 OpenRC：guest 镜像里没有 `rc-service`，`/etc/local.d/*.start`
也不会被执行，所以自启挂在 Alpine 的 `/etc/profile`（它 source `/etc/profile.d/*.sh`）
上。所有运行时依赖（含可选的 `strace`）都在 prebuild 阶段一次性打进 rootfs，guest 内
**不做运行时 `apk add`**：rootfs 未保留 APK 数据库，运行时安装会错误卸载图形栈。

## 构建与运行

```bash
cargo xtask starry app qemu -t virgl-test --arch x86_64
```

这会依次：

- 构建 StarryOS 内核（含 DRM display + virtio-gpu 3D + PRIME dma-buf 支持）
- 运行 `prebuild.sh` 构建 rootfs overlay（rootfs 扩容到 5120M，用 qemu-user-static
  安装 Alpine 包——含可选的 `strace`——并全量升级以启用 virgl、拷贝 Mesa/GL 运行时库、
  注入 weston.ini 与 runner.sh，写入 `/etc/profile.d/virgl-autostart.sh` 登录自启脚本）
- 启动 QEMU（`virtio-vga-gl` + egl-headless + SPICE，额外开 VNC 5909 和 QMP
  socket，1 核 2G，KVM，UEFI 启动，`-snapshot` 不落盘）
- guest 登录后自动在后台启动 Weston、weston-terminal 与 glmark2，命令本身一直挂着，
  不会因为成功、失败、超时或串口输出自动结束

`qemu-x86_64.toml` 里没有任何会因串口输出或时间到期结束 QEMU 的配置：
没有 `shell_prefix` / `shell_init_cmd` / `success_regex`，也没有
`[[shell_check_steps]]`；也没有有效的非零超时——`timeout = 0` 是为满足 ostool
配置解析而保留的键，取值 0 表示显式禁用超时（不会到期结束 QEMU）。
`fail_regex = []` 同样是 ostool 配置解析要求的键，取空列表表示不做任何失败匹配
（内核 panic 也只打印、不终止 VM）。
QEMU 生命周期只由 guest `poweroff`、QMP `quit` 或操作者中断决定。

## 查看画面（本机 Web）

QEMU 额外提供一个只监听 localhost 的 VNC 输出（display 9，TCP 5909）。这个端口
是 **raw VNC/RFB 协议，不是 HTTP**：它不提供 `vnc.html`，浏览器打不开
`http://127.0.0.1:5909/...` 这类地址。

两种正确用法：

1. 普通 VNC 客户端直连：

   ```bash
   vncviewer 127.0.0.1:5909
   ```

2. 浏览器需要 noVNC/websockify 做代理：先用 noVNC 自带的 `novnc_proxy`
   （或等价的 `websockify` 命令）把本机 Web 端口（建议 6080）转发到
   `127.0.0.1:5909`：

   ```bash
   novnc_proxy --listen 127.0.0.1:6080 --vnc 127.0.0.1:5909
   ```

   然后浏览器打开：

   ```text
   http://127.0.0.1:6080/vnc.html?autoconnect=1
   ```

noVNC/websockify 需要在宿主机预先安装，本仓库不下载这些工具。

SPICE 仍可用作备选查看通道：

```bash
spicy --uri="spice+unix:///tmp/starry-virgl-test.sock"
```

两种画面都只是可视证据：VNC/SPICE 的显示拷贝路径与 guest 的 virgl 3D 命令链路
无关，画面正常不代表 Mesa 走了 virgl。真正说明路径是否走通的是串口/日志里的
`GL_RENDERER=virgl` 和没有 GPU 提交错误。

## 串口与日志

串口控制台仍然可以交互：登录脚本和 runner 都**不**向 `/dev/console` 写第二份
输出，runner 的 stdout/stderr 只进 `/tmp/virgl-runner.log`，所以串口不会被
重复的 runner 日志刷屏。要用串口直接看 runner 输出时读文件即可：

```sh
tail -f /tmp/virgl-runner.log
```

相关文件与路径：

| 文件 | 内容 |
|---|---|
| `/etc/profile.d/virgl-autostart.sh` | root 登录时幂等后台启动 runner 的登录脚本 |
| `/usr/local/bin/virgl-runner.sh` | 实际执行 Weston / weston-terminal / glmark2 的验收脚本 |
| `/tmp/virgl-runner.log` | runner 全部输出（stdout/stderr，唯一去处） |
| `/tmp/virgl-runner.pid` | runner 进程 PID（用于幂等判断与 stale PID 清理） |
| `/tmp/virgl-autostart.lock` | 登录自启的 `mkdir` 原子锁（启动期间短暂存在） |
| `/tmp/weston.log` | Weston 启动与 DRM/EGL/GBM 日志 |
| `/tmp/glmark2.log` | glmark2 输出、`GL_RENDERER`、score、错误扫描 |
| `/tmp/glmark2.strace*` | （可选）`strace -ff` 按线程拆分的 ioctl/mmap/munmap/openat/close 轨迹，仅跟踪可用时存在 |
| `/tmp/glmark2.strace-attempt.log` | strace 尝试运行的输出（含 `strace: exec: Function not implemented` 之类的自述错误） |
| `/tmp/weston-terminal.log` | 可见窗口 weston-terminal 的输出 |

## 关闭 QEMU

没有任何自动验收终止条件：不看 runner 是否成功、不匹配任何串口输出、也没有
超时。关闭只能由人工发起，三种方式任选一种：

```bash
# 1) guest 内主动关机
poweroff

# 2) 宿主机通过 QMP 让 QEMU 退出
#    QMP 是有状态协议：客户端必须先读取 greeting，再发送 qmp_capabilities
#    完成协商，之后才能发送 quit。
python3 - <<'PY'
import json, socket

sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.connect("/tmp/starry-virgl-test.qmp")
stream = sock.makefile("rwb")

def recv_message():
    while True:
        line = stream.readline()
        if not line:
            raise RuntimeError("QMP connection closed")
        msg = json.loads(line)
        if "event" in msg:          # 跳过异步事件，只等命令响应
            continue
        return msg

def send(command):
    stream.write((json.dumps(command) + "\n").encode())
    stream.flush()
    return recv_message()

greeting = recv_message()                      # 1. 读取 greeting
print("greeting:", greeting.get("QMP", {}))
print("capabilities:", send({"execute": "qmp_capabilities"}))  # 2. 协商
print("quit:", send({"execute": "quit"}))                      # 3. 退出
PY

#    若只想用 socat，也必须连续发送 qmp_capabilities 与 quit（socat 会依次
#    收 greeting、发这两条命令），不能只发 quit：
printf '{"execute":"qmp_capabilities"}\n{"execute":"quit"}\n' | \
    socat - UNIX-CONNECT:/tmp/starry-virgl-test.qmp

# 3) 直接 Ctrl-C 结束 cargo xtask 进程
```

QMP 监听 Unix socket `/tmp/starry-virgl-test.qmp`，只对本机可见；退出后 socket
文件可能残留，属正常现象。

## 依赖的 Alpine 包

| 包 | 用途 |
|---|---|
| weston + weston-backend-drm + weston-shell-desktop | Wayland compositor + DRM 后端 |
| weston-terminal + foot | 人工可见窗口（weston-shell-desktop 使用） |
| mesa-dri-gallium | virgl 驱动（`pipe_virgl.so`，v3.23+ 才有） |
| mesa-egl + mesa-gbm + mesa-gles | Mesa GL / EGL / GBM 库 |
| mesa-demos + mesa-dev | `es2_info`、`eglinfo`、`drm_info` 等测试工具 |
| glmark2 | GL 基准测试（edge/testing 仓库） |
| strace | 可选的跟踪诊断（ioctl/mmap/munmap/openat/close，构建期打进 rootfs）；内核不支持跟踪时自动退化 |
| seatd + dbus | 图形服务依赖 |
| font-noto | 文字渲染 |
