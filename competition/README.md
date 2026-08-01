# AxVisor mixed-criticality control demonstration

This directory is the entry point for the competition implementation. It
combines a two-vCPU Linux controller guest and a Zephyr RTOS control guest on
AxVisor, an isolated in-hypervisor Ethernet segment, a versioned reliable UDP
protocol, and a deterministic neural thermal controller.

The implementation worktree now has retained, analyzer-validated evidence for
the complete neural/manual Linux-to-Zephyr loop, deterministic cross-guest ACK
loss and exact-once recovery, AxVisor shared/partitioned idle and CPU-stress
runs, a 300-second measured partitioned soak, and a native Zephyr idle/stress
baseline. Neural and manual each completed 1,800/1,800 commands with no
application error or timeout; the fault campaign recovered all 20 intentionally
dropped ACKs among 100 commands.

The same neural profile now also boots on a physical Orange Pi 5 Plus from a
WSL2 automation host. One full hardware run completed 1,800/1,800 commands with
zero controller error, timeout, retransmission, or recovery, then powered down
both guests, synchronized the AxVisor host filesystem, and restored the board's
Linux TF-card system. A maintained 20-command smoke profile repeats that
lifecycle and asserts the Linux guest brought up two vCPUs.

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
| Task 2: bidirectional IP link and versioned reliable protocol | Implemented; normal and ACK-loss runs retained under [`results/axvisor-ivc-reference`](results/axvisor-ivc-reference/) |
| Task 3: Linux neural inference, RTOS action/feedback, manual comparison | Implemented; two error metrics improve while overshoot regresses |
| Physical Orange Pi 5 Plus lifecycle | Validated for full 1,800-command neural and maintained 20-command smoke profiles; automatic Linux restore passes |
| Design, test, and reproduction documents | Present in this directory |
| Approximately five-minute video | Storyboard present; actual recording outstanding |
| Source PR to `dev` | Outstanding; no upstream submission is claimed here |

This submission profile uses Linux plus one Zephyr RTOS baseline. It does not
claim either StarryOS bonus or a multi-RTOS/multi-board bonus. The competition
code remains under the repository's Apache-2.0 license.

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
| Zephyr endpoint | [`ivc/zephyr`](ivc/zephyr/) |
| AxVisor/QEMU/Orange Pi guest configuration | [`ivc/config`](ivc/config/) |
| Orange Pi artifact staging and run entry points | [`stage-orangepi-5-plus.sh`](ivc/stage-orangepi-5-plus.sh), [`run-orangepi-5-plus.sh`](ivc/run-orangepi-5-plus.sh) |
| Real-time benchmark harness | [`scripts/benchmark/axvisor-rt`](../scripts/benchmark/axvisor-rt/) |
| Retained AxVisor, IVC, host, and native-RTOS evidence | [`results`](results/) |
| Cross-guest log validator | [`analyze_qemu.py`](ivc/analyze_qemu.py) |

## Demonstration contract

The intended full run uses four emulated host CPUs:

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
outer QEMU machine with RK3588 hardware. Linux receives a minimal guest DTB
for the emulated GICv3, timer, PL011, and virtio-mmio network device; host CPU
idle-state nodes are removed because AxVisor does not implement PSCI
`CPU_SUSPEND`.

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
