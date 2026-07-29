# 当前攻关状态（2026-07-26）

## 已经坐实的证据

| 模块 | 状态 | 关键数据 | 证据位置 |
|---|---:|---|---|
| Zephyr IPv4-only UDP 基线 | PASS | `40/40 PASS`，UDP `800/800`，fatal marker `0` | `realtime/2026-07-26-native-zsock-fault/ipv4only-udp-mgmt12288-b40-40pass` |
| 可靠 UDP 控制协议 | PASS | `10/10 PASS`，control `200/200`，duplicate ACK `40`，ACK p95 `2.059 ms` | `realtime/2026-07-26-native-zsock-fault/reliable-udp-control-c10-10pass` |
| AI 控制闭环 smoke | PASS | AI control `20/20`，端到端 mean `1.118 ms`，AI 误差 `129.003`，手动误差 `204.640` | `realtime/2026-07-26-native-zsock-fault/ai-control-smoke-pass` |
| AI 控制闭环打包 10 轮 | PASS | `10/10 PASS`，AI control `200/200`，端到端 mean `1.1769 ms`，max `4.4360 ms` | `realtime/2026-07-26-native-zsock-fault/ai-control-c10-packaged-10pass` |
| AxVisor + Zephyr e1000 IPv4/UDP | PASS | strict probe `20/20 PASS`，RTT mean `1.070 ms`，p95 `1.569 ms`，QEMU monitor 确认 `model=e1000` | `network/2026-07-25-axvisor-zephyr-e1000-strict20-pass` |
| AxVisor + Zephyr e1000 + QCZ1/AI | PASS | 可靠 UDP `10/10 PASS`，duplicate ACK `2/2`，AI control `10/10 PASS`，端到端 mean `0.883 ms`，QEMU monitor 确认 PCI `8086:100e` | `network/2026-07-26-axvisor-zephyr-e1000-qcz1-reliable-ai-pass` |
| AxVisor Linux/Zephyr 双 Guest + QCZ1/AI Guest 内闭环 | PASS | Linux Guest 双 vCPU 启动；Zephyr RTOS Guest e1000 服务端；普通 UDP `6/6 PASS`；QCZ1 可靠 UDP `10/10 PASS`；duplicate ACK `2/2`；AI control `10/10 PASS`；AI e2e mean `2.078 ms`，max `3.201 ms`；最终 `QC_DUAL_GUEST_LINUX_INIT=PASS` | `network/2026-07-27-dual-linux-zephyr-qcz1-ai-guest-pass` |
| AxVisor 双 Guest QCZ1/AI 准备运行时工件后的复现实验 | PASS | 交付脚本 `run_axvisor_dual_guest_qcz1_ai.sh` 实测通过；Linux Guest `2` vCPU；普通 UDP `20/20 PASS`；QCZ1 可靠 UDP `10/10 PASS`，duplicate ACK `2`，重传 `0`；AI control `10/10 PASS`，AI e2e mean `1.666 ms`，max `2.155 ms`；tcpdump `88` 包、kernel drop `0`；已生成 `realtime-report.md`/`realtime-summary.json`；最终 `result=PASS` | `network/2026-07-27-contest-script-dual-guest-qcz1-ai-pass` |
| AxVisor 双 Guest QCZ1/AI + Linux 周期探针 clean run | PASS | Linux Guest `2` vCPU；PL011/virtio IRQ `[1,31,47]`；bootargs `noirqdebug`；普通 UDP `20/20 PASS`，RTT mean `3.218 ms`、max `22.945 ms`；QCZ1 `10/10 PASS`，duplicate ACK `2`、重传 `0`；AI `10/10 PASS`，infer mean `50 us`，e2e mean `1.819 ms`、max `2.560 ms`；周期探针 `2000` samples、`1 ms` period，mean lateness `0.838 ms`、p99 `2.524 ms`、max `4.200 ms`；tcpdump `88` 包、kernel drop `0`；bad IRQ 扫描为空；证据包 SHA256 `c04eacb0370af75bc0d0c115e90a2de819f4c27d05d384440f4abf4df77ff01e` | `network/2026-07-27-contest-script-dual-guest-qcz1-ai-rt-noirqdebug-pass` |
| AxVisor 双 Guest QCZ1/AI + Linux/RTOS 双侧周期探针 clean run | PASS | Linux Guest `2` vCPU；RTOS Guest Zephyr/e1000；普通 UDP `20/20 PASS`，RTT mean `2.943 ms`、max `19.039 ms`；QCZ1 `10/10 PASS`，duplicate ACK `2`、重传 `0`；AI `10/10 PASS`，infer mean `66 us`，e2e mean `2.186 ms`、max `3.389 ms`；Linux 周期探针 `2000` samples、`1 ms` period，mean lateness `0.829 ms`、p99 `4.455 ms`、max `10.167 ms`；RTOS 周期探针 `1000` samples、`1 ms` busy_wait，mean lateness `0.110 ms`、p99 `0.887 ms`、max `5.156 ms`；tcpdump `88` 包、kernel drop `0`；`analysis_result=PASS`；证据包 SHA256 `a7963eda86c71d8cc475cb4b1af70b29a81eef76b4a06af26fc806d1c302e5c6` | `network/2026-07-27-contest-script-dual-guest-qcz1-ai-rtos-periodic-pass` |
| AxVisor 双 Guest QCZ1/AI + 0-worker 长样本实时性对照 run | PASS | Linux Guest `2` vCPU；Linux guest busy worker `0`；普通 UDP `20/20 PASS`，RTT mean `7.113 ms`、max `50.686 ms`；QCZ1 `10/10 PASS`，duplicate ACK `2`、重传 `0`；AI `10/10 PASS`，infer mean `60 us`，e2e mean `1.668 ms`、max `1.925 ms`；Linux 周期探针 `10000` samples、`1 ms` period，mean lateness `0.859 ms`、p99 `2.789 ms`、max `12.764 ms`；RTOS 周期探针 `1000` samples、`1 ms` busy_wait，mean lateness `0.088 ms`、p99 `0.613 ms`、max `5.329 ms`；tcpdump `88` 包、kernel drop `0`；`analysis_result=PASS`；bad scan `PASS`；证据包 SHA256 `b3a6dcc0503f7d2fae4add93c05c20aaad0a33874ac924bf1b9b26b9a7295ddd` | `network/2026-07-27-contest-script-dual-guest-qcz1-ai-clean-long-pass` |
| AxVisor 双 Guest QCZ1/AI + 压力负载长样本实时性 run | PASS | Linux Guest `2` vCPU；Linux guest busy worker `1`；普通 UDP `20/20 PASS`，RTT mean `5.483 ms`、max `43.549 ms`；QCZ1 `10/10 PASS`，duplicate ACK `2`、重传 `0`；AI `10/10 PASS`，infer mean `56 us`，e2e mean `1.996 ms`、max `5.333 ms`；Linux 周期探针 `10000` samples、`1 ms` period，mean lateness `0.828 ms`、p99 `2.559 ms`、max `9.586 ms`；RTOS 周期探针 `1000` samples、`1 ms` busy_wait，mean lateness `0.123 ms`、p99 `1.228 ms`、max `4.352 ms`；tcpdump `88` 包、kernel drop `0`；`analysis_result=PASS`；bad scan `PASS`；证据包 SHA256 `d4300613f3835c71f029e656d7dd209b84fbac25333a0d8378e7f3b72db29d0b` | `network/2026-07-27-contest-script-dual-guest-qcz1-ai-stress-long-pass` |
| AxVisor 双 Guest QCZ1/AI + 2-worker 压力负载长样本 run | PASS | Linux Guest `2` vCPU；Linux guest busy worker `2`；普通 UDP `20/20 PASS`，RTT mean `6.673 ms`、max `49.705 ms`；QCZ1 `10/10 PASS`，duplicate ACK `2`、重传 `0`；AI `10/10 PASS`，infer mean `56 us`，e2e mean `4.964 ms`、max `21.059 ms`；Linux 周期探针 `10000` samples、`1 ms` period，mean lateness `0.895 ms`、p99 `4.347 ms`、max `9.145 ms`；RTOS 周期探针 `1000` samples、`1 ms` busy_wait，mean lateness `0.099 ms`、p99 `0.727 ms`、max `3.677 ms`；tcpdump `88` 包、kernel drop `0`；`analysis_result=PASS`；bad scan `PASS`；证据包 SHA256 `69adb1c9741b33b4a5f718096f5e26c457ddbc450fa96168c15aa5dd86599cfa` | `network/2026-07-27-contest-script-dual-guest-qcz1-ai-stress2-long-pass` |
| AxVisor 双 Guest QCZ1/AI + 4-worker 超额压力长样本 run | PASS | Linux Guest `2` vCPU；Linux guest busy worker `4`；普通 UDP `20/20 PASS`，RTT mean `7.406 ms`、max `48.008 ms`；QCZ1 `10/10 PASS`，duplicate ACK `2`、重传 `0`；AI `10/10 PASS`，infer mean `113 us`，e2e mean `8.140 ms`、max `39.642 ms`；Linux 周期探针 `10000` samples、`1 ms` period，mean lateness `2.966 ms`、p99 `41.868 ms`、max `52.850 ms`；RTOS 周期探针 `1000` samples、`1 ms` busy_wait，mean lateness `0.080 ms`、p99 `1.256 ms`、max `6.179 ms`；tcpdump `88` 包、kernel drop `0`；`analysis_result=PASS`；证据包 SHA256 `9d8d94ac85222f73fa4fb5249cbc94ca52a1b0cb2c656c5d8105069da4bcb12f` | `network/2026-07-27-contest-script-dual-guest-qcz1-ai-stress4-long-pass` |
| AxVisor 双 Guest QCZ1/AI + 2-worker 压力 3 轮稳定性 run | PASS | 同一 2-worker 压力配置连续 `3/3 PASS`；每轮普通 UDP `20/20`、QCZ1 `10/10`、AI `10/10`、tcpdump kernel drop `0`、bad scan 为空；Linux 周期 p99 三轮范围 `4.301-16.140 ms`；RTOS 周期 p99 三轮范围 `0.864-0.985 ms`；AI e2e max 三轮范围 `2.230-24.792 ms`；三轮证据包 SHA256 分别为 `fbe83e24d41cc3cc1c9172656de3212e4e044625c0837f6ba7c8ec3f941ddb26`、`6437349e481dd3b5282abe27a34085e4a0d26b214cd7a88478ff7532446f7a16`、`38aac4038f06ae1731125cea46e6afce0b18d0cc5f0845ef7a562676a8cc97f5` | `network/2026-07-27-contest-script-dual-guest-qcz1-ai-stress2-3x-pass` |
| AxVisor 双 Guest QCZ1/AI 最终演示彩排 run | PASS | 短演示配置实测 `result=PASS`；Linux Guest `2` vCPU；普通 UDP `20/20 PASS`，RTT mean `3.705 ms`、max `31.001 ms`；QCZ1 `10/10 PASS`，duplicate ACK `2`、重传 `0`；AI `10/10 PASS`，infer mean `66 us`，e2e mean `1.563 ms`、max `1.754 ms`；Linux 周期探针 `2000` samples，p99 `1.823 ms`、max `2.873 ms`；RTOS 周期探针 `1000` samples，p99 `1.427 ms`、max `7.008 ms`；tcpdump `88` 包、kernel drop `0`；证据包 SHA256 `064e37dca1aec17cc6e7e3169aa80ebb4987a3920978073e5e9cf825b0618eb7` | `contest-package/2026-07-27-demo-rehearsal-latest-evidence` |
| 任务一实时性正式对比材料 | PASS | 已把 Zephyr 原生 latency baseline、AxVisor 双 Guest 0/1/2/4-worker 长样本、2-worker 3 轮稳定性结果整理为评审口径文档和 CSV；文档说明测量范围、CPU/vCPU 放置、平台差异、延迟指标和可复现命令入口。 | `os/axvisor/contest/quancheng2026/docs/realtime-evaluation.md`，`os/axvisor/contest/quancheng2026/results/realtime-comparison.csv` |
| AxVisor 核心改动拆分审查 | PASS | 已把当前核心工作树拆成 4 个 patch 候选：VM config + vTimer、GIC EOI mode、bounded diagnostics、axbuild image helper；交付文档明确第一阶段只提交 contest 目录，核心功能补丁单独审查。 | `os/axvisor/contest/quancheng2026/docs/core-patch-review.md`，`contest-package/2026-07-27-core-patch-candidates/` |
| 5 分钟演示视频脚本 | PASS | 已按比赛演示要求整理录屏布局、主复现命令、备用回放流程、逐分钟讲稿、需要捕获的 PASS marker 和结果解读重点。 | `os/axvisor/contest/quancheng2026/docs/demo-video-script.md` |

