# Venus、VirGL 资源回收与 EXECBUF 信号回归

在具有可读写 `/dev/dri/renderD128`、`/dev/dri/card0`、KVM 及支持 Venus 的
QEMU/virglrenderer 的 x86_64 主机上运行。QEMU 必须为 11 或更新版本；配置显式开启
`kvm,honor-guest-pat=on`，Intel xe 的 host-visible 映射还要求宿主 Linux 6.16+。
这是 [Mesa Venus 的平台要求](https://docs.mesa3d.org/drivers/venus.html)，不是关闭
fence-feedback 的替代方案。

宿主构建工具还需要 `x86_64-linux-musl-gcc` 和 LLVM 链接器 **`ld.lld`**，二者均须在
`PATH` 中。应用预构建固定使用 `-fuse-ld=lld`，不能仅以已安装 GCC 或 Clang 代替。
Ubuntu/Debian 可通过 `sudo apt-get install lld` 安装，Fedora 使用 `sudo dnf install lld`。
这不要求替换系统默认链接器。预构建会在镜像解包、下载前检查 `ld.lld`，缺少时直接
给出安装提示并失败。

从普通 CI 基础镜像准备图形容器时，除补齐 Mesa/Venus、virglrenderer、QEMU 图形
依赖外，也须额外安装 `lld`；普通无图形 CI 不需要因此启用 Venus。

运行：

```sh
cargo xtask starry app qemu -t venus-regression --arch x86_64 --cap virgl
```

这是有界自动回归，不启动桌面或无限运行。客户机只选择 Mesa virtio Vulkan ICD，
不允许切换到客户机内的软件 Vulkan ICD；同一 VirGL fd 反复创建、映射和关闭 GEM，
并验证最后一个 handle
关闭后 VMA 仍有效。随后在同一 Vulkan device 上反复分配 host-visible 内存，提交
真实 `vkCmdFillBuffer`，等待 fence 并逐字回读，再释放资源和映射。失败必须传递到
最外层运行器。普通不具备主机 GPU 的 CI 不声明 `virgl` 能力，不执行此应用。

GEM 回收之后、Vulkan 回读之前，应用还会用活动 CRTC 上带 `OUT_FENCE_PTR` 的
NONBLOCK page flip 作为未完成输入 fence，验证真实 `EXECBUF` 的阻塞等待：
`SIGUSR1` 到达时，没有 `SA_RESTART` 的 ioctl 返回 `-1/EINTR`；带 `SA_RESTART`
的 ioctl 在两次信号分别打断等待后由内核重启，并在 fence 完成后成功返回，用户态
不手动重试。
输入 syncobj 路径使用 `VIRTGPU_EXECBUF_SYNCOBJ_RESET`，成功后 syncobj 被清空，
证明命令只提交一次。每个用例都通过 pipe 就绪、`/proc/<pid>/stat` 睡眠状态和有界
超时区分“信号先于 ioctl”“ioctl 已完成”和“真正打断阻塞等待”。

宿主可通过标准的 `VK_DRIVER_FILES` 显式选择 Lavapipe 后端。此时客户机仍运行
Venus，设备名可能包含 `Virtio-GPU Venus (llvmpipe ...)`；这是宿主软件实现，不是
客户机绕过 VirtIO 的回退，也不能作为物理 GPU 已通过验证的证据。回归不关闭 Mesa
的 fence-feedback 路径。

这些结果证明 QEMU 和当前主机渲染栈上的命令、映射与回收路径，不代表其他物理
GPU 或显示器的扫描输出已验证。

CI 边界：仓库 `container/Dockerfile` 已选用 QEMU 11.1.1，但普通容器不声明 `virgl`，
也未构建图形渲染依赖，因此普通 `starry app qemu --all` 跳过此应用，不构成 Venus
已验证。图形运行器必须提供 KVM、宿主渲染栈并显式传入 `--cap virgl`；版本不满足时
预构建直接失败，不降级到旧 QEMU 参数或关闭反馈。普通四架构 2D/system 门禁不变。

当前 LTP `ltp-syscalls` 用例集没有 DRM/VirtGPU EXECBUF 覆盖，普通 2D system QEMU 的
`supports_3d` 为 false，无法进入这条等待路径；因此用例保留在现有唯一声明 `virgl`
能力的应用入口，没有新增不可达的 LTP 或 system 子用例。
