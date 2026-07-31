# Quancheng Lab 2026 AxVisor Contest Demo

This directory contains the current redcola contest demo artifacts for:

- Zephyr RTOS IPv4/UDP networking baseline.
- Zephyr native RTOS latency baseline.
- Reliable Linux/RTOS UDP control protocol.
- AI inference to RTOS control closed loop.
- AxVisor-hosted Zephyr e1000 IPv4/UDP validation evidence.
- AxVisor dual-guest Linux/RTOS QCZ1 and AI closed-loop validation.
- Reproducible validation scripts and evidence references.

The code is intentionally kept under `os/axvisor/contest/quancheng2026/` so it can be reviewed separately from the AxVisor core scheduler, interrupt, timer and vCPU changes.

## Current Status

Validated on the Kali experiment host at `192.168.75.131`.

Main evidence as of 2026-07-27:

- Zephyr IPv4-only UDP baseline: `40/40 PASS`, `800/800` UDP echo, fatal marker `0`.
- Reliable UDP control campaign: `10/10 PASS`, `200/200` control messages, `40` duplicate ACK checks, fatal marker `0`.
- AI control smoke: `20/20` AI control messages, inference mean `0.0293 ms`, end-to-end mean `1.118 ms`.
- Zephyr native latency baseline: `qemu_cortex_a53`, `47` reported metrics, context switch `2400 ns`, ISR return `1071/1359 ns`, semaphore context switch `3440/3967 ns`, maximum reported primitive latency `46703 ns`, final marker `PROJECT EXECUTION SUCCESSFUL`.
- AxVisor + Zephyr e1000 strict probe: `20/20 PASS`, UDP success rate `1.000000`, RTT mean `1.070 ms`, QEMU monitor confirms `model=e1000`.
- AxVisor dual guest Linux/Zephyr QCZ1 + AI downloadable-runtime reproduction: Linux guest has `2` vCPUs online, plain UDP `20/20 PASS` with RTT mean `2.943 ms` and max `19.039 ms`, reliable UDP `10/10 PASS`, duplicate ACK `2`, retransmits `0`, AI control `10/10 PASS`, AI end-to-end mean `2.186 ms` and max `3.389 ms`, Linux guest periodic probe `2000` samples at `1 ms` period with mean lateness `0.829 ms`, p99 `4.455 ms` and max `10.167 ms`, RTOS guest periodic probe `1000` samples at `1 ms` period with mean lateness `0.110 ms`, p99 `0.887 ms` and max `5.156 ms`, tcpdump captures `88` packets with `0` kernel drops, final markers `QC_RTOS_PERIODIC_RESULT=PASS` and `QC_DUAL_GUEST_LINUX_INIT=PASS`.
- AxVisor dual guest Linux/Zephyr 0-worker long-sample reproduction: Linux guest stress workers `0`, Linux periodic probe `10000` samples at `1 ms` period with mean lateness `0.859 ms`, p99 `2.789 ms` and max `12.764 ms`, RTOS guest periodic probe `1000` samples at `1 ms` period with mean lateness `0.088 ms`, p99 `0.613 ms` and max `5.329 ms`, plain UDP `20/20 PASS`, reliable UDP `10/10 PASS`, duplicate ACK `2`, retransmits `0`, AI control `10/10 PASS`, AI end-to-end mean `1.668 ms` and max `1.925 ms`, tcpdump captures `88` packets with `0` kernel drops.
- AxVisor dual guest Linux/Zephyr 1-worker long/stress reproduction: Linux guest stress workers `1`, Linux periodic probe `10000` samples at `1 ms` period with mean lateness `0.828 ms`, p99 `2.559 ms` and max `9.586 ms`, RTOS guest periodic probe `1000` samples at `1 ms` period with mean lateness `0.123 ms`, p99 `1.228 ms` and max `4.352 ms`, plain UDP `20/20 PASS`, reliable UDP `10/10 PASS`, duplicate ACK `2`, retransmits `0`, AI control `10/10 PASS`, AI end-to-end mean `1.996 ms` and max `5.333 ms`, tcpdump captures `88` packets with `0` kernel drops.
- AxVisor dual guest Linux/Zephyr 2-worker long/stress reproduction: Linux guest stress workers `2`, Linux periodic probe `10000` samples at `1 ms` period with mean lateness `0.895 ms`, p99 `4.347 ms` and max `9.145 ms`, RTOS guest periodic probe `1000` samples at `1 ms` period with mean lateness `0.099 ms`, p99 `0.727 ms` and max `3.677 ms`, plain UDP `20/20 PASS`, reliable UDP `10/10 PASS`, duplicate ACK `2`, retransmits `0`, AI control `10/10 PASS`, AI end-to-end mean `4.964 ms` and max `21.059 ms`, tcpdump captures `88` packets with `0` kernel drops.
- AxVisor dual guest Linux/Zephyr 4-worker overcommit/stress reproduction: Linux guest stress workers `4` on a `2` vCPU Linux guest, Linux periodic probe `10000` samples at `1 ms` period with mean lateness `2.966 ms`, p99 `41.868 ms` and max `52.850 ms`, RTOS guest periodic probe `1000` samples at `1 ms` period with mean lateness `0.080 ms`, p99 `1.256 ms` and max `6.179 ms`, plain UDP `20/20 PASS`, reliable UDP `10/10 PASS`, duplicate ACK `2`, retransmits `0`, AI control `10/10 PASS`, AI end-to-end mean `8.140 ms` and max `39.642 ms`, tcpdump captures `88` packets with `0` kernel drops.
- AxVisor dual guest Linux/Zephyr 2-worker 3-run stability campaign: `3/3 PASS`, empty bad-scan logs, UDP `20/20` per round, QCZ1 `10/10` per round, AI `10/10` per round, tcpdump kernel drops `0` in all rounds. Across the three rounds, Linux periodic p99 ranged `4.301-16.140 ms`, RTOS periodic p99 ranged `0.864-0.985 ms`, and AI end-to-end max ranged `2.230-24.792 ms`.
- Submission design entry: `docs/design.md` summarizes the system architecture, guest configuration, AxVisor modification boundary, protocol, isolation design, AI deployment and reproducibility commands.
- Submission test entry: `docs/test-report.md` summarizes startup validation, communication reliability, realtime comparison, AI closed-loop metrics, stability results and artifact preflight checks.
- Reviewer traceability entry: `docs/scorecard-traceability.md` maps each contest requirement and scoring area to the current redcola evidence set.
- Final submission checklist: `docs/final-submission-checklist.md` records the current PR, core-patch, StarryOS bonus, demo-video and platform-submission gates.
- PR description draft: `docs/pr-description.md` provides a ready-to-use review summary and validation block.
- Task-One realtime evaluation: `docs/realtime-evaluation.md` explains the measurement scope, CPU/vCPU placement, platform differences, and reviewer-facing conclusions; `results/realtime-comparison.csv` records the 0/1/2/4-worker long-sample runs plus the 2-worker 3-run stability table.
- Task-Two network boundary: `docs/network-topology.md` records the IPv4/UDP data path, guest MAC/IP addresses, UDP port, bridge/TAP layout, no-NAT/no-uplink access boundary and measured communication counters.
- Task-Three AI evaluation: `docs/ai-control-evaluation.md` records the closed-loop path, model inputs and output, manual fixed-gain baseline, control-error metric and representative latency/error results.
- Demo video plan: `docs/demo-video-script.md` gives a 5-minute recording script with terminal layout, commands, narration, required PASS markers, and fallback replay flow.
- Latest clean dual-guest evidence archive SHA256: `a7963eda86c71d8cc475cb4b1af70b29a81eef76b4a06af26fc806d1c302e5c6`.
- Latest 0-worker long-sample dual-guest evidence archive SHA256: `b3a6dcc0503f7d2fae4add93c05c20aaad0a33874ac924bf1b9b26b9a7295ddd`.
- Latest 1-worker long/stress dual-guest evidence archive SHA256: `d4300613f3835c71f029e656d7dd209b84fbac25333a0d8378e7f3b72db29d0b`.
- Latest 2-worker long/stress dual-guest evidence archive SHA256: `69adb1c9741b33b4a5f718096f5e26c457ddbc450fa96168c15aa5dd86599cfa`.
- Latest 4-worker overcommit/stress dual-guest evidence archive SHA256: `9d8d94ac85222f73fa4fb5249cbc94ca52a1b0cb2c656c5d8105069da4bcb12f`.
- Latest 2-worker 3-run stability evidence SHA256 records:
  - round 1: `fbe83e24d41cc3cc1c9172656de3212e4e044625c0837f6ba7c8ec3f941ddb26`
  - round 2: `6437349e481dd3b5282abe27a34085e4a0d26b214cd7a88478ff7532446f7a16`
  - round 3: `38aac4038f06ae1731125cea46e6afce0b18d0cc5f0845ef7a562676a8cc97f5`