## 新增排查记录

| 模块 | 状态 | 关键结论 | 证据位置 |
|---|---:|---|---|
| AxVisor Linux/Zephyr 双 Guest + user hub + QCZ1 | FAIL | 两个 VM 均创建和启动成功；Zephyr/QCZ1 已启动 IPv4 `192.0.2.20` 并监听 UDP `4242`；Linux Guest 双核启动、`eth0` MAC 正确；UDP `0/20`，未出现 `NETDEV WATCHDOG`。问题已收敛到双 Guest virtio-net 数据面或中断/队列转发路径。 | `network/2026-07-26-axvisor-linux-zephyr-dual-userhub-qcz1-fail` |
| AxVisor Linux virtio-net + Zephyr e1000 双 Guest + QEMU hub | FAIL | 两个 VM 均创建和启动成功；Zephyr e1000/QCZ1 在 `0x90000000` 启动，IPv4 `192.0.2.20` 并监听 UDP `4242`；Linux Guest 双核启动、`eth0=192.0.2.10`；UDP `0/20`，Linux `eth0` 统计为 RX `0`、TX `96`，无 `NETDEV WATCHDOG`、Zephyr fatal 或 Linux panic。问题进一步收敛到 QEMU hub/二层帧转发、PCIe/e1000 直通收发或 AxVisor 设备隔离路径。 | `network/2026-07-26-axvisor-linux-virtio-zephyr-e1000-hub-qcz1-fail` |
| 双 Guest 网络 blocker 解除 | PASS | 采用 Linux Guest `gppt-gicd-only` 配置避免 Linux GIC/ITS 路径破坏 Zephyr e1000 中断；rootfs 注入前后必须 `e2fsck -fy` 清理 ext4 journal，否则 debugfs 写入会被 Guest 启动时 journal replay 覆盖。 | `network/2026-07-27-dual-linux-zephyr-qcz1-ai-guest-pass` |

