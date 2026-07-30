# Reproduce Notes

These notes describe both the native Zephyr baseline path and the integrated AxVisor dual-guest path.

## Experiment Host

```text
host: kali@192.168.75.131
Zephyr workspace: /home/kali/qc-zephyrproject
Zephyr app: /home/kali/qc-zephyrproject/apps/echo_server_native_zsock_20260726
Build directory: /home/kali/qc-zephyrproject/build/echo_server_virtio_net_bus23_fixed_0x90000000_ipv4only_udp_mgmt12288
```

## Apply RTOS Protocol Patch

From the Zephyr echo-server app:

```bash
cd /home/kali/qc-zephyrproject/apps/echo_server_native_zsock_20260726
patch -p1 < /path/to/tgoskits/os/axvisor/contest/quancheng2026/rtos/zephyr_udp_qc_protocol.patch
```

For the current experiment workspace, the full patched file is also stored as:

```text
rtos/zephyr_udp_qc_protocol_udp.c
```

## Zephyr Config

Use:

```text
rtos/zephyr_ipv4only_udp_mgmt12288.conf
```

Key choices:

- IPv4 + UDP only.
- IPv6 disabled.
- TCP disabled.
- net shell disabled.
- `CONFIG_NET_MGMT_EVENT_STACK_SIZE=12288`.

## Build

The current build was made with:

```bash
cd /home/kali/qc-zephyrproject
/home/kali/qc-zephyrproject/.venv/bin/west build \
  -d /home/kali/qc-zephyrproject/build/echo_server_virtio_net_bus23_fixed_0x90000000_ipv4only_udp_mgmt12288
```

The build directory already records:

```text
BOARD=qemu_cortex_a53
DTC_OVERLAY_FILE=${REPO}/os/axvisor/tmp/configs/2026-07-24_zephyr-qemu-cortex-a53-virtio-net-bus23-sram-0x90000000.overlay
EXTRA_CONF_FILE=/tmp/2026-07-26_zephyr-echo-ipv4only-udp-mgmt12288.conf
```

For a clean rebuild, pass the same board, overlay and extra config explicitly.

## Single Smoke Run

Reliable UDP control:

```bash
/tmp/run_native_zephyr_mgmt_stack_2048_nogdb_validation.sh \
  /home/kali/qc-zephyrproject \
  /path/to/scripts/qc_reliable_udp_combined_probe.py \
  45 \
  2026-07-26-reliable-udp-control-smoke \
  15242 \
  15444 \
  /home/kali/qc-zephyrproject/build/echo_server_virtio_net_bus23_fixed_0x90000000_ipv4only_udp_mgmt12288
```

AI closed-loop smoke:

```bash
/tmp/run_native_zephyr_mgmt_stack_2048_nogdb_validation.sh \
  /home/kali/qc-zephyrproject \
  /path/to/scripts/qc_ai_control_combined_probe.py \
  45 \
  2026-07-26-ai-control-smoke \
  16242 \
  16444 \
  /home/kali/qc-zephyrproject/build/echo_server_virtio_net_bus23_fixed_0x90000000_ipv4only_udp_mgmt12288
```

## Campaign Run

Reliable UDP 10-round campaign:

```bash
/tmp/run_native_zephyr_serial_validation_campaign.sh \
  /home/kali/qc-zephyrproject \
  10 \
  45 \
  2026-07-26-reliable-udp-control-c10 \
  /tmp/run_native_zephyr_mgmt_stack_2048_nogdb_validation.sh \
  /path/to/scripts/qc_reliable_udp_combined_probe.py \
  /home/kali/qc-zephyrproject/build/echo_server_virtio_net_bus23_fixed_0x90000000_ipv4only_udp_mgmt12288
```


## AxVisor Zephyr e1000 Strict Probe

The current e1000 evidence validates Zephyr as an AxVisor guest using QEMU user-mode e1000 networking:

```text
QEMU NIC: -nic user,model=e1000
Guest IP: 192.0.2.1
Host IP: 192.0.2.2
Host UDP forward: 127.0.0.1:14243 -> 192.0.2.1:4242
```

The archived strict probe result is:

```text
marker_vm_created=PASS
marker_vm_booted=PASS
marker_zephyr_boot=PASS
marker_ipv4=PASS
marker_network_connected=PASS
marker_udp_ready=PASS
udp_attempt_count=20
udp_success_count=20
udp_success_rate=1.000000
udp_payload_validation=PASS
udp_rtt_mean_ms=1.070
udp_rtt_p95_ms=1.569
udp_rtt_max_ms=5.073
```

See `docs/e1000_axvisor.md` for the evidence bundle, fixes and reproduction artifacts.

## Evidence

Windows evidence root:

```text
D:\暑假实习\泉城实验室\02_泉城实验室2026揭榜挂帅擂台赛\04_实验工作区\results\realtime\2026-07-26-native-zsock-fault
```

Important archives:

```text
ipv4only-udp-mgmt12288-b40-40pass/qc-ipv4only-udp-mgmt12288-b40-40pass-evidence.tgz
reliable-udp-control-c10-10pass/qc-reliable-udp-control-c10-10pass-evidence.tgz
ai-control-smoke-pass/qc-ai-control-smoke-pass-evidence.tgz
../realtime/2026-07-27-native-zephyr-latency-baseline-pass/evidence.tar.gz
../realtime/2026-07-27-native-zephyr-latency-baseline-pass/evidence-tar-sha256.txt
../network/2026-07-25-axvisor-zephyr-e1000-strict20-pass/2026-07-24_axvisor-zephyr-e1000-el1ns-bam-only-host-eoi0-strict20-evidence.tar.gz
../network/2026-07-27-dual-linux-zephyr-qcz1-ai-guest-pass/summary.txt
../network/2026-07-27-dual-linux-zephyr-qcz1-ai-guest-pass/sha256.txt
../network/2026-07-27-contest-script-dual-guest-qcz1-ai-pass/summary.txt
../network/2026-07-27-contest-script-dual-guest-qcz1-ai-pass/evidence-tar-sha256.txt
../network/2026-07-27-contest-script-dual-guest-qcz1-ai-rt-noirqdebug-pass/summary.txt
../network/2026-07-27-contest-script-dual-guest-qcz1-ai-rt-noirqdebug-pass/realtime-report.md
../network/2026-07-27-contest-script-dual-guest-qcz1-ai-rt-noirqdebug-pass/evidence-tar-sha256.txt
../network/2026-07-27-contest-script-dual-guest-qcz1-ai-rtos-periodic-pass/summary.txt
../network/2026-07-27-contest-script-dual-guest-qcz1-ai-rtos-periodic-pass/realtime-report.md
../network/2026-07-27-contest-script-dual-guest-qcz1-ai-rtos-periodic-pass/evidence.tar.gz.sha256
../network/2026-07-27-contest-script-dual-guest-qcz1-ai-clean-long-pass/summary.txt
../network/2026-07-27-contest-script-dual-guest-qcz1-ai-clean-long-pass/realtime-report.md
../network/2026-07-27-contest-script-dual-guest-qcz1-ai-clean-long-pass/realtime-summary.json
../network/2026-07-27-contest-script-dual-guest-qcz1-ai-clean-long-pass/evidence.tar.gz.sha256
../network/2026-07-27-contest-script-dual-guest-qcz1-ai-stress-long-pass/summary.txt
../network/2026-07-27-contest-script-dual-guest-qcz1-ai-stress-long-pass/realtime-report.md
../network/2026-07-27-contest-script-dual-guest-qcz1-ai-stress-long-pass/realtime-summary.json
../network/2026-07-27-contest-script-dual-guest-qcz1-ai-stress-long-pass/evidence.tar.gz.sha256
../network/2026-07-27-contest-script-dual-guest-qcz1-ai-stress2-long-pass/summary.txt
../network/2026-07-27-contest-script-dual-guest-qcz1-ai-stress2-long-pass/realtime-report.md
../network/2026-07-27-contest-script-dual-guest-qcz1-ai-stress2-long-pass/realtime-summary.json
../network/2026-07-27-contest-script-dual-guest-qcz1-ai-stress2-long-pass/evidence.tar.gz.sha256
../network/2026-07-27-contest-script-dual-guest-qcz1-ai-stress2-3x-pass/stability-summary.md
../network/2026-07-27-contest-script-dual-guest-qcz1-ai-stress2-3x-pass/stability-summary.csv
../network/2026-07-27-contest-script-dual-guest-qcz1-ai-stress2-3x-pass/round1-evidence.tar.gz.sha256
../network/2026-07-27-contest-script-dual-guest-qcz1-ai-stress2-3x-pass/round2-evidence.tar.gz.sha256
../network/2026-07-27-contest-script-dual-guest-qcz1-ai-stress2-3x-pass/round3-evidence.tar.gz.sha256
```

