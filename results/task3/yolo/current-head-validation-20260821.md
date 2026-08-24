# Task3 YOLO current-HEAD validation (2026-08-21)

This record contains the current `openrace/task1-rt-partition` HEAD validation.
All measurements are QEMU AArch64 software-in-the-loop evidence; the fixture
replay adapter is not an in-Guest ONNX runtime and its `infer_us` values are
not physical YOLO inference performance.

## Contract and fixture gates

```bash
bash scripts/test/net-dual-guest/run-ci-regression.sh
python3 scripts/task3/run_yolo_fixture.py \
  --model tmp/task3-yolo/yolo11n.onnx \
  --out-dir /tmp/task3-yolo-current-head
```

The protocol/model gate passed: 21 Rust protocol tests, 14 Python network tests,
CNN compile check, YOLO compile check, and `TASK2_CI_GATE_PASS`. The YOLO11n
artifact SHA-256 is
`634279b40c07c6391472c51ad45b81ebc48706a9a1fe72dd3396322acd0c053b`.
The fixture covers no detection, target `419`, and a step-limited target `600`.

## Zephyr build inputs

The managed Guest was built with Zephyr 4.4.2 and SDK 1.0.1:

```bash
OUT_DIR=tmp/net-dual-guest/zephyr-task2-sdk101f \
ZEPHYR_BASE=/tmp/zephyrproject/zephyr-4.4.2 \
ZEPHYR_SDK_INSTALL_DIR=/tmp/zephyr-sdk-1.0.1 \
OBJCOPY=/tmp/zephyr-sdk-1.0.1/gnu/aarch64-zephyr-elf/bin/aarch64-zephyr-elf-objcopy \
READELF=/tmp/zephyr-sdk-1.0.1/gnu/aarch64-zephyr-elf/bin/aarch64-zephyr-elf-readelf \
TASK2_ZEPHYR_VIRTIO_SLOT=0 \
scripts/test/net-dual-guest/build-zephyr-task2.sh
```

The resulting raw image SHA-256 is
`49ca61bc847835e61a03f94fb619fca01b49f02157d347b0fc0a9806f7fcb433`.
Zephyr commit: `dccb09599635bdff17633fa7e9dab014b91dce90`.

## Current HEAD dual-Guest YOLO run

The reproducible run command is:

```bash
MIN_ELAPSED_MS=12000 \
  bash scripts/task3/run-task3-switch.sh current-head-yolo-capture ai
```

Evidence: `results/task3/switch/current-head-yolo-capture/`.

| Observation | Result |
|---|---:|
| `TASK3_MODEL_READY` | 1 |
| `TASK3_INFER` / `TASK3_CONTROL_SENT` | 74 / 74 |
| `TASK3_DETECTION` | 49 |
| `TASK3_MODEL_REJECTED` (no detection → hold-last-target) | 25 |
| `TASK3_STATUS_RECEIVED` | 73 |
| Linux-side pcap frames | 330 |
| RTOS-side pcap frames | 330 |
| T2N1 UDP frames per pcap | 324 |

Both pcaps pass:

```bash
python3 scripts/test/net-dual-guest/verify_pcap.py \
  --tag '' --require-task2 \
  results/task3/switch/current-head-yolo-capture/linux.pcap \
  results/task3/switch/current-head-yolo-capture/rtos.pcap
```

The run reached `elapsed_ms=12079`, exited through QMP, and produced matching
directed T2N1 ledgers. Pcap SHA-256 values are recorded in the run manifest.

## Previous blocked attempt

The earlier `current-head-yolo` attempt failed before Guest boot because
`tmp/net-dual-guest/zephyr-task2/zephyr-task2.bin` was absent. That failure is
retained as archaeology only; it is superseded by the successful run above.

## Remaining YOLO gap

The normal YOLO control loop and a separate YOLO-mode blackout → Safe → recovery
run are now proven through the T2N1 path. The fault runner requires marker order,
post-recovery control/status activity, non-empty dual captures, and the pcap
ledger verifier before archiving evidence.

Reproduce the fault run with:

```bash
bash scripts/task3/run-task3-switch-fault.sh current-head-yolo-fault-validated
```

Evidence: `results/task3/switch/fault-current-head-yolo-fault-validated/`.

| Fault observation | Result |
|---|---:|
| blackout marker | `virtnet: blackout ON` → `virtnet: blackout OFF` |
| Safe transition | `TASK2_SAFE` after blackout |
| recovery | `TASK2_RECOVERED state=Active elapsed_ms=40586` |
| YOLO control before/after recovery | 156 / 33 `TASK3_CONTROL_SENT` markers |
| status before/after recovery | present on both sides of recovery |
| dual pcap | 812 packets each; 806 T2N1 frames each |
| verifier | `verify_pcap.py --require-task2`: PASS |
| final elapsed window | `45396 ms` |

The result is still QEMU AArch64 SIL evidence. The adapter is
`embedded:fixture-replay`, so this proves model-contract and network recovery
integration, not in-Guest ONNX runtime performance.
