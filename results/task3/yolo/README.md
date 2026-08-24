# Task-3 YOLO perception fixture

This directory records the first reproducible YOLO supplement for Task-3.  It
does not replace the validated no-std temporal CNN in the AArch64 Guest.  The
current Guest image has no ONNX runtime; the fixture runs YOLO11n on the Linux
side/host and verifies the same bounded detection-to-target contract that a
future Guest or NPU adapter must use.

## Reproduce

The model is downloaded only when absent and is accepted only with the pinned
SHA-256 hash:

```bash
python3 scripts/task3/run_yolo_fixture.py \
  --model tmp/task3-yolo/yolo11n.onnx \
  --out-dir results/task3/yolo
```

The script uses the three checked-in fixtures under
`apps/starry/aka00-tennis-yolo/validation/`.  It requires `onnxruntime`,
Pillow, and NumPy in the host environment; missing dependencies fail loudly.

## Current result

`yolo-fixture-manifest.json` records the model URL/hash, input/output shape,
fixture hashes, normalized detection fields, and the bounded decision:

- `tennis-ball-close.jpg`: no detection, rejected safely;
- `tennis-ball-plant.jpg`: class 75, confidence `832/1000`, target `419`;
- `tennis-ball-black-box.jpg`: class 58, confidence `871/1000`, target clamped
  to `600` by the maximum single-frame step.

The generic COCO model's class IDs are reported verbatim.  They are not
relabeled as “tennis ball”; this keeps the fixture honest and leaves
class-specific training/model selection as a later task.

The current final branch validation, Linux endpoint/initramfs hashes, and the
missing-Zephyr-image QEMU blocker are recorded in
`current-head-validation-20260821.md` and
`current-head-build-manifest.toml`. The blocked QEMU attempt is not runtime
evidence; it is retained so the missing prerequisite and exact resume command
remain auditable.

## Boundary

The official Task-3 Guest comparison remains `cnn` versus the frozen P baseline.
This fixture proves the YOLO artifact, post-processing, hash pinning, and
bounded target contract.  It is not yet an in-Guest YOLO runtime or a claim of
hard-real-time inference.
