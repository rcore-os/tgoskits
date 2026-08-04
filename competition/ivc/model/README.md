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

RKNN Toolkit2 is vendor software and is not redistributed by this directory.
The M4-2 conversion step must record the exact official source URL, version,
wheel SHA-256, license, conversion log, and compatibility with the committed
`librknnrt.so` before its version is frozen.

## Ownership boundary for the physical NPU

The direct StarryOS RKNN example proves the existing user Runtime and
`/dev/dri/card1` ioctl path on bare metal. It does not prove that an AxVisor
guest owns the RK3588 NPU. See `rk3588-npu-passthrough-audit.md` for the live-DTB
audit and the required host/guest ownership gate.
