# ATK-DLRK3588 sustained real-YOLO Task 1 A/B

This directory contains physical-board evidence collected on the real
ATK-DLRK3588 (RK3588). Every formal run was booted from RAM with
`scripts/board/atk-dlrk3588-ram-boot.sh`; no image was flashed to eMMC.

The formal alternating order was:

```text
RR-01 -> FP-RR-01 -> RR-02 -> FP-RR-02 -> RR-03 -> FP-RR-03
```

All six accepted runs contain 18,000 contiguous RT-Thread 10 ms periodic
samples and at least 50 real YOLO11n/ncnn inferences. The strict analyzer
rejects incomplete sample sequences, insufficient sustained overlap, console
output drops, panic, ESR_EL2, fatal IRQ, and runner errors.

The median per-run RTOS P99 jitter changed from 39.527 ms under RR to
0.574 ms under FP-RR, a 98.548% reduction. This is a virtualized Guest result,
not a native-RTOS bound. FP-RR also produced severe YOLO inference tail
latency: the median per-run YOLO P99 was 79.425 s versus 2.787 s under RR.
The result therefore demonstrates a scheduling trade-off, not an across-the-
board performance improvement.

Sources of truth:

- `analysis/periodic-summary.csv`: every accepted run
- `analysis/scheduler-medians.csv`: three-run scheduler medians
- `analysis/SUMMARY.md`: compact report
- `logs/`: raw formal UART logs and runner metadata
- `manifests/`: artifact and initramfs payload hashes
- `configs/`: scheduler, Guest, and FIT build configurations
- `diagnostics/`: rejected harness attempts, excluded from all statistics

The only intentional A/B variable was the AxVisor scheduler. The StarryOS and
RT-Thread Guest payloads, real YOLO model, input image, controller, periodic
probe, and their hashes were held constant.
