# AxVisor mixed-criticality control demonstration

This directory is the entry point for the competition implementation. The
retained QEMU campaigns combine a two-vCPU Linux controller and Zephyr; the
physical Orange Pi profile replaces Linux with a two-vCPU StarryOS controller.
Both use AxVisor, an isolated in-hypervisor Ethernet segment, a versioned
reliable UDP protocol, and the same deterministic neural thermal controller.

The implementation worktree now has retained, analyzer-validated evidence for
the complete neural/manual Linux-to-Zephyr loop, deterministic cross-guest ACK
loss and exact-once recovery, AxVisor shared/partitioned idle and CPU-stress
runs, a 300-second measured partitioned soak, and a native Zephyr idle/stress
baseline. Neural and manual each completed 1,800/1,800 commands with no
application error or timeout; the fault campaign recovered all 20 intentionally
dropped ACKs among 100 commands.

The physical evidence is now a repeated formal matrix rather than a single
demonstration run. On board `bf61f4d4a1d994ad`, StarryOS manual and native
neural control each completed five 1,800-command halves; ACK-loss, malformed
ERROR, and actual guest-restart recovery each completed three physical runs.
The same fixed, untrained 4x6x1 weights were then exported through ONNX to two
additional backends: RKNN Runtime on the RK3588 NPU and ONNX Runtime 1.25.0
`CPUExecutionProvider`. Each backend completed five cold-boot full runs and
9,000/9,000 ACK with zero application errors, timeouts, retransmissions, or
recoveries. Every run synchronized and snapshotted the AxVisor filesystem,
passed read-only fsck, and restored the board's Linux TF-card system.

The committed ORT evidence includes the v4 full archive plus immutable v1-v3
failure directories. The passing archive, with all raw/ORT CSVs, summaries,
manifests, and preregistration, starts at
[`results/orangepi-5-plus/ort-control-full-formal-20260805-v4`](results/orangepi-5-plus/ort-control-full-formal-20260805-v4/).
Earlier retained reference captures remain under
[`results/orangepi-starry-reference`](results/orangepi-starry-reference/) and
are kept separate from the clean formal campaigns.
The corresponding RKNN NPU full archive is
[`rknpu-control-full-formal-20260805-v8`](results/orangepi-5-plus/rknpu-control-full-formal-20260805-v8/).

The evidence supports deterministic vCPU placement and selected dispatch-tail
improvements, not universal latency improvement, bounded guest preemption, or
a hardware real-time guarantee. The actual demonstration video remains to be
recorded. Upstream push, dev-target conflict checking, and PR creation are not
claimed by this local Windows/WSL synchronization. See
[test-report.md](test-report.md) for the exact claims and limitations.

## Requirement status

| Competition deliverable | Repository status |
| --- | --- |
| Task 1: AxVisor real-time changes, two-vCPU guest, idle/stress/soak | Implemented in the historical QEMU/Linux matrix and repeated on physical StarryOS with controlled-interference pairs, two soak runs, and CPU1-stress pairs |
| Task 1: native RTOS comparison | Implemented with Zephyr v4.3.0 under [`results/native-zephyr-reference`](results/native-zephyr-reference/) |
| Task 2: bidirectional IP link and versioned reliable protocol | Complete on physical StarryOS/Zephyr; normal, ACK-loss, malformed-ERROR, and guest-restart campaigns are retained and strictly analyzed |
| Task 3: neural inference, RTOS action/feedback, manual comparison | Complete on the same physical board with five manual/native-neural pairs; RMSE/IAE improve, overshoot regresses, and latency direction is mixed |
| Fixed ONNX model and ORT CPU backend | Complete without training; 10,000-vector offline gate plus five physical full runs using ORT 1.25.0 `CPUExecutionProvider` |
| RK3588 NPU backend | Complete with RKNN Runtime 2.3.2/driver 0.9.8 and five physical full runs; evidence establishes hardware offload, not acceleration |
| Physical Orange Pi 5 Plus lifecycle | Native, RKNN, and ORT profiles pass clean-source gates, cold-boot collection, strict analysis, snapshot fsck, and automatic Linux restore |
| Design, test, and reproduction documents | Present in this directory |
| Approximately five-minute video | Storyboard present; actual recording outstanding |
| Source PR to `dev` | Outstanding; no upstream submission is claimed here |

The physical profiles use StarryOS instead of Linux for Tasks 1, 2, and 3.
Historical Linux/QEMU results remain qualified references rather than the sole
support for a scoring item. The project does not claim the multi-RTOS or
multi-board bonus. It also does not call the tiny-model NPU path an
acceleration: its observed full-loop p99 is higher than the ORT CPU comparison.
The competition code remains under the repository's Apache-2.0 license, while
vendor Runtime/toolkit redistribution boundaries are recorded separately.

## Documents

- [design.md](design.md) defines the architecture, guest resources, isolation
  policy, wire protocol, reliability rules, control loop, and known limits.
- [reproduce.md](reproduce.md) gives pinned build, validation, QEMU, WSL2 board,
  ORT campaign, independent aggregation, fsck, and Linux-restore commands.
- [test-report.md](test-report.md) separates retained measurements, host/unit
  checks, platform-qualified comparisons, and unclaimed evidence scopes.
