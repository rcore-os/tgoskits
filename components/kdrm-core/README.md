# DRM Core

这是 TGOSKits 图形栈共享的 DRM core。它将 Linux 7.2.3 中通用的 DRM
设备和文件对象、ioctl、GEM、PRIME、KMS/atomic、fence/syncobj，以及 VirtIO
显示后端所需的通用显示辅助代码作为无标准库 Rust 组件提供。

本组件不拥有硬件设备。底层驱动仍由 RDIF/axgpu 一侧发现和持有，通过
DRM 回调表接入；Starry 的设备节点、文件描述符、用户地址空间和 poll
语义由操作系统适配层提供。通用对象与同步逻辑不依赖 VirtIO、Virgl 或
Intel。

## 1. 公共机制

当前接入范围包含设备与文件生命周期、ioctl 分发、GEM/共享内存对象、
PRIME 文件描述符导入导出、connector/CRTC/plane/property、atomic 检查和
提交、vblank 及 syncobj/fence 行为；同时保留 Linux 源模块名，便于维护者
沿上游实现核对语义。

### 1.1 目标布局

本组件面向 TGOSKits 支持的 64 位小端目标：x86_64、AArch64、RISC-V 64
和 LoongArch 64。硬件寄存器访问、设备探测、文件系统节点以及运行时服务
不在本组件内。

### 1.2 排除范围

以下能力不属于本次公共 DRM core：Intel 显示/GT 驱动、固件 framebuffer
设备、fbdev framebuffer-console 集成、DP bridge 与 MST 拓扑、debugfs、
panic/sysrq 集成，以及 writeback。对这些路径不得依赖静默成功或空实现；
未支持的接口应显式返回错误，或不对外导出。

## 2. 接入边界

`DrmDriver` 回调表连接公共机制与硬件能力，`DrmFile` 保持每次打开的句柄和事件状态。
运行时初始化、对象引用和失败处理的设计见 [共享 DRM 核心接入](../../docs/design/shared-drm.md)。

- kdrm-core：共享 DRM 对象、UAPI 处理、生命周期与同步逻辑。
- drm-kapi：列表、对象引用、锁、等待、内存、页缓存、fence 和文件服务等
  最小内核机制。
- RDIF 后端：设备与硬件资源所有权，并实现驱动回调。
- Starry 适配：ioctl 入口、fd/mmap/poll、用户内存访问和文件服务 provider。

DRM core 的 C-shaped 结构用于保留 Linux 对应关系，不表示本组件直接实现
Linux VFS。不得把操作系统文件描述符表、用户地址空间或驱动硬件状态藏入
共享 core 的全局变量。

本组件以 MIT 许可发布。Linux 源代码对应关系与版权声明见本目录的 NOTICE、
LICENSE-MIT 和各源文件头部。
