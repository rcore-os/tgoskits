# 5-Minute Demo Video Script

This script is for the final Quancheng Lab 2026 AxVisor contest video. It is
written for a single-screen recording with two terminals: one terminal runs the
AxVisor reproduction script, and the other terminal shows the generated report
and evidence files.

## Recording Setup

Recommended terminal layout:

- Left terminal: run commands from `${REPO}/os/axvisor/contest/quancheng2026`.
- Right terminal: inspect `realtime-report.md`, `realtime-summary.json`,
  `docs/realtime-evaluation.md`, and `results/realtime-comparison.csv`.
- Keep the command prompt visible so reviewers can see the repository path.
- Use a clean evidence directory name for the recording, for example
  `/tmp/qc_demo_final_evidence`.
- Before recording, prepare the rootfs, Linux kernel, Zephyr RTOS binary and
  host DTB exactly as listed in `docs/reproduce.md`; the live command below is
  a prepared-artifact reproduction command, not a repository bootstrap.

Main command:

```bash
REPO=/path/to/tgoskits
cd "${REPO}/os/axvisor/contest/quancheng2026"
./scripts/run_axvisor_dual_guest_qcz1_ai.sh \
  --evidence-dir /tmp/qc_demo_final_evidence \
  --timeout 180 \
  --linux-rt-samples 3000 \
  --linux-stress-workers 1 \
  --linux-stress-seconds 0
```

Fast fallback command if recording time is tight:

```bash
REPO=/path/to/tgoskits
cd "${REPO}/os/axvisor/contest/quancheng2026"
./scripts/run_axvisor_dual_guest_qcz1_ai.sh \
  --evidence-dir /tmp/qc_demo_final_evidence \
  --timeout 120 \
  --linux-rt-samples 2000 \
  --linux-stress-workers 0 \
  --linux-stress-seconds 0
```

Post-run report commands:

```bash
./scripts/analyze_dual_guest_realtime.py /tmp/qc_demo_final_evidence --fail-on-missing
sed -n '1,180p' /tmp/qc_demo_final_evidence/realtime-report.md
cat /tmp/qc_demo_final_evidence/realtime-summary.json
```

## 0:00-0:30 Opening

Narration:

大家好，我们是 redcola 团队。我们的赛题是“智能化工控中基于虚拟化的混合系统部署及联动实现”。这个演示展示的是一个 AxVisor 混合系统：同一套 QEMU AArch64 平台上同时运行 Linux Guest 和 Zephyr RTOS Guest，通过 IP/UDP 网络通信完成可靠控制协议，并把 Linux 侧 AI 推理结果发送给 RTOS 侧形成控制闭环。

Screen:

- Show repository path.
- Show `git branch --show-current`.
- Show `README.md` current evidence bullets if there is enough time.

Suggested commands:

```bash
REPO=/path/to/tgoskits
cd "${REPO}"
git branch --show-current
git rev-parse --short HEAD
sed -n '1,40p' os/axvisor/contest/quancheng2026/README.md
```

## 0:30-1:10 Topology And Task Mapping

Narration:

这里的系统对应三个任务要求。任务一是实时性验证：Linux Guest 使用 2 个 vCPU，RTOS Guest 使用 1 个 vCPU，分别采集 1 ms 周期任务延迟，并和 Zephyr 原生 latency benchmark 做基线对照。任务二是客户机间通信：主数据通道是 IP/UDP，不使用共享内存或裸 HyperCall 作为主通道；应用层协议是 QCZ1，包含 magic、version、消息类型、长度、序号、时间戳和校验。任务三是 AI 联动：Linux Guest 中运行轻量神经网络推理，输出控制量，RTOS Guest 接收后更新控制状态并回传结果。

Screen:

- Show topology and protocol docs.

Suggested commands:

```bash
sed -n '1,80p' os/axvisor/contest/quancheng2026/docs/protocol.md
sed -n '1,70p' os/axvisor/contest/quancheng2026/docs/realtime-evaluation.md
```

## 1:10-2:45 Live Reproduction

Narration:

现在运行准备运行时工件后的复现实验脚本。这个脚本会准备 Linux rootfs、注入静态 AArch64 探针程序，启动 AxVisor 双 Guest，然后等待普通 UDP、QCZ1 可靠 UDP、AI 控制、Linux 周期探针、RTOS 周期探针和最终 Guest marker 全部通过。脚本只有在这些条件同时满足时才输出 PASS。

Screen:

- Run the main command in the left terminal.
- While it runs, keep the right terminal on the key requirements.
- When output appears, point out:
  - Linux Guest IP `192.0.2.10`
  - Zephyr RTOS Guest IP `192.0.2.20`
  - plain UDP `20/20 PASS`
  - QCZ1 reliable UDP `10/10 PASS`
  - AI control `10/10 PASS`
  - Linux periodic probe result
  - RTOS periodic probe result
  - tcpdump kernel drop `0`
  - final `result=PASS`

Suggested command:

```bash
./scripts/run_axvisor_dual_guest_qcz1_ai.sh \
  --evidence-dir /tmp/qc_demo_final_evidence \
  --timeout 180 \
  --linux-rt-samples 3000 \
  --linux-stress-workers 1 \
  --linux-stress-seconds 0
```

If the run has already completed before recording, replay the evidence report:

```bash
sed -n '1,220p' /tmp/qc_demo_final_evidence/runner.log
```

## 2:45-3:45 Evidence And Metrics

Narration:

实验结束后，我们用分析脚本做二次校验。这里不是只看 QEMU 是否启动，而是检查完整链路：网络请求成功率、QCZ1 应答、AI 端到端延迟、Linux 和 RTOS 两侧周期延迟、tcpdump 丢包计数以及最终 marker。已沉淀的长样本结果显示，在 0-worker、1-worker、2-worker 压力下，普通 UDP 和 QCZ1 都保持 100% 成功，AI 控制闭环保持 10/10 通过，RTOS 周期探针 mean lateness 均在 0.13 ms 以下。2-worker 压力还完成了 3 轮稳定性复核。

Screen:

- Run analyzer.
- Show report and CSV.

Suggested commands:

```bash
./scripts/analyze_dual_guest_realtime.py /tmp/qc_demo_final_evidence --fail-on-missing
sed -n '1,180p' /tmp/qc_demo_final_evidence/realtime-report.md
column -s, -t results/realtime-comparison.csv | sed -n '1,10p'
sed -n '1,120p' results/stability/2026-07-27-stress2-3x/stability-summary.md
```

## 3:45-4:35 Realtime Baseline And Isolation

Narration:

实时性基线分两层看。第一层是 Zephyr 原生 latency benchmark，它证明 RTOS 侧基准环境健康，记录了 47 项指标，例如 yield context switch、ISR return、semaphore wake 等。第二层是 AxVisor-hosted 双 Guest 场景，它包含 VM exit、虚拟中断、vTimer、Linux 负载和跨 Guest 网络流量，更接近赛题目标。我们没有把两个测试混成一个数字，而是在文档里说明了测量口径和平台差异。

Screen:

- Show native baseline section and realtime evaluation.

Suggested commands:

```bash
sed -n '1,120p' docs/realtime-evaluation.md
sed -n '120,220p' docs/realtime-evaluation.md
```

## 4:35-4:50 StarryOS Bonus Evidence

Narration:

除了标准 Linux Guest 路径，我们还准备了 StarryOS 加分项。StarryOS 作为非实时 Guest 运行同类 AI 控制程序，QEMU AArch64 串口输出 `REDCOLA_STARRY_AI_CONTROL_PASS`。这证明方案不只绑定传统 Linux，也能迁移到组件化 OS/StarryOS 方向。

Screen:

- Show the StarryOS bonus branch and latest PASS evidence.

Suggested commands:

```bash
cd /path/to/tgoskits
sed -n '1,120p' apps/starry/qemu/redcola-ai-control/README.md
sed -n '448,462p' /tmp/redcola-starry-ai-qemu-aarch64-release-20260730_032311.log
```

## 4:50-5:00 Closing

Narration:

总结一下，我们完成了 AxVisor 上 Linux/RTOS 双 Guest 部署，RTOS Guest 使用 Zephyr e1000 IP 网络，Linux 和 RTOS 之间用 QCZ1 可靠 UDP 协议通信，Linux 侧 AI 推理驱动 RTOS 控制状态更新，并提供了原生 RTOS 基线、双 Guest 长样本实时性数据、2-worker 三轮稳定性复核、StarryOS bonus 证据、复现脚本和核心 patch 拆分说明。

Screen:

- Show final files and package hash.

Suggested commands:

```bash
find . -maxdepth 2 -type f | sort
sed -n '1,140p' docs/core-patch-review.md
```

## Important Lines To Capture

Try to capture these strings in the video:

```text
result=PASS
plain_udp=20/20
qcz1=10/10
ai_control=10/10
QC_RTOS_PERIODIC_RESULT=PASS
QC_DUAL_GUEST_LINUX_INIT=PASS
tcpdump kernel drops=0
REDCOLA_STARRY_AI_CONTROL_PASS
```

The exact report formatting may differ between runs. The important point is
that the script and analyzer both gate the result and the recorded evidence
directory remains available after the run.