## 已整理到 tgoskits 的交付目录

远端 Kali 仓库：

`${REPO}/os/axvisor/contest/quancheng2026`

当前内容包括：

- `README.md`
- `docs/design.md`
- `docs/test-report.md`
- `docs/protocol.md`
- `docs/network-topology.md`
- `docs/ai-control-evaluation.md`
- `docs/reproduce.md`
- `docs/demo-video-script.md`
- `docs/e1000_axvisor.md`
- `docs/realtime-evaluation.md`
- `docs/core-patch-review.md`
- `docs/pr-boundary.md`
- `docs/commit-plan.md`
- `linux/qc_reliable_udp_client.py`
- `linux/qc_ai_control_demo.py`
- `linux/qc_dual_guest_udp_echo_probe.c`
- `linux/qc_periodic_latency_probe.c`
- `linux/qc_qcz1_guest_demo.c`
- `linux/qc_dual_guest_qcz1_ai_init.sh`
- `rtos/zephyr_ipv4only_udp_mgmt12288.conf`
- `rtos/zephyr_udp_qc_protocol.patch`
- `rtos/zephyr_udp_qc_protocol_udp.c`
- `results/realtime-comparison.csv`
- `results/stability/2026-07-27-stress2-3x/stability-summary.md`
- `results/stability/2026-07-27-stress2-3x/stability-summary.csv`
- `scripts/qc_udp_echo_probe.py`
- `scripts/qc_reliable_udp_combined_probe.py`
- `scripts/qc_ai_control_combined_probe.py`
- `scripts/analyze_dual_guest_realtime.py`
- `scripts/analyze_zephyr_latency_measure.py`
- `scripts/run_axvisor_dual_guest_qcz1_ai.sh`
- `scripts/run_native_zephyr_latency_baseline.sh`
- `scripts/run_native_zephyr_mgmt_stack_2048_nogdb_validation.sh`
- `scripts/run_native_zephyr_serial_validation_campaign.sh`