- Latest short final-demo rehearsal archive SHA256: `064e37dca1aec17cc6e7e3169aa80ebb4987a3920978073e5e9cf825b0618eb7` (`20/20` plain UDP, `10/10` QCZ1 reliable UDP, `10/10` AI control, Linux `2` vCPUs online, tcpdump kernel drops `0`).

Local evidence root on the Windows host:

```text
D:\暑假实习\泉城实验室\02_泉城实验室2026揭榜挂帅擂台赛\04_实验工作区\results\realtime\2026-07-26-native-zsock-fault
D:\暑假实习\泉城实验室\02_泉城实验室2026揭榜挂帅擂台赛\04_实验工作区\results\network\2026-07-27-dual-linux-zephyr-qcz1-ai-guest-pass
D:\暑假实习\泉城实验室\02_泉城实验室2026揭榜挂帅擂台赛\04_实验工作区\results\network\2026-07-27-contest-script-dual-guest-qcz1-ai-pass
D:\暑假实习\泉城实验室\02_泉城实验室2026揭榜挂帅擂台赛\04_实验工作区\results\network\2026-07-27-contest-script-dual-guest-qcz1-ai-rt-noirqdebug-pass
D:\暑假实习\泉城实验室\02_泉城实验室2026揭榜挂帅擂台赛\04_实验工作区\results\network\2026-07-27-contest-script-dual-guest-qcz1-ai-rtos-periodic-pass
D:\暑假实习\泉城实验室\02_泉城实验室2026揭榜挂帅擂台赛\04_实验工作区\results\network\2026-07-27-contest-script-dual-guest-qcz1-ai-stress-long-pass
D:\暑假实习\泉城实验室\02_泉城实验室2026揭榜挂帅擂台赛\04_实验工作区\results\network\2026-07-27-contest-script-dual-guest-qcz1-ai-stress2-long-pass
D:\暑假实习\泉城实验室\02_泉城实验室2026揭榜挂帅擂台赛\04_实验工作区\results\network\2026-07-27-contest-script-dual-guest-qcz1-ai-stress4-long-pass
D:\暑假实习\泉城实验室\02_泉城实验室2026揭榜挂帅擂台赛\04_实验工作区\contest-package\2026-07-27-demo-rehearsal-latest-evidence
```

