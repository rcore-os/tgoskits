# AArch64 Linux perf E2E

本应用用上游 Alpine `perf 6.19.14` 验证 StarryOS 的 Linux perf ABI。QEMU 和
OrangePi 5 Plus 共用同一组 workload、锁定依赖和验收脚本；板卡资产通过
`session_files` 上传到 `/tmp`，不会写入板卡的持久根文件系统。

`packages.lock` 固定 Alpine v3.23 `main/aarch64` 中 `perf-6.19.14-r0.apk` 及
完整运行时依赖闭包的 URL 和 SHA256。`rust/host-prebuild.sh` 下载并逐包校验，然后
生成临时 runtime archive。板卡 session 的单文件上限为 64 MiB，因此上传前把
同一 gzip 流确定性拆成两个分片，guest 在 `/tmp` 拼回后解包；仓库不保存 APK、
rootfs、runtime archive 或 `perf.data`。

QEMU TCG 只用于验证 `perf_event_open`、计数、overflow、mmap ring 以及
`perf stat/record/report` 控制流。其 cycles 是虚拟计时，cache、branch 和 stall
事件没有可靠的微架构含义；这些硬件事件只在 OrangePi 5 Plus 上验收。

脚本要求 `perf stat` 的 cycles 与 task-clock 非零；`perf record -a -g
--call-graph fp` 必须生成非空 `perf.data`；`perf report --stdio` 必须报告非零 sample、
非零 `perf_leaf` 开销以及 `perf_level_one` 到 `perf_level_three` 的完整用户调用链。
板卡路径还检查 8 核 A55/A76 MIDR 与动态 PMU cpumask、CPU0 到 CPU4 的迁移、8 个
同类事件的 multiplex 比例，以及 CPU0/CPU4 上 cycles、instructions、cache-misses 和
branch-instructions 均递增。

这里的 Linux perf 是 Starry guest 内的标准 perf ABI。仓库的 `tools/qperf` / `cargo
starry perf` 是 QEMU TCG translation-block profiler，采集和分析的是 host QEMU
翻译块，不通过 guest `perf_event_open`、PMUv3 counter 或 perf mmap ring。

```bash
cargo xtask starry app qemu -t linux-perf --arch aarch64
cargo xtask starry app board -t linux-perf -b OrangePi-5-Plus
```