## Zephyr Native Latency Baseline

This is the native RTOS baseline for task one. It runs Zephyr's official
`tests/benchmarks/latency_measure` on native QEMU `qemu_cortex_a53`, outside
AxVisor:

```bash
REPO=/path/to/tgoskits
cd "${REPO}/os/axvisor/contest/quancheng2026"
./scripts/run_native_zephyr_latency_baseline.sh
```

The script writes:

```text
build.log
run.log
latency-summary.json
latency-report.md
summary.txt
sha256.txt
```

Known passing result from 2026-07-27:

```text
evidence_dir=/tmp/qc_zephyr_latency_20260727_014029_script_evidence
zephyr_version=v4.4.0-dirty
board=qemu_cortex_a53
run_status=124
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

`run_status=124` is expected for this wrapper because `west build -t run`
keeps QEMU open after the benchmark prints `PROJECT EXECUTION SUCCESSFUL`.
The script treats the run as passing only when the success marker is present,
metrics are parsed, and no QEMU process remains after timeout cleanup.

## AxVisor Dual-Guest QCZ1 and AI Reproduction

The integrated task-two and task-three path is reproduced after preparing the runtime artifacts that are intentionally not checked into this first-stage contest PR:

```bash
REPO=/path/to/tgoskits
cd "${REPO}"
cargo xtask image pull --arch aarch64 -S tmp/axbuild/rootfs

install -D /path/to/linux-qemu \
  os/axvisor/tmp/images/qemu-aarch64/linux/linux-qemu
install -D /path/to/zephyr.bin \
  os/axvisor/tmp/images/qemu-aarch64/zephyr-e1000-0x90000000-qcz1/zephyr.bin
install -D /path/to/2026-07-24_qemu-aarch64-host-reserve-zephyr-0x90000000.dtb \
  os/axvisor/tmp/configs/2026-07-24_qemu-aarch64-host-reserve-zephyr-0x90000000.dtb