## Directory Layout

```text
docs/
  design.md                           Submission design document entry point.
  test-report.md                      Submission test report entry point.
  scorecard-traceability.md           Reviewer requirement-to-evidence matrix.
  final-submission-checklist.md       Current PR, bonus, demo and platform gates.
  pr-description.md                   Draft PR summary and validation text.
  protocol.md                         Protocol frame format and behavior.
  network-topology.md                 Guest IP/MAC, bridge/TAP, port and access boundary.
  ai-control-evaluation.md            AI closed-loop scenario and manual baseline comparison.
  reproduce.md                        Build, run and evidence commands.
  demo-video-script.md                5-minute final demo recording script.
  e1000_axvisor.md                    AxVisor-hosted Zephyr e1000 evidence.
  realtime-evaluation.md              Task-One realtime comparison and conclusions.
  core-patch-review.md                AxVisor core patch split and risk notes.
  pr-boundary.md                      PR staging boundary and patch grouping notes.
  commit-plan.md                      First-stage commit preflight and message draft.
linux/
  qc_reliable_udp_client.py            Linux-side reliable UDP client.
  qc_ai_control_demo.py                Lightweight MLP AI control demo.
  qc_dual_guest_udp_echo_probe.c       Linux guest static plain UDP probe.
  qc_periodic_latency_probe.c          Linux guest periodic latency probe.
  qc_qcz1_guest_demo.c                 Linux guest static QCZ1 + AI demo.
  qc_dual_guest_qcz1_ai_init.sh        Linux guest init script for dual-guest evidence.
rtos/
  zephyr_ipv4only_udp_mgmt12288.conf   Stable Zephyr RTOS network config.
  zephyr_udp_qc_protocol.patch         Patch against Zephyr echo-server udp.c.
  zephyr_udp_qc_protocol_udp.c         Full udp.c snapshot with protocol logic.
scripts/
  qc_udp_echo_probe.py                 Plain UDP echo sanity probe.
  qc_reliable_udp_combined_probe.py    Plain echo + reliable UDP control probe.
  qc_ai_control_combined_probe.py      Plain echo + AI control probe.
  analyze_dual_guest_realtime.py       Latency/reliability report generator for dual-guest evidence.
  analyze_zephyr_latency_measure.py    Zephyr latency_measure report generator.
  run_axvisor_dual_guest_qcz1_ai.sh    Full AxVisor Linux/Zephyr dual-guest reproduction.
  run_native_zephyr_latency_baseline.sh
  run_native_zephyr_mgmt_stack_2048_nogdb_validation.sh
  run_native_zephyr_serial_validation_campaign.sh
results/
  CURRENT_STATUS_2026-07-26.md
  realtime-comparison.csv
  stability/2026-07-27-stress2-3x/stability-summary.md
  stability/2026-07-27-stress2-3x/stability-summary.csv
```

