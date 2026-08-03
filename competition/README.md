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

The same neural profile now also boots in StarryOS on a physical Orange Pi 5
Plus from a WSL2 automation host. One retained full hardware run completed
1,800/1,800 commands with zero controller error, timeout, retransmission, or
recovery. A retained 20-command smoke repeats the lifecycle. Both prove two
online StarryOS vCPUs, power down StarryOS and Zephyr, synchronize the AxVisor
host filesystem, restore the board's Linux TF-card system, and pass the strict
board-log analyzer. The raw logs, summaries, and hashes are under
[`results/orangepi-starry-reference`](results/orangepi-starry-reference/).

The evidence supports deterministic vCPU placement and selected dispatch-tail
improvements, not universal latency improvement, bounded guest preemption, or
a hardware real-time guarantee. The actual demonstration video remains to be
recorded. Upstream push, dev-target conflict checking, and PR creation are not
claimed by this local Windows/WSL synchronization. See
[test-report.md](test-report.md) for the exact claims and limitations.

## Requirement status

| Competition deliverable | Repository status |
| --- | --- |
| Task 1: AxVisor real-time changes, two-vCPU Linux, idle/stress/soak | Implemented and retained under [`results/axvisor-rt-reference`](results/axvisor-rt-reference/) |
| Task 1: native RTOS comparison | Implemented with Zephyr v4.3.0 under [`results/native-zephyr-reference`](results/native-zephyr-reference/) |
| Task 2: bidirectional IP link and versioned reliable protocol | Implemented in QEMU Linux/Zephyr and physical StarryOS/Zephyr; normal and ACK-loss QEMU runs plus physical normal runs are retained |
| Task 3: neural inference, RTOS action/feedback, manual comparison | Implemented; the physical StarryOS neural result reproduces the QEMU neural control metrics, while the Linux QEMU manual comparison shows two error metrics improve and overshoot regresses |
| Physical Orange Pi 5 Plus lifecycle | Retained full 1,800-command and 20-command StarryOS/Zephyr profiles pass strict analysis and automatic Linux restore |
| Design, test, and reproduction documents | Present in this directory |
| Approximately five-minute video | Storyboard present; actual recording outstanding |
| Source PR to `dev` | Outstanding; no upstream submission is claimed here |

The physical closed-loop profile uses StarryOS instead of Linux for Tasks 2 and
3 and demonstrates the StarryOS replacement path. The retained Task 1
idle/stress/soak campaign and manual-policy comparison remain Linux/QEMU
evidence, so this report does not claim that every scoring sub-item was rerun
under StarryOS. It does not claim the multi-RTOS/multi-board bonus. The
competition code remains under the repository's Apache-2.0 license.

## Documents

- [design.md](design.md) defines the architecture, guest resources, isolation
  policy, wire protocol, reliability rules, control loop, and known limits.
- [reproduce.md](reproduce.md) gives pinned build, validation, image, and QEMU
  commands and explains retained provenance and remaining formal deliverables.
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
| Linux guest image/init | [`ivc/linux`](ivc/linux/) |
| StarryOS physical guest image/init | [`ivc/starry`](ivc/starry/) |
| Zephyr endpoint | [`ivc/zephyr`](ivc/zephyr/) |
| AxVisor/QEMU/Orange Pi guest configuration | [`ivc/config`](ivc/config/) |
| Orange Pi artifact staging and run entry points | [`stage-orangepi-5-plus.sh`](ivc/stage-orangepi-5-plus.sh), [`run-orangepi-5-plus.sh`](ivc/run-orangepi-5-plus.sh) |
| Real-time benchmark harness | [`scripts/benchmark/axvisor-rt`](../scripts/benchmark/axvisor-rt/) |
| Retained AxVisor, IVC, host, and native-RTOS evidence | [`results`](results/) |
| Cross-guest log validators | [`analyze_qemu.py`](ivc/analyze_qemu.py), [`analyze_board.py`](ivc/analyze_board.py) |

## Demonstration contract

The retained QEMU full run uses four emulated host CPUs:

```text
pCPU 0: Zephyr vCPU 0          pCPU 1: Linux vCPU 0 (dedicated)
pCPU 3: excluded from guests   pCPU 2: Linux vCPU 1 (dedicated)

Linux 10.0.0.1/24 -- virtio-net -- AxVisor segment 1 -- virtio-net -- 10.0.0.2/24 Zephyr
                     UDP CONTROL -> STATUS + ACK
```

Linux performs the checked-in `thermal-4x6x1-v1` inference and sends the
actuator command over UDP. Zephyr applies a fresh command once, steps the
deterministic thermal plant, and returns status. Duplicate packets do not
repeat the control side effect, and command silence drives the endpoint to a
zero-actuator safe state.

There is no host-facing NIC, bridge, NAT rule, default route, vsock data path,
shared-memory data path, or hypercall data path in this profile.

pCPU3 is excluded from guest affinity masks for intended AxVisor
housekeeping; the implementation does not prove that every host task or
physical interrupt is pinned there.

The Orange Pi profile keeps the same three-vCPU partition but replaces the
outer QEMU machine with RK3588 hardware and the Linux controller with
StarryOS. StarryOS receives a minimal guest DTB for the emulated GICv3, timer,
PL011, virtio block, and virtio network devices. Its two vCPUs remain dedicated
to pCPUs1/2 and run the same `ivcproto` Linux-ABI binary from a compact ext4
rootfs. Zephyr remains on pCPU0.

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