cd os/axvisor/contest/quancheng2026
sudo -v
./scripts/run_axvisor_dual_guest_qcz1_ai.sh
```

The default tap mode creates per-run bridge/TAP devices and starts tcpdump, so sudo authentication is deliberately supplied by the caller with sudo -v; the repository does not store a sudo password or use stdin password mode. Use --prepare-only to validate artifact preparation without creating host network devices.

For reviewer machines where creating host TAP devices is not available, the same runner can execute the two guests through QEMU hub networking:

```bash
./scripts/run_axvisor_dual_guest_qcz1_ai.sh --net-mode hub
```

This mode still requires the runtime artifacts above and still checks the Linux guest, Zephyr guest, plain UDP, QCZ1 reliable UDP, AI control and realtime markers before printing `result=PASS`. It intentionally skips host bridge/TAP creation and tcpdump capture, so the default tap mode remains the host-network lifecycle evidence.

Runtime artifact contract for the integrated dual-guest runner:

| Artifact | Expected path under repo root | Preparation source | Known passing SHA256 |
| --- | --- | --- | --- |
| AArch64 Alpine rootfs image | `tmp/axbuild/rootfs/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img` | `cargo xtask image pull --arch aarch64 -S tmp/axbuild/rootfs` | `dc7540d3140fcaacc9c942fe1340a9a4d2c14e319fbad3aadf34261e85446424` |
| Linux guest kernel | `os/axvisor/tmp/images/qemu-aarch64/linux/linux-qemu` | Matching local AxVisor image/build output | `f262d305daa57a8f59d848d530e0d24f0b48f9d0b39f86eeb27f4114845bef17` |
| Zephyr RTOS guest binary | `os/axvisor/tmp/images/qemu-aarch64/zephyr-e1000-0x90000000-qcz1/zephyr.bin` | Matching local Zephyr/e1000 RTOS build output | `0baf6b4a08dc13a69ed739afd5c58bb138f7ae23cbc46921e864cdb4cc660f86` |
| Host DTB | `os/axvisor/tmp/configs/2026-07-24_qemu-aarch64-host-reserve-zephyr-0x90000000.dtb` | Matching local AxVisor host-device-tree output | `0f840bc4c162c2c0bd8f871d97c2124c9083ebe7d6d2063855e0ade5a8aa90bc` |

The runner enforces the checked-in `runtime-artifacts-known-passing.sha256` manifest with `sha256sum --strict --check` before QEMU is started. The current manifest records the runtime artifact set used by the current-head TAP/tcpdump validation on 2026-07-29. The runner records the manifest file hash and the manifest check output in the evidence directory, and exits non-zero with `runtime_artifact_manifest_check=FAIL` if any runtime artifact is missing or does not match the known-passing contract. Alternate runtime artifacts require updating this manifest in review together with the corresponding run evidence.

The runner uses the extracted rootfs image from `cargo xtask image pull`; if the image is absent, it attempts that image-manager pull before checking the rest of the runtime artifact contract and manifest. The Linux kernel, Zephyr RTOS binary and host DTB paths above are not stored in this first-stage contest PR; they must be supplied from the matching local AxVisor image/build output before running the integrated QEMU path. If those runtime artifacts are absent, the runner exits before QEMU with `missing_required_path=...`; in that state the PR only supports static validation and documentation review for this integrated path. The stress and long-sample commands later in this section assume the same runtime artifacts have already been prepared.

Current limitation: this PR does not claim that the Linux kernel, Zephyr RTOS binary or host DTB can be regenerated from this PR alone. The integrated QEMU path is a prepared-artifact reproduction path; generation of those runtime artifacts is kept outside this first-stage contest artifact PR and should be reviewed as a separate follow-up if needed.

The default topology is:

```text
Linux guest:
  vCPU/pCPU: 2 vCPU, pCPU 1-2
  IPv4/MAC: 192.0.2.10/24, 52:54:00:12:34:10
  device: virtio-net on virtio-mmio bus 31, offloads disabled
  IRQ passthrough: PL011 SPI 1 plus virtio IRQs 31 and 47
  bootargs: root=/dev/vda rw init=/qc-dual-net.sh noirqdebug
  role: QCZ1 reliable UDP client and AI inference controller

Zephyr RTOS guest:
  vCPU/pCPU: 1 vCPU, pCPU 0
  IPv4/MAC: 192.0.2.20/24, 52:54:00:12:34:20
  device: QEMU e1000, PCI 8086:100e
  role: UDP server, QCZ1 state machine and control actuator

Host network:
  per-run isolated bridge, exact name recorded as bridge= in bridge.txt
  per-run Linux TAP, exact name recorded as tap_linux= in bridge.txt
  per-run RTOS TAP, exact name recorded as tap_rtos= in bridge.txt
