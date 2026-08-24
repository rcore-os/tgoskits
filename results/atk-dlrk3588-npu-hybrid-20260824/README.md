# RK3588 NPU hybrid-topology evidence

These are compact derived results from the physical ATK-DLRK3588 runs on the
`explore/atk-dlrk3588-npu-hybrid` implementation before its semantic migration
to the submission branch. Large FIT images, initramfs archives and raw UART
logs are deliberately not stored in Git. The checked-in runners and strict
analyzers reproduce the measurements and reject incomplete or interleaved
captures.

## Frozen topology

- StarryOS vCPU0 and Zephyr vCPU0 share pCPU1 at priorities 89 and 90.
- StarryOS vCPU1 uses pCPU2 for preprocessing, RKNN submission and
  postprocessing.
- YOLOv8 tensor inference runs on the RK3588 NPU assigned only to StarryOS.
- Zephyr executes the 10 ms periodic task and all T2N1 control actions.

## Task 1: 30,000 samples per cell

All four cells used the same Zephyr binary with the corrected RK3588 24 MHz
counter. Each capture contained exactly 30,000 contiguous 10 ms samples, no
UART output in the sampling window, and no repaired CSV lines. Stress cells
each contained 1,407 complete CONTROL/STATUS exchanges from the live
RKNN/T2N1/RTOS loop.

Under stress, FP-RR reduced mean wake-up jitter from 1.497 ms to 0.500 ms,
P99 from 9.656 ms to 4.091 ms, and maximum jitter from 17.803 ms to 7.252 ms.
Deadline misses above 10 ms fell from 247 to zero. The full compact matrix is
in `task1-summary.csv`.

The first attempted long run accidentally retained QEMU's 62.5 MHz timer
setting and is invalid. The checked-in board builder now supplies 24 MHz
explicitly, while QEMU leaves its platform frequency unchanged.

## Task 3: fixed perception versus RKNN/NPU, 3 + 3

Each arm ran the same 12-event sampled-video scene three times under the same
FP-RR hybrid topology, Zephyr image and T2N1 protocol. Fixed perception made
8/12 correct high-level actions in every run; it could not recognize the
hazard. RKNN made 12/12 correct actions in every run, with 100% vehicle recall
and 100% hazard recall. All 72 sent CONTROL messages received matching STATUS
responses.

Across the three RKNN runs, mean full model inference was 46.389 +/- 0.091 ms
and mean inference-start-to-STATUS latency was 273.215 +/- 2.544 ms (mean and
sample standard deviation of the three run means). The wider end-to-end value
includes image handling, atomic result publication and polling, guest
scheduling, T2N1 delivery, RTOS execution and STATUS return; it is not the NPU
kernel time alone. See `task3-3x3-summary.csv`.

The comparison demonstrates two separate benefits: NPU YOLO improves
perception and safety-decision correctness, while FP-RR improves RTOS timing
determinism under the resulting AI/communication workload. It does not claim
that the scheduler accelerates tensor computation.

## Reproduction entry points

```bash
# Build fixed and RKNN controllers with the compile-time scene mode.
TASK3_CONTROL_LOOP=1 TASK3_MODEL=fixed-perception \
  TASK3_RKNN_CONTROL_PATH=/rknn-control.txt \
  OUT_DIR=tmp/hybrid-fixed scripts/test/net-dual-guest/build-linux-task2.sh

TASK3_CONTROL_LOOP=1 TASK3_MODEL=rknn \
  TASK3_RKNN_CONTROL_PATH=/rknn-control.txt \
  OUT_DIR=tmp/hybrid-rknn scripts/test/net-dual-guest/build-linux-task2.sh

# Assemble raw cpio payloads. RKNN_BUNDLE is external because the model,
# runtime and glibc deployment libraries are not repository source files.
BUSYBOX_STATIC=/path/to/aarch64/busybox \
TASK2_BINARY=tmp/hybrid-fixed/controller/task2-net \
  scripts/task3/build-hybrid-scene-payload.sh fixed tmp/hybrid-fixed.cpio

BUSYBOX_STATIC=/path/to/aarch64/busybox \
TASK2_BINARY=tmp/hybrid-rknn/controller/task2-net \
RKNN_BUNDLE=/path/to/staged/rknn \
  scripts/task3/build-hybrid-scene-payload.sh rknn tmp/hybrid-rknn.cpio

# Feed either cpio to the portable board build.
STARRY_INITRD=tmp/hybrid-rknn.cpio \
  scripts/board/build-atk-zephyr-task123-unified.sh tmp/hybrid-board
```

Board boot remains RAM-only through `scripts/board/atk-dlrk3588-ram-boot.sh`;
these workflows never flash or erase persistent storage.
