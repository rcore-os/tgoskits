# Thermal controller model pipeline

`thermal-4x6x1-v1.weights.json` is the only editable mathematical source for
the fixed 4×6×1 controller. The model is not trained, and no conversion stage
may tune its weights. The Rust constants, ONNX model, golden corpus, RKNN model,
and ORT model must all be generated from this source.

The graph consumes an already normalized `[1, 4]` `float32` tensor:

```text
Gemm -> Relu -> Gemm -> Clip(0, 1)
```

Raw observation validation, normalization, and actuator rounding remain outside
the graph. They are nevertheless frozen in the canonical JSON and emitted into
the generated Rust module so the native and deployed backends share the same
boundary contract.

## Rebuild M4-0/M4-1 artifacts in WSL2

Use Ubuntu 22.04 and CPython 3.10. The lock file contains hashes, so an altered
or substituted package is rejected.

```bash
cd /home/seven_wsl/Workspace/starry/tgoskits-rt-ivc
uv venv --python 3.10 .venv-ivc-model
source .venv-ivc-model/bin/activate
uv pip sync --require-hashes competition/ivc/model/requirements-lock.txt
python competition/ivc/model/export_thermal_onnx.py
python competition/ivc/model/export_thermal_onnx.py --check
```

Build the real Rust oracle and compare all 10,000 frozen vectors against the
native controller and ONNX Runtime when the latter is installed:

```bash
cargo build -p ivcproto --example thermal_oracle
python competition/ivc/model/verify_thermal_models.py \
  --rust-oracle target/debug/examples/thermal_oracle
```

The ORT gate requires `max_abs_error <= 1e-6`. An adjacent one-permille command
difference is classified separately, and accepted only when both outputs are
within `1e-6` of the same half-permille rounding boundary. It is never counted
as an exact actuator match. Any larger command difference, or a difference away
from that explicitly defined ambiguity interval, fails verification.

The deterministic rebuild gate creates two independent temporary output trees,
then compares the generated Rust source, ONNX bytes, golden JSON, and manifest:

```bash
bash competition/ivc/model/rebuild-check.sh
```

## Rebuild and verify M4-2 RKNN FP16 artifacts

M4-2 uses the exact `/usr/bin/python3.10` from Ubuntu 22.04 (3.10.12). The
separate lock includes the commit-pinned official Toolkit2 wheel and hashes for
every package. `--torch-backend cpu` is required because the frozen Torch build
is `2.4.0+cpu`.

```bash
uv venv --python /usr/bin/python3.10 \
  /home/seven_wsl/.cache/tgoskits/ivc-rknn-py310-formal
uv pip sync \
  --python /home/seven_wsl/.cache/tgoskits/ivc-rknn-py310-formal/bin/python \
  --require-hashes --torch-backend cpu \
  competition/ivc/model/requirements-rknn-lock.txt

RKNN_PY=/home/seven_wsl/.cache/tgoskits/ivc-rknn-py310-formal/bin/python
"$RKNN_PY" competition/ivc/model/convert_thermal_rknn.py
"$RKNN_PY" competition/ivc/model/convert_thermal_rknn.py --check
"$RKNN_PY" competition/ivc/model/verify_thermal_rknn.py
"$RKNN_PY" competition/ivc/model/verify_thermal_rknn.py --check
IVC_RKNN_PYTHON="$RKNN_PY" \
  bash competition/ivc/model/rebuild-rknn-check.sh
```

The frozen RKNN model is 15,873 bytes with SHA-256
`2ad3fecedc9767ee57cbcd31787f70297a8f8e2cfcdc8e07b81b949566d53bb8`.
Toolkit2 maps the two mathematical compute nodes to RK3588 NPU `ConvRelu` and
`ConvClip` nodes; input/output wrappers remain on the CPU and there are no
custom CPU operators. The normalized evidence is in `rknn-conversion.log` and
`rknn-conversion-report.json`.

Toolkit2 otherwise writes process-memory-dependent bytes into a fixed internal
region of this small model. The converter starts a fresh child with
`MALLOC_PERTURB_=255` and `PYTHONHASHSEED=0`; two independent rebuild checks are
byte-identical. It never patches the exported binary. Unnormalized vendor logs
contain timestamps and are intentionally written only when `--raw-log` names an
external evidence path. A pre-freeze audit using the same formal configuration
without allocator perturbation produced two different hashes and 845 differing
bytes within offsets 12128 through 14200; those potentially process-dependent
artifacts are not committed, while their hashes are retained in the report.

The 10,000-vector Toolkit2 host-simulator result is frozen in
`rknn-simulator-report.json`: maximum error against native f32 is
`0.001798778772354126`, the command-delta histogram is
`{-2: 23, -1: 322, 0: 9369, 1: 280, 2: 6}`, and maximum error against the
deterministic FP16 oracle is `0.00048828125`. These are FP16 gates, separate
from the stricter ORT-f32 gate above.

Toolkit2 2.3.2 does not execute an artifact loaded with `load_rknn()` in its
host simulator. The simulator therefore rebuilds the same frozen ONNX with the
same RK3588 FP16 configuration; it is not evidence that the committed `.rknn`
ran on hardware. The exact compiled artifact must next pass Linux and StarryOS
physical-RK3588 tests.

RKNN Toolkit2 is vendor software and is not redistributed by this directory.
`rknn-toolkit2-2.3.2-source.json` records the official commit, URL, wheel
SHA-256, upstream files, and license assessment. The repository contains the
generated project model but not the vendor wheel.

## Run the M4-3 physical Linux RKNN reference

The Linux reference compiles a small C++ runner on the Orange Pi, loads the
frozen `.rknn`, pins execution to NPU core 0, and records both wall time and
`RKNN_QUERY_PERF_RUN` device time for every one of the 10,000 vectors. It
explicitly deploys the repository's Runtime 2.3.2 and rejects an `ldd` result
that resolves the board's older system Runtime 1.4.0.

Keep the board lease for the complete SSH/rsync/build/run/harvest sequence:

```bash
export IVC_RKNN_PYTHON=/home/seven_wsl/.cache/tgoskits/ivc-rknn-py310-formal/bin/python
export ORANGEPI_SSH_TARGET=orangepi@192.168.31.33
export ORANGEPI_SSH_IDENTITY=/home/seven_wsl/.ssh/orangepi_automation

bash competition/ivc/model/run-thermal-rknn-linux-reference.sh \
  --result-dir competition/results/orangepi-5-plus/<run-id> \
  --run-id <run-id> \
  --require-clean
```

`--require-clean` checks source provenance before creating the result
directory or taking the board lease. A remote directory is never reused or
deleted; choose a new run ID after any failure. The script calls `sync` before
releasing the lease, preserves partial failures, runs the independent Python
analyzer, and writes a recursive `checksums.sha256` manifest.

The analyzer requires Runtime API 2.3.2, driver/module 0.9.6, Linux
`6.1.43-rockchip-rk3588`, writable ext4 rootfs, FP16 compiled tensor
interfaces with FP32 submission/retrieval, 10,000 positive device-time
samples, maximum native-f32 error at most `0.002`, maximum actuator delta at
most 2, and maximum FP16-oracle error at most `0.0005`. This Linux result is a
hardware reference only; it does not satisfy the separate AxVisor handoff or
StarryOS guest-ownership gates.

## Ownership boundary for the physical NPU

The direct StarryOS RKNN example proves the existing user Runtime and
`/dev/dri/card1` ioctl path on bare metal. It does not prove that an AxVisor
guest owns the RK3588 NPU. See `rk3588-npu-passthrough-audit.md` for the live-DTB
audit and the required host/guest ownership gate.