静态检查：

- Python `py_compile`：`PASS`
- Shell `bash -n`：`PASS`
- artifact / template / stale 3-run 扫描：`PASS`
- e1000 文档索引：`PASS`
- `run_axvisor_dual_guest_qcz1_ai.sh --prepare-only`：`PASS`
- `run_axvisor_dual_guest_qcz1_ai.sh --prepare-only --linux-rt-samples 3000 --linux-stress-workers 1`：`PASS`
- `run_axvisor_dual_guest_qcz1_ai.sh --timeout 95`：`PASS`
- `run_axvisor_dual_guest_qcz1_ai.sh --timeout 180 --linux-rt-samples 10000 --linux-stress-workers 0`：`PASS`
- `run_axvisor_dual_guest_qcz1_ai.sh --timeout 150 --linux-rt-samples 10000 --linux-stress-workers 1`：`PASS`
- `run_axvisor_dual_guest_qcz1_ai.sh --timeout 180 --linux-rt-samples 10000 --linux-stress-workers 2`：`PASS`
- `analyze_dual_guest_realtime.py --fail-on-missing`：`PASS`
- `cargo fmt --check -p arm_vcpu -p arm_vgic -p axvmconfig -p axvm -p axbuild`：`PASS`
- `cargo test -p axvmconfig -p axvm -p arm_vgic --lib`：`PASS`（`arm_vgic 5/5`，`axvm 110/110`，`axvmconfig 18/18`）
- `cargo test -p arm_vcpu --lib`：`PASS`
- `CARGO_BUILD_JOBS=1 cargo test -p axbuild image::tests::parses_pull_by_arch --lib`：`PASS`
- `CARGO_BUILD_JOBS=1 cargo test -p axbuild image::storage::tests::pull_rootfs_image_returns_extracted_rootfs_file --lib`：`PASS`

本地交付目录快照（源码、文档、演示脚本、正式实时性对比表、核心 patch 审查说明和小型稳定性摘要，不含镜像和原始日志包）：

Latest source/documentation upload package is recorded outside the repository in:

`contest-package/FINAL_UPLOAD_MANIFEST_2026-07-27.md`

SHA256：

See the external upload manifest and its `SHA256SUMS.txt`. The package SHA is kept outside this in-repository status file to avoid self-referential archive hashing.

核心 patch 候选包：

`contest-package/2026-07-27-core-patch-candidates/`

SHA256：

- `core-01-vmconfig-vtimer.patch`：`52f4909c41c316bacdb25d57bcf0771ad66ef8995a0492bac36f37bbcc847ff8`
- `core-02-gic-eoi-mode.patch`：`9bcc107c630541a2753d6e300da0edc542e19baf8d8ec75cf911bbe7c61c0d01`
- `core-03-bounded-diagnostics.patch`：`3e3993ebf1a869517afd8044741c47726f07a2694eb15e8457b7d23e1bf60eb7`
- `core-04-axbuild-image-helper.patch`：`117c1cb719ddc60e7943ce48c6bb630e54d60d7db55c30c961bd59d2decd4655`

准备运行时工件后的复现实验证据：

`network/2026-07-27-contest-script-dual-guest-qcz1-ai-rtos-periodic-pass`

证据包 SHA256：

`a7963eda86c71d8cc475cb4b1af70b29a81eef76b4a06af26fc806d1c302e5c6`

0-worker 长样本对照实验证据：

`network/2026-07-27-contest-script-dual-guest-qcz1-ai-clean-long-pass`

证据包 SHA256：

`b3a6dcc0503f7d2fae4add93c05c20aaad0a33874ac924bf1b9b26b9a7295ddd`

压力/长样本实验证据：

`network/2026-07-27-contest-script-dual-guest-qcz1-ai-stress-long-pass`

证据包 SHA256：

`d4300613f3835c71f029e656d7dd209b84fbac25333a0d8378e7f3b72db29d0b`

