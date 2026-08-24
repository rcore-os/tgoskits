# Task3 baseline/CNN/YOLO quantitative comparison (2026-08-21)

This batch is the current-HEAD QEMU AArch64 software-in-the-loop comparison.
The same switch runner, topology, fixed plant scenario and `MIN_ELAPSED_MS=26000`
were used for all modes. Runs were interleaved in this order:

```text
baseline-1, cnn-1, yolo-1,
baseline-2, cnn-2, yolo-2,
baseline-3, cnn-3, yolo-3
```

Each run produced two captures and passed:

```bash
python3 scripts/test/net-dual-guest/verify_pcap.py \
  --tag '' --require-task2 <run>/linux.pcap <run>/rtos.pcap
```

The raw evidence is under `results/task3/switch/quant-{baseline,cnn,yolo}-{1,2,3}/`.
The three build artifacts and their hashes are recorded in
`build-variants-manifest.toml`.

## Metrics contract

`quantify-model-runs.py` reports two distinct error metrics:

- `tracking_rmse`: error between the plant state and the target actually sent
  by the model/controller. This is the fairest metric for YOLO because its
  fixture adapter emits bounded dynamic targets (`500`, `419`, `519`, ...).
- `scenario_*`: error against the frozen plant scenario (`300 → 800 → 500`).
  It is directly comparable for baseline/CNN. YOLO values are diagnostic only;
  its model target is intentionally not the fixed-step target.

Settling time is reported only when a target remains stable for enough samples.
YOLO's per-frame fixture target changes make fixed-step settling **not
applicable**, rather than silently assigning it a misleading value.

`replay_infer_*` is the bounded fixture-replay adapter overhead. It is not
ONNX-runtime inference time and must not be used as a physical YOLO performance
claim.

## Median over three runs

| Mode | Samples | Model-target RMSE | Frozen-scenario RMSE | Mean RTT (ms) | P95 RTT (ms) | Model-target settling (ms) | Max positive overshoot | Detection / rejection | Mean confidence | Replay infer mean (us) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| baseline | 188 | 197.09 | 198.57 | 140.24 | 315 | N/A | 46 | 0 / 0 | N/A | N/A |
| CNN | 157 | 46.99 | 62.31 | 165.59 | 335 | 1157 | 149 | 0 / 0 | N/A | 13452 |
| YOLO fixture replay | 132 | 152.96 | 295.97* | 196.95 | 363 | N/A | 0 | 88 / 45 | 0.8515 | 2.82 |

`*` The YOLO frozen-scenario RMSE is diagnostic only because the model emits
dynamic fixture targets; the model-target RMSE is the valid YOLO tracking value.

The result supports the following bounded conclusion:

- CNN improved model-target tracking RMSE over baseline in this batch, while
  carrying higher RTT and a measurable replay/temporal-model cost.
- YOLO fixture replay exercised detection, rejection and bounded target-change
  behavior, but its dynamic target semantics and replay adapter mean it is not a
  like-for-like accuracy or inference-speed comparison with CNN.
- All three modes completed the T2N1 loop and passed dual-pcap ledger checks.

## Reproduction

```bash
scripts/task3/build-quant-variants.sh

MIN_ELAPSED_MS=26000 bash scripts/task3/run-task3-switch.sh quant-baseline-1 baseline
MIN_ELAPSED_MS=26000 bash scripts/task3/run-task3-switch.sh quant-cnn-1 cnn
MIN_ELAPSED_MS=26000 bash scripts/task3/run-task3-switch.sh quant-yolo-1 yolo

python3 scripts/task3/quantify-model-runs.py \
  results/task3/switch/quant-baseline-1/run.log \
  results/task3/switch/quant-baseline-2/run.log \
  results/task3/switch/quant-baseline-3/run.log \
  results/task3/switch/quant-cnn-1/run.log \
  results/task3/switch/quant-cnn-2/run.log \
  results/task3/switch/quant-cnn-3/run.log \
  results/task3/switch/quant-yolo-1/run.log \
  results/task3/switch/quant-yolo-2/run.log \
  results/task3/switch/quant-yolo-3/run.log \
  --modes baseline,baseline,baseline,cnn,cnn,cnn,yolo,yolo,yolo \
  --out-dir results/task3/quant-20260821
```

The exact current batch is summarized in `model-quant-summary.csv` and
`model-quant-aggregate.csv`; raw logs, pcap files and per-run manifests remain
the authoritative evidence.