```

See `docs/network-topology.md` for the reviewer-facing network matrix,
including MAC addresses, IP addresses, UDP port `4242`, route assumptions,
the no-NAT/no-uplink bridge boundary and the access-control rationale.
See `docs/ai-control-evaluation.md` for the AI closed-loop scenario and the
manual fixed-gain baseline comparison.

The Linux guest VM config uses `gppt-gicd`:

```text
["gppt-gicd", 0x0800_0000, 0x1_0000, 0, 0x21, []]
```

This is required for the passing dual-guest run because it traps Linux GIC distributor accesses and prevents Linux from disturbing the Zephyr e1000 interrupt route.
The Linux VM passes through interrupt IDs `[1, 31, 47]`. ID `1` is the PL011 SPI from the QEMU AArch64 device tree, and `31`/`47` are the virtio-mmio interrupts used by the Linux guest. The `noirqdebug` boot argument is kept as a contest-run mitigation for QEMU PL011 spurious interrupt diagnostics; it avoids disabling IRQ 13 during short measurement runs and should be documented separately from deeper AxVisor interrupt-path work.

The script performs these steps:

```text
1. Compile linux/qc_dual_guest_udp_echo_probe.c as a freestanding static AArch64 ELF.
2. Compile linux/qc_qcz1_guest_demo.c as a freestanding static AArch64 ELF.
3. Compile linux/qc_periodic_latency_probe.c as a freestanding static AArch64 ELF.
4. Copy the canonical axbuild rootfs image produced by the image manager into the contest build directory.
5. Run e2fsck -fy before debugfs injection to replay and clear the ext4 journal.
6. Inject /qc-dual-net.sh, /qc-udp-probe, /qc-qcz1-demo and /qc-rt-probe.
7. Run e2fsck -fy after injection so the guest does not replay stale metadata.
8. Generate runtime, Linux VM and Zephyr VM TOML configs into the evidence directory.
9. Create per-run bridge/TAP objects and record their exact names in bridge.txt.
10. Run root-level cargo xtask axvisor qemu with both VM configs.
11. Capture qemu.log, tcpdump.log, summary.txt, realtime-report.md and SHA256 records.
```

The rootfs journal cleanup is important. Without the first `e2fsck -fy`, `debugfs` writes can appear to succeed but then be reverted when the Linux guest replays the ext4 journal on boot. The runner first copies the canonical axbuild rootfs image into the contest build directory and only modifies that copy.

The script reports `result=PASS` only if all required markers are present:

```text
QC_DUAL_GUEST_UDP_ECHO_RESULT=PASS
QC_RT_PERIODIC_RESULT=PASS
QC_RTOS_PERIODIC_RESULT=PASS
QC_QCZ1_RELIABLE_RESULT=PASS
QC_AI_CONTROL_RESULT=PASS
QC_QCZ1_GUEST_DEMO=PASS
QC_DUAL_GUEST_LINUX_INIT=PASS
```

Generate the latency/reliability report:

```bash
./scripts/analyze_dual_guest_realtime.py /tmp/<dual-guest-evidence-dir> --fail-on-missing
```

The report writes `realtime-summary.json` and `realtime-report.md` into the evidence directory. It covers the integrated Linux/RTOS device and network service path, including Linux guest and RTOS guest 1 ms periodic probes. The 0/1/2/4-worker runs form the current task-one long-sample dataset for AxVisor-hosted dual-guest realtime evidence, and the 4-worker run intentionally overcommits the 2-vCPU Linux guest.
The same script can also collect a longer Linux guest periodic probe and inject CPU pressure into the Linux guest:

```bash
./scripts/run_axvisor_dual_guest_qcz1_ai.sh \
  --evidence-dir /tmp/qc_clean_long_20260727_033600_evidence \
  --timeout 180 \
  --linux-rt-samples 10000 \
  --linux-stress-workers 0 \
  --linux-stress-seconds 0
```

```bash
./scripts/run_axvisor_dual_guest_qcz1_ai.sh \
  --evidence-dir /tmp/qc_stress_long_20260727_030428_evidence \
  --timeout 150 \
  --linux-rt-samples 10000 \
  --linux-stress-workers 1 \
  --linux-stress-seconds 0
```

`--linux-rt-samples` controls the Linux guest 1 ms periodic probe sample count. `--linux-stress-workers` starts guest-side busy-loop workers before the periodic, UDP, QCZ1 and AI probes run. `--linux-stress-seconds 0` keeps the workers active until the init script stops them after all probes finish.

For a stronger 2-worker stress run on the 2-vCPU Linux guest:

```bash
./scripts/run_axvisor_dual_guest_qcz1_ai.sh \
  --evidence-dir /tmp/qc_stress2_long_20260727_033000_evidence \
  --timeout 180 \
  --linux-rt-samples 10000 \
  --linux-stress-workers 2 \
  --linux-stress-seconds 0
