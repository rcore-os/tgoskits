# Design Document

This document is the reviewer-facing design entry point for the redcola
Quancheng Lab 2026 AxVisor contest artifact. Detailed protocol, topology,
reproduction and patch-risk notes are linked from the sections below.

## Goal

The demo builds an intelligent industrial-control style mixed system on
AxVisor:

```text
Linux guest
  -> AI inference and control client
  -> IPv4/UDP QCZ1 protocol
  -> Zephyr RTOS guest
  -> control-state update and status feedback
```

The same integrated run also collects Linux and RTOS periodic timing evidence
while the communication and AI-control paths are active.

## System Architecture

```text
Kali host
  cargo xtask axvisor qemu
  per-run isolated bridge, recorded as bridge= in bridge.txt
    per-run Linux TAP, recorded as tap_linux= -> Linux guest virtio-net
    per-run RTOS TAP, recorded as tap_rtos=  -> Zephyr RTOS guest e1000

Linux guest
  2 vCPU, pCPU 1-2
  IPv4 192.0.2.10/24
  MAC  52:54:00:12:34:10
  static AArch64 probes injected into Alpine rootfs

Zephyr RTOS guest
  1 vCPU, pCPU 0
  IPv4 192.0.2.20/24
  MAC  52:54:00:12:34:20
  UDP port 4242
  e1000 device
```

The Linux guest acts as the control client and AI inference side. The RTOS guest
acts as the UDP endpoint, QCZ1 state machine and observable control actuator.

See `docs/network-topology.md` for MAC/IP/port, bridge/TAP and access-boundary
details.

## Guest Configuration

Linux guest:

- `2` vCPU online in the passing run.
- vCPU placement: pCPU `1-2`.
- virtio-net on the dual-guest bridge.
- PL011 and virtio interrupt pass-through IDs: `[1, 31, 47]`.
- bootargs include `init=/qc-dual-net.sh noirqdebug`.
- rootfs injection uses `e2fsck -fy` before and after `debugfs` writes to avoid
  ext4 journal replay overwriting injected files.

RTOS guest:

- Zephyr RTOS on QEMU Cortex-A53/AArch64.
- e1000 network device.
- IPv4-only UDP configuration.
- TCP, IPv6 and Zephyr net shell disabled in the contest RTOS config.
- QCZ1 protocol implementation added to the UDP echo-server path.

See `docs/reproduce.md` for the exact build and run commands.

## AxVisor Modifications and Patch Boundary

The first-stage contest directory is intentionally separated from AxVisor core
changes. The current core work is split into four patch candidates:

- VM config and physical timer/vTimer support.
- GICv3 EOI mode adjustment for the Zephyr e1000 interrupt path.
- bounded diagnostics for exception/IRQ/vGICD debugging.
- axbuild image-helper improvements.

The recommended submission order is to first commit this contest artifact
directory, then review the core patches separately.

See `docs/core-patch-review.md` and `docs/pr-boundary.md`.

## Communication Protocol

The primary data channel is IPv4/UDP. The QCZ1 application protocol runs over
UDP and provides:

- magic and version fields;
- message type and header length;
- payload length;
- flags;
- sequence number;
- timestamp;
- checksum;
- error reporting.

The RTOS endpoint supports:

- `CONTROL_SET`;
- `ACK`;
- `STATE_REQ`;
- `STATUS`;
- `ERROR`.

UDP reliability is implemented at the application layer with ACK validation,
timeout/retry, retransmission accounting and duplicate-command suppression.
Plain UDP echo remains available as a smoke-test channel on the same RTOS port.

See `docs/protocol.md` for the frame format.

## Isolation Design

The integrated dual-guest run keeps the contest data path inside an isolated
per-run TAP/bridge network. In TAP mode, the runner generates unique host
object names from the current process, for example `qcb<PID>`, `qcl<PID>` and
`qcr<PID>`, records the exact names in `bridge.txt`, and removes only the
resources created by that run. The bridge has no NAT or routed uplink. The host
SSH/control path is separate from the guest data path.

The Linux guest includes a `gppt-gicd` device mapping so Linux GIC distributor
accesses do not disturb the Zephyr/e1000 interrupt route. The RTOS guest uses a
separate vCPU placement and a dedicated e1000 endpoint.

## AI Model and Deployment

The AI controller runs inside the Linux guest as a small fixed-weight neural
network implemented in the static guest demo program. It uses deterministic
sample inputs:

```text
error_milli
velocity_milli
load_milli
```

The model outputs `ai_score_milli`, which is sent to the RTOS guest in a QCZ1
`CONTROL_SET` frame together with `setpoint_milli` and `client_sample_id`.

The RTOS guest applies:

```text
output_milli = setpoint_milli * ai_score_milli / 1000
```

The manual comparison baseline uses fixed gain `manual_score_milli = 800`.
Control quality is reported as mean absolute control error, and timing is
reported as inference and end-to-end Linux/RTOS control latency.

See `docs/ai-control-evaluation.md`.

## Reproducibility

Primary integrated command after preparing the runtime artifacts listed in `docs/reproduce.md`:

```bash
REPO=/path/to/tgoskits
cd "${REPO}/os/axvisor/contest/quancheng2026"
./scripts/run_axvisor_dual_guest_qcz1_ai.sh
```

Primary native RTOS baseline command:

```bash
REPO=/path/to/tgoskits
cd "${REPO}/os/axvisor/contest/quancheng2026"
./scripts/run_native_zephyr_latency_baseline.sh
```

The integrated script prints `result=PASS` only when Linux guest, RTOS guest,
plain UDP, QCZ1 reliable UDP, AI control, Linux periodic probe, RTOS periodic
probe and tcpdump markers are present.
