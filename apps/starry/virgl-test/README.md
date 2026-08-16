# virgl-test 3D 加速集成测试！

在 StarryOS 上运行 Weston (DRM backend + GL 渲染器) 作为 Wayland compositor，
通过 Mesa 的 virgl (gallium) 驱动走 virtio-gpu 3D 命令到达 QEMU host 的
virglrenderer，验证从内核 DRM 到 Mesa 用户态再到 virgl 硬件加速渲染的全链路连通性。

与 ffplay 的 llvmpipe 软渲染不同，virgl-test 的目标是确认真正走 3D 硬件加速
路径：`GL_RENDERER` 必须报告 `virgl`，而不是回退到 `llvmpipe`/`softpipe` 软渲染。

## 当前状态

| 组件 | 状态 | 备注 |
|---|---|---|
| Weston DRM + GL | ✅ 正常 | `--renderer=gl` 路径，走 renderD128 |
| Mesa virgl 驱动 | ✅ 正常 | `GL_RENDERER = virgl`，经 virtio-gpu 3D 命令 |
| virtio-gpu 内核 3D | ✅ 正常 | GETPARAM / CONTEXT_INIT / GET_CAPS / EXECBUFFER / blob |
| PRIME dma-buf 导出/导入 | ✅ 正常 | import 即 attach 到 importer 的 vrend context |
| glmark2 | ✅ 正常 | Score 69，0 个命令提交错误 |
| DRM render node | ✅ 正常 | `renderD128`（复用 card0，sysfs 补 MODALIAS + drm 子目录） |

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

## 测试流程

1. 启动 seatd / dbus
2. 启动 Weston（`drm-backend.so`，`--renderer=gl`，`/root/.config/weston.ini`）
3. 等待 Wayland socket 就绪（最多 15 秒）
4. 检查 `/dev/dri/`、`renderD128` 的 sysfs vendor/device/uevent/subsystem
5. 检查 `virtio_gpu_dri.so` 存在
6. 运行 `weston-simple-egl` 与 `glmark2-es2-wayland`（600 秒超时）做渲染验证与跑分
7. 测试完成，VM 保持运行，可手动操作，`poweroff` 退出

## 构建与运行

```bash
cargo xtask starry app qemu -t virgl-test --arch x86_64
```

这会依次：

- 构建 StarryOS 内核（含 DRM display + virtio-gpu 3D + PRIME dma-buf 支持）
- 运行 `prebuild.sh` 构建 rootfs overlay（rootfs 扩容到 5120M，用 qemu-user-static
  安装 Alpine 包、全量升级以启用 virgl、拷贝 Mesa/GL 运行时库、注入 weston.ini
  与 runner.sh）
- 启动 QEMU（`virtio-vga-gl` + egl-headless + spice，1 核 2G，KVM，UEFI 启动）
- 等待测试结果（`timeout = 900`，VM 最长运行 900 秒）

`fail_regex` 只匹配 `panic`；`timeout = 900` 表示 VM 900 秒后自动退出（超时保护），
测试期间观察 `virgl-test 完成` 横幅 + 画面，也可手动 `poweroff` 提前退出。

## 查看画面

QEMU 使用 SPICE 输出，连接查看：

```bash
spicy --uri="spice+unix:///tmp/starry-virgl-test.sock"
```

## 依赖的 Alpine 包

| 包 | 用途 |
|---|---|
| weston + weston-backend-drm + weston-shell-desktop | Wayland compositor + DRM 后端 |
| weston-terminal + foot | 桌面终端（weston-shell-desktop 需要） |
| mesa-dri-gallium | virgl 驱动（`pipe_virgl.so`，v3.23+ 才有） |
| mesa-egl + mesa-gbm + mesa-gles | Mesa GL / EGL / GBM 库 |
| mesa-demos + mesa-dev | `es2_info`、`eglinfo`、`drm_info` 等测试工具 |
| glmark2 | GL 基准测试（edge/testing 仓库） |
| seatd + dbus | 图形服务依赖 |
| font-noto | 文字渲染 |
