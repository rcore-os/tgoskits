# 旧 host-performance 场景的历史记录

以下记录来自旧stock Linux 6.1.99＋host-performance场景，仅作追溯；不能作为本目录新virtual-cpufreq场景的验收结果。当前使用方法见README.md。

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
