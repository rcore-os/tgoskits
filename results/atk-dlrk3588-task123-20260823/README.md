# Task 1-3 final verification, 2026-08-23

## Final status

| Task | Status | Physical evidence |
|---|---|---|
| Task 1 | PASS for implementable functional/physical scope; one native baseline BLOCKED and one official-dev control NOT COMPARABLE | RR/FP-RR real YOLO A/B, 2-vCPU Linux, idle/stress, 600-second soak |
| Task 2 | PASS | dual-ended pcap, 97.1% ACK in short window, five retries to Safe, peer HeartbeatTimeout to Safe, recovery after blackout off |
| Task 3 | PASS | real in-guest ncnn/yolo11n, 137 matched inference/CONTROL/ACK/STATUS loops |

Task 1 must not be summarized as an unconditional all-controls PASS. The
native RTOS control cannot be run because the audited upstream trees lack an
ATK-DLRK3588/RK3588 bare-metal BSP. Unmodified official `dev` lacks the
scheduler pair, priority plumbing, instrumentation and equivalent workload,
so that control is not comparable. Both limitations have detailed evidence in
the Task 1 archive.

## Task 1 headline results

* Task 1 verifier: 6/6 PASS.
* Full `scripts/test/net-dual-guest`: 28/28 PASS.
* Six physical YOLO runs: every run 300/300.
* Median run P99: RR 30.176 ms, FP-RR 2.805 ms (10.76x / 90.7% lower).
* Linux idle/stress: 20,000 samples each, P99 206 us, zero overflow.
* Linux 600-second soak: 600,000 samples, P99 204 us, P99.9 212 us,
  max 486 us, zero overflow.
* Concurrent RT-Thread soak probe: 300/300, P99 499.125 us,
  max 532.958 us.
* No panic, ESR_EL2, IRQ 26 fatal or generic fatal marker in formal Task 1 logs.

## Repository evidence snapshot

This directory intentionally keeps the reviewable reports, configurations,
serial logs, pcaps, test output, and hashes. The generated raw/FIT images are
not committed because the complete local archive is about 714 MiB and the
images are reproducible build products. Their exact identities are pinned in
`artifact-hashes.txt`; `SHA256SUMS.txt` authenticates every file that is
committed here.

`atk-ram-boot-detach-regression-20260823.log` records the final reboot-flow
regression: the board began inside the RT-Thread VM2 console, the tool detached
to the AxVisor shell, issued the whole-board reboot, interrupted U-Boot, staged
the FIT in RAM, and reached `booti` without a physical Reset press.

The full local archives used for the final check were:

* `/home/huhu/atk-bringup/task1-board-evidence-20260823`
* `/home/huhu/atk-bringup/task23-fifo-evidence-20260823`

## Authenticated full archives

### Task 1

Path: `/home/huhu/atk-bringup/task1-board-evidence-20260823`

Files covered: 102 plus the manifest itself.

`MANIFEST.sha256` SHA256:
`e86a2d52b92920815a2470d84cd7ccc1266a6c90ba606836bab3fe55ef82dbb5`

### Task 2/3

Path: `/home/huhu/atk-bringup/task23-fifo-evidence-20260823`

`MANIFEST.sha256` SHA256:
`1e2d96ae95a87ae2978beff7285ca3998e7e897a63116f11daa8ceb338eea5ae`

Both full-archive manifests were rechecked successfully with `sha256sum -c`
after the final reports were written. For this repository snapshot, run
`sha256sum -c SHA256SUMS.txt` from this directory.
