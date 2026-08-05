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

## Freeze and verify the ORT CPU fallback

The ORT route is a no-training CPU fallback and comparison backend. It converts
the same frozen ONNX file to ORT format with ONNX Runtime 1.25.0, fixed
optimization style, ARM targeting, and type-reduced operator metadata. Use the
separate CPython 3.12.11 environment; its hash-locked packages are intentionally
independent from the RKNN Toolkit2 environment.

```bash
uv venv --python 3.12.11 \
  /home/seven_wsl/.cache/tgoskits/ivc-ort-py312
uv pip sync \
  --python /home/seven_wsl/.cache/tgoskits/ivc-ort-py312/bin/python \
  --require-hashes competition/ivc/model/requirements-ort-lock.txt

ORT_PY=/home/seven_wsl/.cache/tgoskits/ivc-ort-py312/bin/python
"$ORT_PY" competition/ivc/model/export_thermal_ort.py --check
IVC_ORT_PYTHON="$ORT_PY" \
  bash competition/ivc/model/rebuild-ort-check.sh
```

The frozen 4,144-byte artifact has SHA-256
`3582869baf9b8cec722208d06f66acd680a64128b52875d22e7f0e43f2ed7887`.
Its reduced build contract contains only `Clip`, `Gemm`, and the optimized
`FusedGemm`. CPU EP verification passes all 10,000 vectors with maximum
absolute error `2.980232238769531e-07`, 9,999 exact actuator commands, one
explicit half-permille rounding-boundary equivalence, and zero material command
mismatches.

Repeated conversions with the pinned toolchain exposed two different ORT
FlatBuffer byte layouts. Both have the same public graph, reduced operator
contract, and 10,000-vector output fingerprint. The committed report therefore
freezes one canonical byte sequence while `--check` accepts only the two
audited regenerated hashes and reruns the complete semantic gate. It does not
claim byte-deterministic upstream serialization or silently replace the
canonical artifact.

`onnxruntime-1.25.0-source.json` freezes the official Linux AArch64 archive,
release commit, headers, shared libraries, license files, and dynamic ABI
requirements. The full CPU runtime library is 19,215,360 bytes and requires at
most GLIBC 2.27, GLIBCXX 3.4.21, and CXXABI 1.3.11. Those facts establish a
candidate rootfs payload; the exact C API runner has now also passed the
physical-board syscall, resource, and 10,000-vector gates described below.

Build the exact AArch64 runner, StarryOS kernel, guest DTB, and 160 MiB glibc
rootfs from WSL2 with:

```bash
bash competition/ivc/starry/build-ort-offline.sh
```

The build entrypoint automatically prefers the Ubuntu system `libclang` over a
Homebrew LLVM that may require a newer host GLIBC. Unless
`IVC_ORT_PYTHON` is set explicitly, it uses the locked environment at
`${XDG_CACHE_HOME:-$HOME/.cache}/tgoskits/ivc-ort-py312/bin/python` and reruns
the complete frozen ORT check before compiling StarryOS.

After setting the board Linux root selector, one command rebuilds, stages,
runs, restores Linux, harvests the immutable block snapshot, and independently
validates every embedded artifact and all 10,000 outputs:

```bash
ORANGEPI_AXVISOR_HOST_ROOT=/dev/mmcblk0p2 \
ORANGEPI_ORT_REQUIRE_CLEAN_SOURCE=1 \
  bash competition/ivc/run-ort-offline.sh \
    --result-dir tmp/competition/ivc/ort-formal-YYYYMMDD-v1
```

The selector uses AxVisor block-device numbering: its `disk0p2` is the SD-card
rootfs that Linux reports as `/dev/mmcblk1p2`. Passing the Linux name to
AxVisor instead selects the eMMC `misc` partition and is expected to fail
before the guest starts.

Do not set the clean-source requirement to zero for formal evidence. Failed
runs retain their logs, provenance, and checksums under the requested result
directory and must not be relabeled as passing runs.

The formal physical run `ort-offline-formal-20260805-v2` used clean commit
`2df7da841f5fe778c02bb91aafae9ac908f595d5`. ONNX Runtime 1.25.0
CPUExecutionProvider completed all 10,000 vectors with maximum absolute error
`2.980232238769531e-07`, 9,999 exact commands, one accepted rounding-boundary
equivalence, and zero material mismatches. Wall latency p50/p95/p99/maximum was
121333/128042/157208/3090792 ns and session initialization was 1780 us. Five
session create/destroy cycles ended with 224 KiB post-destroy RSS growth, peak
RSS was 16,196 KiB, and 63.69% of the 160 MiB rootfs remained available. Linux
was restored on `/dev/mmcblk1p2` ext4. The recursive evidence manifest has
SHA-256 `33eac20d68ba9dfc134b8208f924b583cc6c76f595b90b392ad91ac7620a1999`.

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

The formal clean-source run
`thermal-rknn-linux-formal-20260804-v1` used commit
`25d3f6d63bf09e62706563e3fa58502b3803db67` and completed all 10,000 NPU
inferences. Device p50/p95/p99/maximum was 130/359/397/798 us; wall-time
p50/p95/p99/maximum was 136790/376247/411538/838242 ns. Its numerical errors
and actuator-delta histogram exactly match the frozen host-simulator result.
The evidence is under
`competition/results/orangepi-5-plus/thermal-rknn-linux-formal-20260804-v1/`.

## Ownership boundary for the physical NPU

The direct StarryOS RKNN example proves the existing user Runtime and
`/dev/dri/card1` ioctl path on bare metal. It does not prove that an AxVisor
guest owns the RK3588 NPU. See `rk3588-npu-passthrough-audit.md` for the live-DTB
audit and the required host/guest ownership gate.