```

To collect a multi-run stability campaign under the same 2-worker pressure profile, repeat the run and analyze each completed evidence directory:

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

Known prepared-artifact passing result from 2026-07-27:

```text
evidence_dir=/tmp/qc_full_rtos_periodic_20260727_024133_evidence
windows_evidence_dir=results\network\2026-07-27-contest-script-dual-guest-qcz1-ai-rtos-periodic-pass
analysis_result=PASS
QC_NPROC=2
QC_CPUINFO_PROCESSORS=2
QC_CPU_ONLINE=0-1
QC_RT_PERIOD_SAMPLES=2000
QC_RT_PERIOD_NS=1000000
QC_RT_LATENCY_MIN_NS=124288
QC_RT_LATENCY_MEAN_NS=829168
QC_RT_LATENCY_P50_NS=709824
QC_RT_LATENCY_P95_NS=1496576
QC_RT_LATENCY_P99_NS=4455344
QC_RT_LATENCY_MAX_NS=10166704
QC_RT_OVERRUN_GT_100US=2000
QC_RT_OVERRUN_GT_500US=1424
QC_RT_OVERRUN_GT_1000US=503
QC_RT_PERIODIC_RESULT=PASS
QC_RTOS_PERIOD_SAMPLES=1000
QC_RTOS_PERIOD_NS=1000000
QC_RTOS_PERIODIC_METHOD=busy_wait
QC_RTOS_LATENCY_MIN_NS=0
QC_RTOS_LATENCY_MEAN_NS=110157
QC_RTOS_LATENCY_P50_NS=48592
QC_RTOS_LATENCY_P95_NS=380848
QC_RTOS_LATENCY_P99_NS=886736
QC_RTOS_LATENCY_MAX_NS=5155776
QC_RTOS_OVERRUN_GT_100US=338
QC_RTOS_OVERRUN_GT_500US=26
QC_RTOS_OVERRUN_GT_1000US=8
QC_RTOS_PERIODIC_RESULT=PASS
QC_UDP_REQUESTS=20
QC_UDP_SUCCESSES=20
QC_UDP_FAILURES=0
QC_UDP_RTT_MEAN_US=2943
QC_UDP_RTT_MAX_US=19039
QC_QCZ1_RELIABLE_REQUESTS=10
QC_QCZ1_RELIABLE_SUCCESSES=10
QC_QCZ1_RELIABLE_FAILURES=0
QC_QCZ1_DUPLICATE_ACKS=2
QC_QCZ1_RETRANSMITS=0
QC_QCZ1_LATENCY_MEAN_US=3112
QC_QCZ1_LATENCY_MAX_US=8108
QC_AI_REQUESTS=10
QC_AI_SUCCESSES=10
QC_AI_FAILURES=0
QC_AI_INFER_MEAN_US=66
QC_AI_E2E_MEAN_US=2186
QC_AI_E2E_MAX_US=3389
QC_AI_CONTROL_ERROR_MEAN=207
QC_MANUAL_CONTROL_ERROR_MEAN=240
QC_AI_CONTROL_RESULT=PASS
QC_DUAL_GUEST_LINUX_INIT=PASS
tcpdump_packets_captured=88
tcpdump_packets_dropped_by_kernel=0
result=PASS
evidence_tar_sha256=a7963eda86c71d8cc475cb4b1af70b29a81eef76b4a06af26fc806d1c302e5c6
```

Known 0-worker long-sample passing result from 2026-07-27:

```text
evidence_dir=/tmp/qc_clean_long_20260727_033600_evidence
windows_evidence_dir=results\network\2026-07-27-contest-script-dual-guest-qcz1-ai-clean-long-pass
analysis_result=PASS
QC_LINUX_STRESS_CONFIG_WORKERS=0
QC_LINUX_STRESS_CONFIG_SECONDS=0
QC_LINUX_STRESS_RESULT=SKIP
QC_RT_PERIOD_SAMPLES=10000
QC_RT_PERIOD_NS=1000000
QC_RT_LATENCY_MIN_NS=120000
QC_RT_LATENCY_MEAN_NS=859358
QC_RT_LATENCY_P50_NS=764352
QC_RT_LATENCY_P95_NS=1638128
QC_RT_LATENCY_P99_NS=2788848
QC_RT_LATENCY_MAX_NS=12764288
QC_RT_PERIODIC_RESULT=PASS
QC_RTOS_PERIOD_SAMPLES=1000
QC_RTOS_PERIOD_NS=1000000
QC_RTOS_PERIODIC_METHOD=busy_wait
QC_RTOS_LATENCY_MIN_NS=0
QC_RTOS_LATENCY_MEAN_NS=87811
QC_RTOS_LATENCY_P50_NS=30576
QC_RTOS_LATENCY_P95_NS=316336
QC_RTOS_LATENCY_P99_NS=613216
QC_RTOS_LATENCY_MAX_NS=5328816
QC_RTOS_PERIODIC_RESULT=PASS
QC_UDP_REQUESTS=20
QC_UDP_SUCCESSES=20
QC_UDP_FAILURES=0
QC_UDP_RTT_MEAN_US=7113
QC_UDP_RTT_MAX_US=50686
QC_QCZ1_RELIABLE_REQUESTS=10
QC_QCZ1_RELIABLE_SUCCESSES=10
QC_QCZ1_RELIABLE_FAILURES=0
QC_QCZ1_DUPLICATE_ACKS=2
QC_QCZ1_RETRANSMITS=0
QC_QCZ1_LATENCY_MEAN_US=3200
QC_QCZ1_LATENCY_MAX_US=14983
QC_AI_REQUESTS=10
QC_AI_SUCCESSES=10
QC_AI_FAILURES=0
QC_AI_INFER_MEAN_US=60
QC_AI_E2E_MEAN_US=1668
QC_AI_E2E_MAX_US=1925
QC_AI_CONTROL_ERROR_MEAN=207
QC_DUAL_GUEST_LINUX_INIT=PASS
tcpdump_packets_captured=88
tcpdump_packets_dropped_by_kernel=0
result=PASS
evidence_tar_sha256=b3a6dcc0503f7d2fae4add93c05c20aaad0a33874ac924bf1b9b26b9a7295ddd
```

Known 1-worker long/stress passing result from 2026-07-27:

```text
evidence_dir=/tmp/qc_stress_long_20260727_030428_evidence
windows_evidence_dir=results\network\2026-07-27-contest-script-dual-guest-qcz1-ai-stress-long-pass
analysis_result=PASS
QC_LINUX_STRESS_CONFIG_WORKERS=1
QC_LINUX_STRESS_CONFIG_SECONDS=0
QC_LINUX_STRESS_RESULT=STARTED/STOPPED
QC_RT_PERIOD_SAMPLES=10000
QC_RT_PERIOD_NS=1000000
QC_RT_LATENCY_MIN_NS=117968
QC_RT_LATENCY_MEAN_NS=827911
QC_RT_LATENCY_P50_NS=776848
QC_RT_LATENCY_P95_NS=1670256
QC_RT_LATENCY_P99_NS=2559424
QC_RT_LATENCY_MAX_NS=9586112
QC_RT_PERIODIC_RESULT=PASS
QC_RTOS_PERIOD_SAMPLES=1000
QC_RTOS_PERIOD_NS=1000000
QC_RTOS_PERIODIC_METHOD=busy_wait
QC_RTOS_LATENCY_MIN_NS=0
QC_RTOS_LATENCY_MEAN_NS=122604
QC_RTOS_LATENCY_P50_NS=25264
QC_RTOS_LATENCY_P95_NS=481440
QC_RTOS_LATENCY_P99_NS=1227840
QC_RTOS_LATENCY_MAX_NS=4352208
QC_RTOS_PERIODIC_RESULT=PASS
QC_UDP_REQUESTS=20
QC_UDP_SUCCESSES=20
QC_UDP_FAILURES=0
QC_UDP_RTT_MEAN_US=5483
QC_UDP_RTT_MAX_US=43549
QC_QCZ1_RELIABLE_REQUESTS=10
QC_QCZ1_RELIABLE_SUCCESSES=10
QC_QCZ1_RELIABLE_FAILURES=0
QC_QCZ1_DUPLICATE_ACKS=2
QC_QCZ1_RETRANSMITS=0
QC_QCZ1_LATENCY_MEAN_US=2411
QC_QCZ1_LATENCY_MAX_US=8712
QC_AI_REQUESTS=10
QC_AI_SUCCESSES=10
QC_AI_FAILURES=0
QC_AI_INFER_MEAN_US=56
QC_AI_E2E_MEAN_US=1996
QC_AI_E2E_MAX_US=5333
QC_AI_CONTROL_ERROR_MEAN=207
QC_DUAL_GUEST_LINUX_INIT=PASS
tcpdump_packets_captured=88
tcpdump_packets_dropped_by_kernel=0
result=PASS
evidence_tar_sha256=d4300613f3835c71f029e656d7dd209b84fbac25333a0d8378e7f3b72db29d0b
```

Known 2-worker long/stress passing result from 2026-07-27:

```text
evidence_dir=/tmp/qc_stress2_long_20260727_033000_evidence
windows_evidence_dir=results\network\2026-07-27-contest-script-dual-guest-qcz1-ai-stress2-long-pass
analysis_result=PASS
QC_LINUX_STRESS_CONFIG_WORKERS=2
QC_LINUX_STRESS_CONFIG_SECONDS=0
QC_LINUX_STRESS_RESULT=STARTED/STOPPED
QC_RT_PERIOD_SAMPLES=10000
QC_RT_PERIOD_NS=1000000
QC_RT_LATENCY_MIN_NS=120288
QC_RT_LATENCY_MEAN_NS=894770
QC_RT_LATENCY_P50_NS=753872
QC_RT_LATENCY_P95_NS=1880336
QC_RT_LATENCY_P99_NS=4346944
QC_RT_LATENCY_MAX_NS=9145360
QC_RT_PERIODIC_RESULT=PASS
QC_RTOS_PERIOD_SAMPLES=1000
QC_RTOS_PERIOD_NS=1000000
QC_RTOS_PERIODIC_METHOD=busy_wait
QC_RTOS_LATENCY_MIN_NS=0
QC_RTOS_LATENCY_MEAN_NS=98703
QC_RTOS_LATENCY_P50_NS=44256
QC_RTOS_LATENCY_P95_NS=360480
QC_RTOS_LATENCY_P99_NS=726688
QC_RTOS_LATENCY_MAX_NS=3676896
QC_RTOS_PERIODIC_RESULT=PASS
QC_UDP_REQUESTS=20
QC_UDP_SUCCESSES=20
QC_UDP_FAILURES=0
QC_UDP_RTT_MEAN_US=6673
QC_UDP_RTT_MAX_US=49705
QC_QCZ1_RELIABLE_REQUESTS=10
QC_QCZ1_RELIABLE_SUCCESSES=10
QC_QCZ1_RELIABLE_FAILURES=0
QC_QCZ1_DUPLICATE_ACKS=2
QC_QCZ1_RETRANSMITS=0
QC_QCZ1_LATENCY_MEAN_US=3593
QC_QCZ1_LATENCY_MAX_US=12514
QC_AI_REQUESTS=10
QC_AI_SUCCESSES=10
QC_AI_FAILURES=0
QC_AI_INFER_MEAN_US=56
QC_AI_E2E_MEAN_US=4964
QC_AI_E2E_MAX_US=21059
QC_AI_CONTROL_ERROR_MEAN=207
QC_DUAL_GUEST_LINUX_INIT=PASS
tcpdump_packets_captured=88
tcpdump_packets_dropped_by_kernel=0
result=PASS
evidence_tar_sha256=69adb1c9741b33b4a5f718096f5e26c457ddbc450fa96168c15aa5dd86599cfa
```

Known 2-worker 3-run stability result from 2026-07-27:

```text
remote_root=/tmp/qc_multirun_stress2_20260727_035234
windows_evidence_dir=results\network\2026-07-27-contest-script-dual-guest-qcz1-ai-stress2-3x-pass
rounds=3
round1_result=PASS
round1_evidence_sha256=fbe83e24d41cc3cc1c9172656de3212e4e044625c0837f6ba7c8ec3f941ddb26
round2_result=PASS
round2_evidence_sha256=6437349e481dd3b5282abe27a34085e4a0d26b214cd7a88478ff7532446f7a16
round3_result=PASS
round3_evidence_sha256=38aac4038f06ae1731125cea46e6afce0b18d0cc5f0845ef7a562676a8cc97f5
badscan_empty=round1,round2,round3
udp_success=20/20_each_round
qcz1_success=10/10_each_round
ai_success=10/10_each_round
tcpdump_kernel_drops=0_each_round
linux_periodic_p99_ns_min_mean_max=4300992/9204864/16139568
rtos_periodic_p99_ns_min_mean_max=864272/927499/984736
ai_e2e_max_us_min_mean_max=2230/11160/24792
```

The compact table copied into this source package is `results/stability/2026-07-27-stress2-3x/stability-summary.md`. The raw evidence archives remain in the Windows evidence directory and are referenced by SHA256 rather than committed into the source tree.