2-worker 压力/长样本实验证据：

`network/2026-07-27-contest-script-dual-guest-qcz1-ai-stress2-long-pass`

证据包 SHA256：

`69adb1c9741b33b4a5f718096f5e26c457ddbc450fa96168c15aa5dd86599cfa`

4-worker 超额压力/长样本实验证据：

`network/2026-07-27-contest-script-dual-guest-qcz1-ai-stress4-long-pass`

证据包 SHA256：

`9d8d94ac85222f73fa4fb5249cbc94ca52a1b0cb2c656c5d8105069da4bcb12f`

## 当前边界

tgoskits 当前仍有较多未审查核心改动，不能直接 `git add .` 或整体提交。

目前可安全单独审查的交付边界是：

`os/axvisor/contest/quancheng2026/`

## 距离一等奖/擂主还差的关键工作

1. 整理 AxVisor 核心改动，明确哪些是必须提交的调度/中断/定时器/网络相关修正，并把未完成实验、镜像、临时文件排除在 PR 外。
2. 按 `docs/demo-video-script.md` 实录 5 分钟演示视频。
3. 根据赛事提交方式决定是否先提交 `os/axvisor/contest/quancheng2026/` 的 38 个交付文件，再单独审查核心补丁。

## 下一步建议

优先推进 PR 审查和核心改动拆分；双 Guest 通信、QCZ1 可靠协议、AI 闭环、Linux 周期探针、RTOS Guest 周期探针、0-worker 对照、1-worker、2-worker 和 4-worker 压力/长样本实时性 run 都已经完成准备运行时工件后的复现实验脚本和证据沉淀，2-worker 压力也已经完成 3 轮稳定性复核，任务一正式对比材料已整理到 `docs/realtime-evaluation.md` 和 `results/realtime-comparison.csv`，演示视频脚本已整理到 `docs/demo-video-script.md`。后续重点是核心改动边界审查、最终交付包和实录演示视频。

当前已经有：

- RTOS e1000 在 AxVisor 下能通 IP/UDP；
- `QCZ1` 可靠 UDP 和 AI 控制闭环已经在 AxVisor-hosted Zephyr e1000 通路跑通；
- 可靠 UDP 协议和 AI 控制闭环在 Zephyr RTOS 侧能稳定运行；
- Linux 侧客户端和 AI demo 已经打包；
- Linux Guest 与 RTOS Guest 双侧 1 ms 周期探针已经进入复现脚本；
- Linux Guest CPU 压力负载和 `10000` 样本周期探针已经在 0-worker、1-worker、2-worker、4-worker 四档下跑通并归档；
- 2-worker 压力配置已经完成连续 3 轮稳定性复跑，三轮均 PASS。

现在两条线已经合并：Linux Guest 作为客户端，Zephyr/e1000 RTOS Guest 作为服务端，已经跑通 `QCZ1` 可靠控制、AI 控制闭环、双侧周期探针、0/1/2/4-worker 长样本 run 和 2-worker 三轮稳定性 run。下一步应该把配置/镜像生成说明、对比图表、PR 边界审查和演示视频补齐。

## 2026-07-27 新增：Zephyr 原生实时性基线 PASS

新增交付脚本：

- `scripts/run_native_zephyr_latency_baseline.sh`
- `scripts/analyze_zephyr_latency_measure.py`

正式运行结果：

- 远端证据目录：`/tmp/qc_zephyr_latency_20260727_014029_script_evidence`
- Windows 证据目录：`results\realtime\2026-07-27-native-zephyr-latency-baseline-pass`
- 证据包 SHA256：`495811bbbe818b53a508d06f779de70743780283ed991bbdb5552c4476e66603`
- Zephyr：`v4.4.0-dirty`
- Board：`qemu_cortex_a53`
- benchmark：`tests/benchmarks/latency_measure`
- `success_marker=1`
- `metric_count=47`
- `qemu_alive_after_run=0`
- `result=PASS`

关键实时性基线指标：

| 指标 | 结果 |
|---|---:|
| preemptive `k_yield` 上下文切换 | `2400 ns` |
| cooperative `k_yield` 上下文切换 | `2400 ns` |
| ISR 返回到被中断线程 | `1071 ns` |
| ISR 返回并切换到另一线程 | `1359 ns` |
| semaphore take 阻塞切换 | `3440 ns` |
| semaphore give 唤醒切换 | `3967 ns` |
| mutex lock | `768 ns` |
| heap malloc | `4656 ns` |
| 全部 47 项最大值 | `46703 ns` |

说明：`run_status=124` 是包装脚本预期行为，因为 `west build -t run` 在 benchmark 打印 `PROJECT EXECUTION SUCCESSFUL` 后仍保持 QEMU 窗口，脚本通过 timeout 结束 QEMU；判定 PASS 依赖 success marker、解析到 47 项指标、且无 QEMU 残留。

这个结果补齐了任务一中的“RTOS 原生/等价基线”一部分，但还不能替代 AxVisor-hosted RTOS Guest 的周期任务抖动、最大延迟和长时间稳定性实验。下一步继续做 AxVisor 下的 periodic jitter + stress，并与这组原生 Zephyr baseline 对比。