## Protocol Summary

The RTOS endpoint still accepts plain UDP echo packets for compatibility with the original Zephyr sample and smoke probes. Packets with magic `QCZ1` use the contest control protocol:

- `CONTROL_SET`: Linux/AI side sends control input and model output.
- `ACK`: RTOS confirms command processing and reports duplicate status.
- `STATE_REQ`: Linux side asks for current RTOS control state.
- `STATUS`: RTOS returns last sequence, setpoint, AI score, output and counters.
- `ERROR`: RTOS reports bad length, bad version, bad checksum or unsupported type.

See `docs/protocol.md` for the full frame format.
See `docs/network-topology.md` for the IP/MAC, port, bridge and access-control
boundary used by the integrated dual-guest run.

## Zephyr Native Latency Baseline

The task-one RTOS baseline uses Zephyr's official `tests/benchmarks/latency_measure` benchmark on native QEMU `qemu_cortex_a53`:

```bash
REPO=/path/to/tgoskits
cd "${REPO}/os/axvisor/contest/quancheng2026"
./scripts/run_native_zephyr_latency_baseline.sh
```

Known passing result from 2026-07-27:

```text
success_marker=1
metric_count=47
qemu_alive_after_run=0
thread.yield.preemptive.ctx.k_to_k       : 2400 ns
isr.resume.interrupted.thread.kernel     : 1071 ns
isr.resume.different.thread.kernel       : 1359 ns
semaphore.take.blocking.k_to_k           : 3440 ns
semaphore.give.wake+ctx.k_to_k           : 3967 ns
mutex.lock.immediate.recursive.kernel    : 768 ns
heap.malloc.immediate                    : 4656 ns
result=PASS
```

## AxVisor Dual-Guest Topology

The current integrated evidence runs the contest protocol in the required AxVisor dual-guest topology:

```text
Linux guest  <--- IP/UDP --->  Zephyr/RTOS guest
```

For the passing run, Linux uses a 2-vCPU guest pinned to pCPU 1-2 and Zephyr uses a 1-vCPU e1000 RTOS guest pinned to pCPU 0. The Linux VM includes a `gppt-gicd` device so Linux GIC distributor accesses do not disturb the Zephyr e1000 interrupt path. The Linux VM passes through PL011 and virtio IRQs `[1, 31, 47]` and boots with `noirqdebug` to avoid QEMU PL011 spurious interrupt diagnostics from pausing the measurement path during short contest runs.

The detailed network boundary is documented in `docs/network-topology.md`.
The AI/manual baseline comparison is documented in `docs/ai-control-evaluation.md`.

Prepare the reviewed runtime artifacts, then run:

```bash
REPO=/path/to/tgoskits
cd "${REPO}/os/axvisor/contest/quancheng2026"
./scripts/prepare_dual_guest_runtime_artifacts.sh --repo "${REPO}"
sudo -v
./scripts/run_axvisor_dual_guest_qcz1_ai.sh
```

The preparation script downloads the fixed public runtime archive:

```text
https://raw.githubusercontent.com/irinaparchina-art/tgoskits/contest/quancheng2026-runtime-artifacts/quancheng2026-dual-guest-runtime-v1.tar.xz
```

