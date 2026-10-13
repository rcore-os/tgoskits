# AxVisor Linux perf30 profile

这个用例在 OrangePi 5 Plus 上运行当前 Linux 6.1.99 客户机的真实 USB 摄像头和
RKNN/NPU 流水线。它单独建立高性能 profile，不改变 `robot-linux/smoke` 的普通
28 FPS 回归入口。

## 1. 配置边界

本节说明 perf30 用例的 CPU、宿主 governor、客户机 devfreq 和 benchmark 资产边界。
这些约束由对应的 TOML 与 board shell step 实现，普通 `robot-linux/smoke` 入口仍保持
独立的默认 governor 和 28 FPS 回归门槛。

- `linux-smp1-emmc-perf30.toml` 将一个 Linux vCPU 显式固定到宿主 A76 CPU4：
  `phys_cpu_ids = [0x400]` 是客户机可见 MPIDR，`phys_cpu_sets = [0x10]` 才是宿主
  CPU 位图。`linux-smp1-emmc-perf30-cpu5.toml` 保留 CPU5 的 `[0x500]`/`[0x20]`
  A/B 模板；它没有接入默认 board case，使用前应复制对应 build/board 配置并单独记录
  资产和结果。现有 `robot-linux/linux-smp1-emmc.toml` 是 CPU0 基线。
- `board-orangepi-5-plus-robot-linux-perf30.toml` 的 `cmdline` 是宿主 U-Boot
  bootargs，增加 `cpufreq.default_governor=performance`。这不是客户机 kernel
  cmdline；普通 board 的默认 governor 不受影响。
- board shell step 只处理 Linux 客户机能够看到且名称匹配 `rknpu`/`npu`/`dmc` 的
  `devfreq` 节点，确认 `performance` governor 存在并读回成功。缺少 NPU 或写入
  失败会 fail-closed；DMC 节点按板卡固件能力记录。当前 OrangePi 的 ATF 日志为
  `trusted firmware unsupported`，客户机没有 DMC devfreq，因此会明确输出
  `DMC_DEVFREQ_UNAVAILABLE_FIRMWARE`，不会把 DMC 缺失伪装成已启用。
- 同一步还打印客户机 CPUFreq 前端状态：生产 6.1.99 镜像应明确输出
  `CPUFREQ_VIRT_UNAVAILABLE_HOST_PERF`，表示性能来自宿主 profile；带
  `virt-cpufreq` backport 的镜像则打印实际 driver/governor，不能把缺少客户机
  前端误报为客户机调频已启用。
- 运行真实脚本的最低门槛为每次 `run_vision_usb_ci_once.sh 29.5` 的最终 PASS；
  29.5 是约 30 FPS 的门槛，原有 smoke 的 28.0 门槛继续独立保留。
- 同一 profile 随后运行 `/rknn_yolov8_image/rknn_yolov8_bench` 60 秒，要求单行
  `UVC_RKNN_BENCH_RESULT` 的 `capture_fps >= 29.5`、`decode_errors=0` 和
  `inference_errors=0`；长测步骤与视觉脚本步骤分开，避免前一个成功标记提前结束验收。
- 长测依赖独立部署的 `/rknn_yolov8_image` 资产，不由 AxVisor VM 镜像自动提供。用
  `benchmarks/starry/orangepi-5-plus-uvc-rknn/build-image-runner.sh` 构建后，将
  `rknn-yolov8-image/install/rk3588_linux_aarch64/rknn_yolov8_image/` 同步到板卡
  `/rknn_yolov8_image/`。profile 会先检查 benchmark、RKNN/RGA runtime、模型和标签，
  缺失时输出 `RKNN_BENCH_ASSETS=FAIL` 并停止；通过时打印这些文件的 `sha256sum`，
  便于把实际板卡资产纳入验收记录。

该 profile 使用 `/guest/linux/orangepi-5-plus-6.1.99`，因此不假设客户机已经带
`virt-cpufreq` 驱动。后续 virtual-cpufreq 客户机资产可以只替换 VM 的
`kernel_path`，沿用同一块板卡验收协议。

## 2. 板卡入口

从仓库根目录运行：

```bash
cargo xtask axvisor test board \
  --board orangepi-5-plus-robot-linux-perf30
```

运行前仍需按当前 robot-linux smoke 约定把 Linux kernel、`initramfs.cpio`、模型和
`aka-rk3588-virtual` 运行包放到板卡的 `/guest` 或客户机根文件系统中，并按上面的
benchmark 资产合同准备 `/rknn_yolov8_image`。

## 3. 当前验证边界

本次 PR 的实体板记录只覆盖下面的 `robot-linux-perf30` 入口。双客户机目录中同步修改的
`dual-linux-zephyr/linux-smp1-sd.toml` 与 `dual-starry-zephyr/zephyr-smp1.toml` 已完成编译
检查，但没有在本次硬件窗口按新的 CPU4/CPU2 绑定重新运行；它们不属于当前 CI 板卡矩阵，
现有双客户机历史记录不能替代这次绑定变更的实机证据。后续运行应单独保存 session、
亲和性、温度和完整 IVC/执行器结果。

实体 OrangePi 验证记录（2026-10-12，`OrangePi-5-Plus-1`，board-runner
session `fda87432-169a-4b24-b736-6cfb2fbb229a`）如下：

- 宿主 bootargs 包含 `cpufreq.default_governor=performance`，Linux 客户机启动为
  MPIDR `0x400`，宿主亲和性为 CPU4（`0x10`）。
- Linux 可见 `fdab0000.npu` 为 `performance`，`cur_freq=max_freq=1000000000`；
  DMC 计数为 0，原因已由 ATF/客户机 dmesg 记录。
- 两个约 10 秒窗口为 `29.95`、`29.99 FPS`，汇总 `29.97 FPS`，JPEG、推理、
  FT232 回环和暂停恢复均为 0 错误或 PASS（回环 `401/401`）。
- 60 秒 `rknn_yolov8_bench` 输出 `duration_sec=60.1`、`captured=1797`、
  `capture_fps=29.90`、`inferences=816`、`infer_fps=13.58`、
  `decode_errors=0`、`inference_errors=0`；profile 的 `run_ms_avg=21.02`。
- 本次板卡上的 benchmark 资产 SHA256 为：benchmark
  `b86b867b032e05995f6f527be9558705dfee73945a356f5abb422bf1490c04fa`，
  `librknnrt.so` `d31fc19c85b85f6091b2bd0f6af9d962d5264a4e410bfb536402ec92bac738e8`，
  `librga.so` `d0f5ae5c18c4c54ccee1d697b60f94507c5827389bb8fe9d4b0a4cb39cdc5972`，
  模型 `40d62454f08c9a73ccd2349ee8ccadfa51e9a6f93c48e975b3302e29aff1a4d0`，
  标签 `bd17f1ee35d5f3c862a4894605855abbb9dda4b0621fdb0ac4c2c8c7bb7e730a`。

完整原始日志保留在本次 board runner 输出；后续实体板运行仍需保存宿主启动参数、
vCPU 亲和性、NPU/DMC governor、温度和每个 FPS 窗口的原始值。`measure/` 入口用于
在当前固件缺少 DMC 时重复同一 CPU4 + host-performance 测量，不替代带 DMC 的能力门禁。