- [video-storyboard.md](video-storyboard.md) is a recording checklist and
  approximately five-minute storyboard. It is not a substitute for a video.
- [requirement.md](requirement.md) is the original competition specification.

## Implementation map

| Area | Primary files |
| --- | --- |
| CPU partition validation and runtime placement | [`axvmconfig`](../virtualization/axvmconfig/src/partition.rs), [`axvm`](../virtualization/axvm/src/manager.rs) |
| Emulated virtio-net device | [`axdevice`](../virtualization/axdevice/src/virtio_net/mod.rs) |
| Isolated switch policy and integration | [`axvm-net`](../virtualization/axvm-net/src/lib.rs), [`axvm` network metrics](../virtualization/axvm/src/network.rs) |
| Shared Rust wire protocol and controller | [`ivcproto`](../tools/ivcproto/src/lib.rs) |
| Single-source weights, ONNX/ORT/RKNN conversion, and numeric gates | [`ivc/model`](ivc/model/) |
| Linux guest image/init | [`ivc/linux`](ivc/linux/) |
| StarryOS physical guest image/init | [`ivc/starry`](ivc/starry/) |
| Zephyr endpoint | [`ivc/zephyr`](ivc/zephyr/) |
| AxVisor/QEMU/Orange Pi guest configuration | [`ivc/config`](ivc/config/) |
| Orange Pi artifact staging and run entry points | [`stage-orangepi-5-plus.sh`](ivc/stage-orangepi-5-plus.sh), [`run-orangepi-5-plus.sh`](ivc/run-orangepi-5-plus.sh) |
| ORT five-run preregistration and aggregation | [`run-ort-control-campaign.sh`](ivc/run-ort-control-campaign.sh), [`aggregate_ort_campaign.py`](ivc/aggregate_ort_campaign.py) |
| RKNN raw/deadline/campaign verification | [`analyze_board.py`](ivc/analyze_board.py), [`rknpu_deadline.py`](ivc/rknpu_deadline.py), [`aggregate_rknpu_campaign.py`](ivc/aggregate_rknpu_campaign.py) |
| Real-time benchmark harness | [`scripts/benchmark/axvisor-rt`](../scripts/benchmark/axvisor-rt/) |
| Retained AxVisor, IVC, host, and native-RTOS evidence | [`results`](results/) |
| Cross-guest log validators | [`analyze_qemu.py`](ivc/analyze_qemu.py), [`analyze_board.py`](ivc/analyze_board.py) |
| Formal ORT evidence | [`ort-control-full-formal-20260805-v4`](results/orangepi-5-plus/ort-control-full-formal-20260805-v4/) |
| Formal RKNN NPU evidence | [`rknpu-control-full-formal-20260805-v8`](results/orangepi-5-plus/rknpu-control-full-formal-20260805-v8/) |

## Demonstration contract

The retained QEMU full run uses four emulated host CPUs:

```text
pCPU 0: Zephyr vCPU 0          pCPU 1: Linux vCPU 0 (dedicated)
pCPU 3: excluded from guests   pCPU 2: Linux vCPU 1 (dedicated)

Linux 10.0.0.1/24 -- virtio-net -- AxVisor segment 1 -- virtio-net -- 10.0.0.2/24 Zephyr
                     UDP CONTROL -> STATUS + ACK
```

The QEMU Linux guest performs the native checked-in `thermal-4x6x1-v1`
inference. The physical StarryOS guest can execute the same fixed model as
native Rust, ONNX Runtime CPU, or RKNN on the RK3588 NPU. Each backend sends
the same CONTROL payload contract over UDP. Zephyr applies a fresh command
once, steps the deterministic thermal plant, and returns status. Duplicate
packets do not repeat the control side effect, and command silence drives the
endpoint to a zero-actuator safe state.

There is no host-facing NIC, bridge, NAT rule, default route, vsock data path,
shared-memory data path, or hypercall data path in this profile.

pCPU3 is excluded from guest affinity masks for intended AxVisor
housekeeping; the implementation does not prove that every host task or
physical interrupt is pinned there.

The Orange Pi profile keeps the same three-vCPU partition but replaces the
outer QEMU machine with RK3588 hardware and the Linux controller with
StarryOS. StarryOS receives a minimal guest DTB for the emulated GICv3, timer,
PL011, virtio block, and virtio network devices. Its two vCPUs remain dedicated
to pCPUs1/2 and run the same `ivcproto` Linux-ABI binary from a finite ext4
rootfs. Zephyr remains on pCPU0. Only the RKNN profile additionally enables
AxVisor's NPU power/clock/reset handoff, maps the three NPU core MMIO windows,
and supplies identity DMA; ORT explicitly reports `CPUExecutionProvider` and
has no NPU passthrough.

## Quick host-only checks

These checks do not prove cross-guest operation, but they are fast regression
gates from the repository root:

```sh
cargo +nightly-2026-07-15 test -p ivcproto
cargo +nightly-2026-07-15 test -p axvm-net

bash competition/ivc/run-host-loopback.sh \
  tmp/competition/ivc/host-loopback

cargo +nightly-2026-07-15 run -p ivcproto -- \
  evaluate-csv tmp/competition/ivc/host-ai.csv
```

Do not present the host loopback latency as AxVisor or cross-guest latency. Do
not present the host plant comparison as an in-guest timing result.