Archive SHA256:

```text
656687bab1f6e055a6be411ee5e4c4a83ccc9366f37c8df9fed0ff5457777283
```

It also installs the checked-in tgosimages registry template, pulls the AArch64
Alpine rootfs with `cargo xtask image --no-auto-sync -S tmp/axbuild/rootfs pull
--arch aarch64`, extracts only the expected Linux kernel, Zephyr binary and host
DTB paths, and verifies `runtime-artifacts-known-passing.sha256` before the
runner starts QEMU.

The default tap mode creates per-run bridge/TAP devices and starts tcpdump, so sudo authentication is deliberately supplied by the caller with sudo -v; the repository does not store a sudo password or use stdin password mode. Use --prepare-only to validate artifact preparation without creating host network devices.

For reviewer machines where creating host TAP devices is not available, the same runner can execute the two guests through QEMU hub networking:

```bash
./scripts/run_axvisor_dual_guest_qcz1_ai.sh --net-mode hub
```

This mode still requires the runtime artifacts above and still checks the Linux guest, Zephyr guest, plain UDP, QCZ1 reliable UDP, AI control and realtime markers before printing `result=PASS`. The runner also executes `scripts/qc_qcz1_guest_status_negative_selftest.py` before QEMU setup, so STATUS timeout and malformed STATUS responses are covered by the same documented runner entry. It intentionally skips host bridge/TAP creation and tcpdump capture, so the default tap mode remains the host-network lifecycle evidence.

Runtime artifact contract for the integrated dual-guest runner:

| Artifact | Expected path under repo root | Preparation source | Known passing SHA256 |
| --- | --- | --- | --- |
| AArch64 Alpine rootfs image | `tmp/axbuild/rootfs/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img` | `cargo xtask image --no-auto-sync -S tmp/axbuild/rootfs pull --arch aarch64` | `dc7540d3140fcaacc9c942fe1340a9a4d2c14e319fbad3aadf34261e85446424` |
| Rootfs image registry metadata | `tmp/axbuild/rootfs/images.toml` | Copied from checked-in `runtime-rootfs-images-known-passing.toml` when absent | `682b389cdff44b89486019aef0c356a7db37a30bbd5c365f869c9c0eabd1203a` |
| Linux guest kernel | `os/axvisor/tmp/images/qemu-aarch64/linux/linux-qemu` | Public runtime archive prepared by `scripts/prepare_dual_guest_runtime_artifacts.sh` | `f262d305daa57a8f59d848d530e0d24f0b48f9d0b39f86eeb27f4114845bef17` |
| Zephyr RTOS guest binary | `os/axvisor/tmp/images/qemu-aarch64/zephyr-e1000-0x90000000-qcz1/zephyr.bin` | Public runtime archive prepared by `scripts/prepare_dual_guest_runtime_artifacts.sh` | `0baf6b4a08dc13a69ed739afd5c58bb138f7ae23cbc46921e864cdb4cc660f86` |
| Host DTB | `os/axvisor/tmp/configs/2026-07-24_qemu-aarch64-host-reserve-zephyr-0x90000000.dtb` | Public runtime archive prepared by `scripts/prepare_dual_guest_runtime_artifacts.sh` | `0f840bc4c162c2c0bd8f871d97c2124c9083ebe7d6d2063855e0ade5a8aa90bc` |

The runner enforces the checked-in `runtime-artifacts-known-passing.sha256` manifest with `sha256sum --strict --check` before QEMU is started. The current manifest records the runtime artifact set used by the current-head TAP/tcpdump validation on 2026-07-29. The runner records the manifest file hash and the manifest check output in the evidence directory, and exits non-zero with `runtime_artifact_manifest_check=FAIL` if any runtime artifact is missing or does not match the known-passing contract. If the local rootfs registry metadata is absent, the runner restores it from the checked-in `runtime-rootfs-images-known-passing.toml` template before the manifest check; an existing mismatched registry is not overwritten and fails the check. Alternate runtime artifacts require updating this manifest in review together with the corresponding run evidence.