## 2026-07-27 新增：AxVisor-hosted RTOS Guest 周期探针 PASS

最新准备运行时工件后的复现实验已把 RTOS Guest 内部 1 ms 周期探针加入 Zephyr/e1000 客户机，并与 Linux Guest 周期探针、普通 UDP、QCZ1 可靠协议、AI 控制闭环一起纳入 `run_axvisor_dual_guest_qcz1_ai.sh` 的 PASS 判定。

正式运行结果：

- 远端证据目录：`/tmp/qc_full_rtos_periodic_20260727_024133_evidence`
- Windows 证据目录：`results\network\2026-07-27-contest-script-dual-guest-qcz1-ai-rtos-periodic-pass`
- 证据包 SHA256：`a7963eda86c71d8cc475cb4b1af70b29a81eef76b4a06af26fc806d1c302e5c6`
- 分析器：`analyze_dual_guest_realtime.py --fail-on-missing`
- `analysis_result=PASS`
- `QC_RTOS_PERIODIC_RESULT=PASS`
- `result=PASS`

关键指标：

| 指标 | 结果 |
|---|---:|
| 普通 UDP | `20/20 PASS` |
| QCZ1 可靠 UDP | `10/10 PASS`，duplicate ACK `2`，retransmits `0` |
| AI 闭环 | `10/10 PASS`，infer mean `66 us`，e2e mean `2186 us`，max `3389 us` |
| Linux 周期探针 | `2000` samples，mean `829168 ns`，p99 `4455344 ns`，max `10166704 ns` |
| RTOS 周期探针 | `1000` samples，busy_wait，mean `110157 ns`，p99 `886736 ns`，max `5155776 ns` |
| tcpdump | `88` packets，kernel drops `0` |

说明：RTOS 周期探针当前是 Guest 内部 `busy_wait` 口径，适合作为 AxVisor-hosted RTOS Guest 的 1 ms 周期循环延迟证据；压力/长样本 run 已经归档，后续继续补多轮复跑和不同压力强度对比。

## 2026-07-27 新增：AxVisor 双 Guest 压力/长样本实时性 PASS

脚本已支持压力参数：

- `--linux-rt-samples N`：设置 Linux Guest 1 ms 周期探针样本数，默认 `2000`。
- `--linux-stress-workers N`：在 Linux Guest 内启动 CPU busy-loop worker，默认 `0`。
- `--linux-stress-seconds N`：压力持续秒数，`0` 表示持续到所有探针结束。

正式运行命令：

```bash
./scripts/run_axvisor_dual_guest_qcz1_ai.sh \
  --evidence-dir /tmp/qc_stress_long_20260727_030428_evidence \
  --timeout 150 \
  --linux-rt-samples 10000 \
  --linux-stress-workers 1 \
  --linux-stress-seconds 0
```

正式运行结果：

- 远端证据目录：`/tmp/qc_stress_long_20260727_030428_evidence`
- Windows 证据目录：`results\network\2026-07-27-contest-script-dual-guest-qcz1-ai-stress-long-pass`
- 证据包 SHA256：`d4300613f3835c71f029e656d7dd209b84fbac25333a0d8378e7f3b72db29d0b`
- 分析器：`analyze_dual_guest_realtime.py --fail-on-missing`
- `analysis_result=PASS`
- 异常扫描：`BAD_SCAN=PASS`
- `result=PASS`

关键指标：

| 指标 | 结果 |
|---|---:|
| Linux Guest 压力负载 | `1` 个 busy worker，实验结束后 `STOPPED` |
| 普通 UDP | `20/20 PASS`，RTT mean `5483 us`，max `43549 us` |
| QCZ1 可靠 UDP | `10/10 PASS`，duplicate ACK `2`，retransmits `0` |
| AI 闭环 | `10/10 PASS`，infer mean `56 us`，e2e mean `1996 us`，max `5333 us` |
| Linux 周期探针 | `10000` samples，mean `827911 ns`，p99 `2559424 ns`，max `9586112 ns` |
| RTOS 周期探针 | `1000` samples，busy_wait，mean `122604 ns`，p99 `1227840 ns`，max `4352208 ns` |
| tcpdump | `88` packets，kernel drops `0` |

这个结果把任务一要求中的压力负载、最大延迟、长样本稳定性和异常扫描推进到可归档状态；2-worker 压力配置已经继续完成 3 轮复跑统计，可以向评审说明结果不是单次偶然通过。

## 2026-07-27 新增：AxVisor 双 Guest 2-worker 压力/长样本实时性 PASS

正式运行命令：

```bash
./scripts/run_axvisor_dual_guest_qcz1_ai.sh \
  --evidence-dir /tmp/qc_stress2_long_20260727_033000_evidence \
  --timeout 180 \
  --linux-rt-samples 10000 \
  --linux-stress-workers 2 \
  --linux-stress-seconds 0
```

正式运行结果：

- 远端证据目录：`/tmp/qc_stress2_long_20260727_033000_evidence`
- Windows 证据目录：`results\network\2026-07-27-contest-script-dual-guest-qcz1-ai-stress2-long-pass`
- 证据包 SHA256：`69adb1c9741b33b4a5f718096f5e26c457ddbc450fa96168c15aa5dd86599cfa`
- 分析器：`analyze_dual_guest_realtime.py --fail-on-missing`
- `analysis_result=PASS`
- 异常扫描：`BAD_SCAN=PASS`
- `result=PASS`

