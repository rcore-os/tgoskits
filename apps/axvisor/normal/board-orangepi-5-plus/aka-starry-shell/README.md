# StarryOS 与统一 Zephyr shell 的 AKA 板测

本用例在 OrangePi-5-Plus-robot-emmc 上执行真实相机、NPU、车轮与机械臂的 AKA 联调。Zephyr 通过 `aka run` 启动控制程序；StarryOS 执行客户机 `/opt/aka/run_dual_pick_ci_once.sh --min-fps 0.01`。不用 `ivc wait`。

## 镜像和文件系统

StarryOS、Zephyr 和客户机 rootfs 由 tgosimages 构建，正式产物沿用原目录：

- `IMAGES/orangepi/starry/orangepi-5-plus`
- `IMAGES/orangepi/zephyr/orangepi-5-plus[.dtb]`
- `IMAGES/rootfs/rootfs-aarch64-orangepi-jammy.img`

板端 `/guest/ivc-test` 是临时部署位置；AKA 已安装在原客户机 ext4 系统的 `/opt/aka`，保留真实校准参数及原有系统内容。客户机通过 virtio-blk 自动选择唯一磁盘 `/dev/blk0`，eMMC/SD 控制器禁用，不使用 AKA cpio 或 `root=/dev/vda`。

StarryOS 的镜像构建配置在本目录 `starry-guest-build.toml`，由 tgosimages 的 `scripts/os/starry.sh --src-dir` 调用原始 TGOSKits checkout；原有本地修改保留。宿主 AxVisor 从原始 tgoskits-pr 通过 cargo xtask 构建运行。

StarryOS 使用生成设备树的 `stdout-path` 选择默认控制台，不硬编码 `console=ttyS2`，无需串口重定向。最终状态检查在确认 VM 控制台附着后发送，不等待已经输出过的 shell 提示符。

## CPU 和功能验收

StarryOS 使用一个 Cortex-A76 vCPU（物理 MPIDR `0x400`），Zephyr 使用 MPIDR `0x200`。

本用例只验收 AKA 完整流程，不评估吞吐性能。现有单轮脚本只接受正数 FPS 参数，因此采用 `--min-fps 0.01`，用于确认感知管线确实有数据产出。脚本仍输出两个感知窗口的统计信息，原来的 28 FPS 性能目标不再作为本测例门槛。

完整成功要求 31 项控制检查通过、工作负载运行至少 62000 ms、应用及检查脚本正常退出、清理完成、IVC dropped=0，最终输出 `AKA_SHELL_CI_PASS`。仅启动或握手成功不构成验收。

## 执行

应用位于 `apps/axvisor/normal/board-orangepi-5-plus/aka-starry-shell`。在原始 TGOSKits 仓库执行（运行器支持从 apps 目录发现）：

```sh
cargo xtask axvisor test board -g normal \
  -c board-orangepi-5-plus/aka-starry-shell \
  --server http://10.3.10.60:2999 -b OrangePi-5-Plus-robot-emmc
```

IVC 映射及关闭修复、宿主存储交接修复仅在 TGOSKits 中维护，分别位于 `virtualization/axvm/src/control/requests.rs`、`virtualization/axvm/src/runtime/ivc/manager.rs` 和 `virtualization/axvm/src/host/storage.rs`。逻辑关闭会保留仍可能 DMA 可见的共享内存；当前重跑用例采用板级复位，不能把逻辑关闭视为已安全物理回收。高风险核心改动合入前仍需领域审查。

带直通设备且仍持有 IVC 端点（含已逻辑关闭端点）的客户机，停止、重置和删除请求会在拆除运行资源前明确返回不支持，保持原运行状态；需要板级复位回收。停止 vCPU 或生命周期设备不能单独证明物理 DMA 已停稳。
