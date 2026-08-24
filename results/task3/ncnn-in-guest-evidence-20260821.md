# Task 3 YOLO ncnn Guest evidence

This branch uses the same execution shape as the existing CNN path:

```text
Linux Guest: ncnn loads yolo11n.ncnn.param/.bin and input.ppm
    -> normalized detection
    -> task3-model::perception safety contract
    -> T2N1 CONTROL
    -> RTOS Guest
```

`TASK3_MODEL=yolo` no longer selects fixture replay. The fixture adapter remains
only in `task3-model` tests. The Linux initramfs builder installs the model
assets under `/usr/share/task3-yolo/`.

## Reproducible checks

```bash
scripts/task3/run-ncnn-smoke.sh
TASK3_CONTROL_LOOP=1 TASK3_MODEL=yolo \
  TASK3_MODEL_PATH=/usr/share/task3-yolo \
  scripts/test/net-dual-guest/build-linux-task2.sh
TASK3_MODEL=yolo scripts/test/net-dual-guest/build-linux-initramfs.sh
```

The AArch64 QEMU smoke produced:

```text
TASK3_NCNN_READY status=0
TASK3_NCNN_INFER infer_us=13230772
TASK3_NCNN_DETECTION class=75 confidence_milli=843 center_x_milli=421 area_milli=63
```

Asset SHA-256 values:

```text
d2c0adf8939dc9ce02964ce8ada104447768ffd8e3bffad8fa11e2e61e709c1f  yolo11n.ncnn.param
0ae562447923999779b12b4f91f96b9ef263add8c9902d10e22e6dd6a2932c12  yolo11n.ncnn.bin
608c8a61ff0bb43e5a8613f1f6f8aa08af74b084363610ed2b526ad925e4cb6f  input.ppm
```

The smoke validates real AArch64 ncnn execution. A full dual-Guest packet
capture still remains the final integration gate.