关键指标：

| 指标 | 结果 |
|---|---:|
| Linux Guest 压力负载 | `2` 个 busy worker，实验结束后 `STOPPED` |
| 普通 UDP | `20/20 PASS`，RTT mean `6673 us`，max `49705 us` |
| QCZ1 可靠 UDP | `10/10 PASS`，duplicate ACK `2`，retransmits `0` |
| AI 闭环 | `10/10 PASS`，infer mean `56 us`，e2e mean `4964 us`，max `21059 us` |
| Linux 周期探针 | `10000` samples，mean `894770 ns`，p99 `4346944 ns`，max `9145360 ns` |
| RTOS 周期探针 | `1000` samples，busy_wait，mean `98703 ns`，p99 `726688 ns`，max `3676896 ns` |
| tcpdump | `88` packets，kernel drops `0` |

这组数据比 1-worker 压力更强：Linux 侧 2 个 vCPU 都有 busy worker 背景负载，AI 端到端最大延迟上升到 `21.059 ms`，但 UDP、QCZ1、AI 控制闭环、双侧周期探针和异常扫描仍全部 PASS。它适合作为任务一“压力负载下最坏情况响应”和任务三“端到端延迟上界”的强压力证据。

## 2026-07-27 新增：AxVisor 双 Guest 0-worker 长样本实时性对照 PASS

正式运行命令：

```bash
./scripts/run_axvisor_dual_guest_qcz1_ai.sh \
  --evidence-dir /tmp/qc_clean_long_20260727_033600_evidence \
  --timeout 180 \
  --linux-rt-samples 10000 \
  --linux-stress-workers 0 \
  --linux-stress-seconds 0
```

正式运行结果：

- 远端证据目录：`/tmp/qc_clean_long_20260727_033600_evidence`
- Windows 证据目录：`results\network\2026-07-27-contest-script-dual-guest-qcz1-ai-clean-long-pass`
- 证据包 SHA256：`b3a6dcc0503f7d2fae4add93c05c20aaad0a33874ac924bf1b9b26b9a7295ddd`
- 分析器：`analyze_dual_guest_realtime.py --fail-on-missing`
- `analysis_result=PASS`
- 异常扫描：`BAD_SCAN=PASS`
- `result=PASS`

关键指标：

| 指标 | 结果 |
|---|---:|
| Linux Guest 压力负载 | `0` 个 busy worker，`QC_LINUX_STRESS_RESULT=SKIP` |
| 普通 UDP | `20/20 PASS`，RTT mean `7113 us`，max `50686 us` |
| QCZ1 可靠 UDP | `10/10 PASS`，duplicate ACK `2`，retransmits `0` |
| AI 闭环 | `10/10 PASS`，infer mean `60 us`，e2e mean `1668 us`，max `1925 us` |
| Linux 周期探针 | `10000` samples，mean `859358 ns`，p99 `2788848 ns`，max `12764288 ns` |
| RTOS 周期探针 | `1000` samples，busy_wait，mean `87811 ns`，p99 `613216 ns`，max `5328816 ns` |
| tcpdump | `88` packets，kernel drops `0` |

这组数据是 1-worker/2-worker/4-worker 压力长样本的无压力对照。至此，任务一已经有 0/1/2/4-worker 四档同脚本、同协议、同双 Guest 拓扑的可复现实验数据，其中 4-worker 用于覆盖 2-vCPU Linux Guest 的超额压力场景，且 2-worker 压力场景已有 3 轮稳定性统计，后续重点转为正式对比表、复现说明和 PR 边界审查。

## 2026-07-27 新增：AxVisor 双 Guest 2-worker 压力 3 轮稳定性 PASS

正式运行命令模板：

```bash
for round in 1 2 3; do
  ./scripts/run_axvisor_dual_guest_qcz1_ai.sh \
    --evidence-dir "/tmp/qc_multirun_stress2_20260727_035234/round${round}_evidence" \
    --timeout 180 \
    --linux-rt-samples 10000 \
    --linux-stress-workers 2 \
    --linux-stress-seconds 0
  ./scripts/analyze_dual_guest_realtime.py \
    "/tmp/qc_multirun_stress2_20260727_035234/round${round}_evidence" \
    --fail-on-missing
done
```

正式运行结果：

- 远端证据根目录：`/tmp/qc_multirun_stress2_20260727_035234`
- Windows 证据目录：`results\network\2026-07-27-contest-script-dual-guest-qcz1-ai-stress2-3x-pass`
- 交付目录摘要：`results/stability/2026-07-27-stress2-3x/stability-summary.md`
- 三轮 `analysis_result=PASS`
- 三轮 `badscan.log` 均为空
- 三轮 tcpdump kernel drop 均为 `0`

三轮证据包 SHA256：

