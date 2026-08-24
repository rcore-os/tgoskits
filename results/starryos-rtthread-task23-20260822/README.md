# StarryOS + RT-Thread Task-2/Task-3 virtual evidence (2026-08-22)

RT-Thread endpoint commit:
`6ea682795bdbac59d3700b21e159ccaa3f7632cb`, BSP
`qemu-virt64-aarch64`, lwIP 2.0.3, toolchain
`aarch64-none-elf-gcc 10.2.1`. The T2N1 wire format and endpoint state
machine mirror the Zephyr endpoint; only the socket/clock/logging APIs are
RT-Thread-specific.

All seven scenarios passed the pcap and scenario verifiers (`PASS` in each
`verify-pcap.log` / `verify-scenario.log`), matching the Zephyr endpoint
coverage:

| Scenario | Result |
| --- | --- |
| normal | 3 control/status rounds with ACK/STATUS, `STARRY_T2N1_PASS` |
| drop-ack | seq=1 ACK dropped once, retransmit attempt=1, duplicate ACK recovered |
| retry-exhausted | ACK always dropped, attempts 1..5, `RetryExhausted` Safe, recovered |
| blackout | switch blackout, both sides Safe, recovery after `virtnet drop off` |
| out-of-order | controller sends seq=2 first, peer reports `OutOfOrder`, Safe, recovered |
| invalid-parameter | controller sends an invalid CONTROL, peer reports `InvalidParameter`, Safe, recovered |
| model-rejected | injected invalid YOLO output, `ModelRejected` Safe, heartbeats only |

Key timestamps from the guest logs:

- blackout: `STARRY_T2N1_SAFE elapsed_ms=6153`, recovered at 11602 ms,
  `FAULT_RECOVERY_COMPLETE elapsed_ms=38647`.
- retry-exhausted: `RETRY_EXHAUSTED elapsed_ms=3027`, recovered at 3158 ms.

Per-scenario image hashes:

| Variant | SHA-256 |
| --- | --- |
| none (normal/blackout) | `14363b8ce6ede9ef19ad76d8d2aeca3ab9084cbde616b1cdf74609bca570b548` |
| drop-ack-once | `91c71e35ca42e8cdf86144175dbec35ff81e620eddaee1763d7020dabc99e567` |
| drop-ack-always | `184d47bf425d70caa4b26a9a717ea83f7bbe2b1aa95ad7848f761a22a641564e` |

Each scenario directory contains `run.log`, `steps.txt`, both pcaps, the
RT-Thread manifest, and the verifier output. Physical-board validation is a
separate follow-up and is not claimed by this virtual evidence.

## Task 1 with RT-Thread companion (status: passing, FPU/SIMD fix)

The shared-pCPU1 Task 1 scheduler A/B with RT-Thread as the companion Guest
passes under the RR arm after fixing AxVisor's guest FPU/SIMD context
switching. Root cause: `Aarch64ContextFrame` did not save/restore the AArch64
FPU/SIMD registers on guest exit/entry, so on a shared physical CPU the
RT-Thread Guest's floating-point state polluted StarryOS's ncnn inference,
producing `RuntimeError code -7` or `NoDetection` (NaN confidence).

Fix: the trap frame now saves `Q0-Q31`/`FPSR`/`FPCR` on lower-EL guest exits
and restores them in the final assembly window before the guest `ERET`.

Verified with the shared-core RR scenario (`normal`):

- 3/3 ncnn/YOLO inferences completed; detections were
  `confidence_milli=843`, `center_x_milli=421`, `area_milli=63` for all three
  requests (no NaN / `NoDetection`).
- All three control/status rounds produced `ACK` and
  `STARRY_T2N1_STATUS_DELIVERED`; `STARRY_T2N1_PASS` observed.
- Dual-end pcaps contain 38 frames / 20 `T2N1` records; pcap and scenario
  verifiers both report `PASS`.

Evidence: `results/rtthread-task1-fpu-fix-normal-20260822/`.

The full shared-core Task 1 scheduler A/B (RR and FP-RR) also passes; both
arms complete the same T2N1/ncnn/YOLO workload with 3/3 inferences at
`confidence_milli=843`. The bounded FP-RR service path is exercised
(`lower_priority_services=217`), and the A/B verifier reports `PASS`.
Evidence: `results/rtthread-task1-ab-20260822/`. (The first FP-RR attempt hit
a one-off teardown-phase guest breakpoint during pcap dump after all
functional steps had passed; the arm was re-run cleanly and the rerun is the
archived evidence.)

A 300-sample, 10 ms periodic wake-up probe was also run for the RT-Thread
companion (RR vs bounded FP-RR, 3 pairs). The probe uses the AArch64 virtual
timer (CNTV), exposed to the RT-Thread guest through its GIC handler slot,
with relative-period deadlines (per-wake jitter, same definition as the
Zephyr probe). FP-RR reduces P99 wake-up jitter from ~39.7 ms to ~1.7 ms
(23.9x) while the StarryOS YOLO inference stays essentially unchanged
(median 22.3 s RR vs 22.9 s FP-RR). Evidence and caveats:
`results/starryos-task1-periodic-rtthread-20260822/`.

## Probe comparison: Zephyr versus RT-Thread

Both RTOS probes run the identical experiment (300 samples, 10 ms period,
three RR/FP-RR pairs, StarryOS YOLO on pCPU1). The scheduler improvement is
the same direction for both; the multiple differs because of the timer path.

| Metric (3-run median) | Zephyr RR → FP-RR | RT-Thread RR → FP-RR |
|---|---:|---:|
| P99 wake-up jitter | 12.498 → 0.646 ms (19.35x) | 39.737 → 1.662 ms (23.9x) |
| P99.9 / maximum | 12.883 → 0.671 ms | 43.051 → 1.803 ms |
| Mean wake-up jitter | 9.505 → 0.566 ms | 23.465 → 0.980 ms |
| Samples later than 1 ms | 300/300 → 0/300 | 300/300 → 62/300 |
| YOLO inference (median) | 20.8 → 21.2 s | 22.3 → 22.9 s |
| FP-RR lower-priority services | 70 (49-97) | 92 (87-93) |

Zephyr's probe uses the virtual timer (`CNTVCT_EL0`/`CNTV_CVAL_EL0`), which
AxVisor passes through in hardware, giving it a sub-millisecond FP-RR floor.
RT-Thread's periodic probe now uses the same virtual-timer path (its GIC
handler slot for INTID 27 is exposed in the periodic build), so its FP-RR
floor is ~1.7 ms, close to Zephyr. RT-Thread's RR baseline (~39.7 ms P99) is
about 3x Zephyr's because its thread resume under RR waits for a shared-vCPU
slice; FP-RR eliminates that wait.

The Zephyr Task 1 A/B continues to pass after the FPU/SIMD fix (RR and
FP-RR; FP-RR `lower_priority_services=196`), confirming no regression on the
first RTOS path. Evidence is in
`results/zephyr-task1-ab-fpu-regression-20260822/` (and the earlier
`results/starryos-task1-zephyr-regression-20260822/`).