The preparation script is the clean-environment entry point for the integrated QEMU path. It uses a fixed archive URL and SHA256, validates the archive member list before extraction, then runs the checked-in manifest over the rootfs registry, rootfs image, Linux kernel, Zephyr RTOS binary and host DTB. The runner keeps enforcing the same manifest and exits before QEMU if any artifact is missing or mismatched. After the manifest check passes, the runner copies the rootfs into `tmp/quancheng2026-dual-guest-qcz1-ai-build/rootfs/` and passes that copy to `cargo xtask axvisor qemu --rootfs`; this path is intentionally outside axbuild image storage so the QEMU launch treats it as a caller-managed runtime artifact and does not re-enter image-manager download/sync logic after verification. The stress and long-sample commands below assume `scripts/prepare_dual_guest_runtime_artifacts.sh` has already completed successfully.

The script prints `result=PASS` only after the pre-QEMU QCZ1 C guest STATUS negative selftest passes and QEMU emits plain UDP, reliable QCZ1, AI control, Linux guest periodic, RTOS guest periodic and final Linux init PASS markers. The Linux-side Python client also includes `--selftest-status-validation` so reviewer-local UDP STATUS counterexamples cannot be accepted as PASS.

For the 0-worker long-sample baseline used as the no-pressure comparison point:

```bash
REPO=/path/to/tgoskits
cd "${REPO}/os/axvisor/contest/quancheng2026"
./scripts/run_axvisor_dual_guest_qcz1_ai.sh \
  --evidence-dir /tmp/qc_clean_long_20260727_033600_evidence \
  --timeout 180 \
  --linux-rt-samples 10000 \
  --linux-stress-workers 0 \
  --linux-stress-seconds 0
```

This mode collects the same Linux 1 ms periodic probe, RTOS guest 1 ms periodic probe, UDP/QCZ1 reliability data and AI closed-loop latency without guest-side busy-loop pressure. The known passing archive SHA256 is `b3a6dcc0503f7d2fae4add93c05c20aaad0a33874ac924bf1b9b26b9a7295ddd`.

For the longer stress-backed task-one run with one Linux guest stress worker:

```bash
REPO=/path/to/tgoskits
cd "${REPO}/os/axvisor/contest/quancheng2026"
./scripts/run_axvisor_dual_guest_qcz1_ai.sh \
  --evidence-dir /tmp/qc_stress_long_20260727_030428_evidence \
  --timeout 150 \
  --linux-rt-samples 10000 \
  --linux-stress-workers 1 \
  --linux-stress-seconds 0
```

This mode keeps one Linux guest CPU busy-loop worker active while collecting the Linux 1 ms periodic probe, RTOS guest 1 ms periodic probe, UDP/QCZ1 reliability data and AI closed-loop latency. The known passing archive SHA256 is `d4300613f3835c71f029e656d7dd209b84fbac25333a0d8378e7f3b72db29d0b`.

For a stronger 2-worker pressure run on the 2-vCPU Linux guest:

```bash
REPO=/path/to/tgoskits
cd "${REPO}/os/axvisor/contest/quancheng2026"
./scripts/run_axvisor_dual_guest_qcz1_ai.sh \
  --evidence-dir /tmp/qc_stress2_long_20260727_033000_evidence \
  --timeout 180 \
  --linux-rt-samples 10000 \
  --linux-stress-workers 2 \
  --linux-stress-seconds 0
```

This mode keeps two Linux guest CPU busy-loop workers active during the same integrated probe sequence. The known passing archive SHA256 is `69adb1c9741b33b4a5f718096f5e26c457ddbc450fa96168c15aa5dd86599cfa`.

For an overcommit stress run with four Linux workers on the 2-vCPU Linux guest:

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

This mode intentionally oversubscribes the 2-vCPU Linux guest while keeping the same Zephyr/e1000 RTOS guest, IP/UDP topology, QCZ1 reliable control path and AI closed loop active. The known passing archive SHA256 is `9d8d94ac85222f73fa4fb5249cbc94ca52a1b0cb2c656c5d8105069da4bcb12f`.

For a three-round stability campaign under the same 2-worker pressure profile:

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

The known stability campaign passed `3/3` rounds. See `results/stability/2026-07-27-stress2-3x/stability-summary.md` for the per-round evidence SHA256 values and aggregate latency table.

Generate a compact latency and reliability report from a completed evidence directory:

```bash
./scripts/analyze_dual_guest_realtime.py /tmp/<dual-guest-evidence-dir> --fail-on-missing
```