| 轮次 | SHA256 |
|---:|---|
| 1 | `fbe83e24d41cc3cc1c9172656de3212e4e044625c0837f6ba7c8ec3f941ddb26` |
| 2 | `6437349e481dd3b5282abe27a34085e4a0d26b214cd7a88478ff7532446f7a16` |
| 3 | `38aac4038f06ae1731125cea46e6afce0b18d0cc5f0845ef7a562676a8cc97f5` |

关键稳定性统计：

| 指标 | 第 1 轮 | 第 2 轮 | 第 3 轮 |
|---|---:|---:|---:|
| UDP 成功率 | `20/20` | `20/20` | `20/20` |
| QCZ1 成功率 | `10/10` | `10/10` | `10/10` |
| AI 成功率 | `10/10` | `10/10` | `10/10` |
| Linux 周期 mean/p99/max ns | `1275322/16139568/41942128` | `991262/4300992/9572688` | `1185251/7174032/23575136` |
| RTOS 周期 mean/p99/max ns | `103672/864272/4739680` | `115154/933488/4327488` | `118194/984736/4949296` |
| UDP RTT mean/max us | `3573/32804` | `6172/33907` | `6309/33870` |
| QCZ1 mean/max us | `2591/6173` | `5592/26197` | `3441/16016` |
| AI e2e mean/max us | `1585/2230` | `5177/24792` | `2861/6457` |

这个结果是目前任务一和任务三最有说服力的稳定性证据：同一双 Guest 拓扑、同一 2-worker 压力配置、同一脚本连续三轮通过，且通信、可靠协议、AI 闭环、双侧周期探针和异常扫描全部纳入 PASS 判定。

## 2026-07-27 新增：AxVisor 双 Guest 4-worker 超额压力/长样本实时性 PASS

补充目的：

- 在 2-vCPU Linux Guest 内启动 `4` 个 busy worker，形成超过 vCPU 数量的压力/超额调度场景。
- 验证在 Linux 侧明显受压时，Zephyr/e1000 RTOS Guest、IP/UDP 通道、QCZ1 可靠协议和 AI 控制闭环仍能完整通过。
- 作为 0/1/2-worker 长样本与 2-worker 三轮稳定性之外的 4-worker 超额压力夺擂加分证据。

资源处理记录：

- 第一次 4-worker run 在构建阶段失败，原因不是实验链路失败，而是 Kali `/tmp` tmpfs 94% 占用、swap 满载，内核日志显示 `Out of memory: Killed process rustc`。
- 已将 `/tmp/zephyr-sdk-minimal-inspect` 和 `/tmp/zephyr-qc-virtio-net-elr-src-guard` 挪到 `/home/kali/qc_tmp_archive_20260727/`，释放 tmpfs 后使用 `CARGO_BUILD_JOBS=1` 重跑通过。

正式运行命令：

```bash
REPO=/path/to/tgoskits
cd "${REPO}/os/axvisor/contest/quancheng2026"
CARGO_BUILD_JOBS=1 ./scripts/run_axvisor_dual_guest_qcz1_ai.sh \
  --evidence-dir /tmp/2026-07-27_05-57-22-dual-guest-qcz1-ai \
  --timeout 180 \
  --linux-rt-samples 10000 \
  --linux-stress-workers 4 \
  --linux-stress-seconds 0
./scripts/analyze_dual_guest_realtime.py \
  /tmp/2026-07-27_05-57-22-dual-guest-qcz1-ai \
  --fail-on-missing
```

正式运行结果：

- 远端证据目录：`/tmp/2026-07-27_05-57-22-dual-guest-qcz1-ai`
- Windows 证据目录：`results\network\2026-07-27-contest-script-dual-guest-qcz1-ai-stress4-long-pass`
- 证据包 SHA256：`9d8d94ac85222f73fa4fb5249cbc94ca52a1b0cb2c656c5d8105069da4bcb12f`
- 分析器：`analyze_dual_guest_realtime.py --fail-on-missing`
- `analysis_result=PASS`
- `result=PASS`

关键指标：

| 指标 | 结果 |
|---|---:|
| Linux Guest 压力负载 | `4` 个 busy worker，`QC_LINUX_STRESS_RESULT=STARTED/STOPPED` |
| 普通 UDP | `20/20 PASS`，RTT mean `7406 us`，max `48008 us` |
| QCZ1 可靠 UDP | `10/10 PASS`，duplicate ACK `2`，retransmits `0`，ACK mean `6627 us`，max `32095 us` |
| AI 闭环 | `10/10 PASS`，infer mean `113 us`，e2e mean `8140 us`，max `39642 us` |
| Linux 周期探针 | `10000` samples，mean `2966298 ns`，p99 `41868000 ns`，max `52850464 ns` |
| RTOS 周期探针 | `1000` samples，busy_wait，mean `80229 ns`，p99 `1255504 ns`，max `6179136 ns` |
| tcpdump | `88` packets，kernel drops `0` |

结论：4-worker 场景会显著拉高 Linux Guest 内部周期探针延迟，这是预期的 2-vCPU 超额压力表现；但 RTOS Guest 周期探针仍保持低均值，且 UDP、QCZ1 可靠通信和 AI 控制闭环全部 `10/10` 或 `20/20` 通过，说明隔离后的 RTOS/e1000 通道没有被 Linux 侧过载拖垮。
