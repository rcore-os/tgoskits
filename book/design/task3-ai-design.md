# Task-3 AI Control Loop Design Document

> Branch: `openrace/task3-clean` (based on the Task-2 baseline `01f77307e`)
> Status: the five official requirements are complete; the link-fault safe
> recovery extension is kept as an optional capability.

## 1. Goal and Scope

Task-3 builds a **reproducible, quantifiable, and demonstrable** AI control
loop on top of the Task-2 dual-guest UDP/IP link:

```text
Zephyr virtual object / sensor
        │ T2N1 STATUS
        ▼
Linux Guest time-series model inference
        │ T2N1 CONTROL
        ▼
Zephyr applies the control and updates the virtual object
        │ T2N1 STATUS
        └────────── back to Linux
```

This work is a **software-in-the-loop (SIL) validation on QEMU**. It
explicitly does not claim:

- to have run on a physical board;
- that QEMU-measured latencies are hard-real-time guarantees;
- generalization beyond the frozen scenario.

## 2. System Architecture

### 2.1 Platform

| Item | Value |
|---|---|
| QEMU | 10.2.1, AArch64, `virt,virtualization=on,gic-version=3` |
| Hypervisor | Axvisor, virtual virtio-mmio endpoints + internal L2 software switch |
| Linux Guest | controller + model inference (statically packed initramfs, musl user binary) |
| Zephyr Guest | virtual object + executor (board `qemu_cortex_a53`, Zephyr 4.4.99) |
| Data link | two independent VirtIO-MMIO endpoints joined by the Axvisor internal L2 software switch |
| Endpoints | Linux `10.0.42.15:4242` ↔ Zephyr `10.0.42.2:4242` |

The data plane runs entirely inside Axvisor: each guest obtains a virtual
virtio-mmio endpoint (0x0a00_0000, wired IRQ 48) through
`[[devices.virtual]] model = "virtio-net"`, and the port joins the
hypervisor's built-in L2 software switch (SwitchPort + ingress queues +
poll_dma + vIRQ). Guest-to-guest traffic does not depend on any QEMU NIC,
socket pair, capture proxy, or external physical link; QEMU only hosts the
runtime environment. The protocol, the model, and both endpoint applications
are decoupled from the transport, so the same implementation needs no changes
in the board form factor.

### 2.2 Protocol and Messages

Task-3 reuses the Task-2 T2N1 reliable messaging (CONTROL / STATUS / ACK /
HEARTBEAT / ERROR). Task-3 uses a request-response pattern: one CONTROL maps
to one STATUS at a 100 ms control period (5–10 Hz). No high-frequency
telemetry stream is introduced, so the protocol state machine is unchanged.

## 3. Frozen Scenario and Parameters

The following parameters are frozen before the official comparison (fixed
seed; the scenario is not modified after results are seen):

```text
state range     0..1000
output range    0..1000
base_loss       15
nonlinear_loss  120
response        0.35
target track    0-5s: 300, 5-15s: 800, 15-25s: 500
disturbance     8s: +150 load, 17s: -150 load
control period  5-10 Hz (request-response)
baseline        Kp=2 pure P controller (frozen parameters, not
                deliberately de-tuned)
```

Plant update (Zephyr integer semantics; C division truncates toward zero;
the Python training side replicates it point by point):

```text
loss(state) = base_loss + trunc(nonlinear_loss * state² / 1_000_000)
state_next = clamp(state + trunc(response * (output - state)) - loss + disturbance, 0, 1000)
```

## 4. Model Design and Training

### 4.1 Model Structure

A 1D temporal CNN with a pure-Rust `no_std` forward pass; weights are
embedded via `include_bytes!`:

```text
input: 64 history samples x 4 features [state, target, error, prev_output]
Conv1D 4->32 k=5 ReLU
Conv1D 32->64 k=5 ReLU
Global average pooling
Dense 64->32 ReLU
Dense 32->1
```

- Parameters: 13,089; estimated MACs: ~0.7M per inference;
- Weights: `components/task3-model/model/weights.bin` (final DAgger
  artifact); SHA-256 and trainer hash recorded in `model/model.json`
  (`trainer=dagger_train.py`, `dagger_iterations=6`, `dagger_epochs=25`,
  `dagger_closed_loop_weight=3`);
- In-guest inference: mean 11.3 ms, p95 14.6 ms (QEMU TCG environment,
  measured via `infer_us`).

### 4.2 Training Method: Residual Learning + DAgger

- **Residual learning**: the model only learns the loss/disturbance
  compensation that the frozen P controller misses; the P term keeps the
  loop stable (frozen-scenario closed-loop RMSE 33, real dual-guest run
  RMSE 29.3).
- **Teacher**: a gain=0.5 inverse-control tracking policy.
- **DAgger true closed loop**: 100 ms `plant.step` rollouts where model
  outputs genuinely feed back into the state; 6 iterations, 120 random
  episodes each, teacher labels, 3x resampling (parameters recorded in
  `model.json`).
- **Data**: 400 random episodes (random target steps/disturbances/initial
  values), 94,800 samples; the frozen fixed test scenario is excluded from
  the training set.
- **Feature contract**: `scripts/task3/features.py::build_window` is the
  single implementation shared by the dataset, DAgger, evaluation, and the
  Rust guest; the Rust `build_features` mirrors it and is pinned
  cross-language by golden-window tests (error < 1e-12).
- **Golden tests**: convolutions are checked against a torch f64 reference
  (1e-9) with asymmetric inputs (ramps).
- **Metadata consistency**: `dagger_train.py` refreshes `model.json`
  (weight hash/trainer/parameters) after training; `export_golden.py`
  regenerates the Rust golden vectors from the final `weights.bin`, keeping
  "weights ↔ inference ↔ metadata" consistent.

### 4.3 YOLO perception supplement

The official AArch64 Guest path remains the no-std temporal CNN above.  A
separate YOLO11n ONNX fixture now exercises the perception side of the same
Task-3 contract without adding an ONNX runtime to the small Guest image:

```text
YOLO11n ONNX
  → letterbox/preprocess + channel-first decode
  → confidence/area/coordinate validation
  → bounded center-x target mapping
  → T2N1 CONTROL (future Guest/NPU adapter)
```

The reusable Rust boundary is `task3_model::perception`: it decodes a
YOLOv8-style channel-first tensor, reports malformed/non-finite output, and
limits one-frame target changes before the result can drive control.  The host
fixture is reproducible with `scripts/task3/run_yolo_fixture.py`; its model and
input hashes are archived under `results/task3/yolo/`.

The controller now accepts `TASK3_MODEL=baseline|cnn|yolo`.  `cnn` preserves the
validated temporal-CNN path; `yolo` uses a deterministic replay adapter for the
three archived fixture observations, then calls the same bounded Rust
`perception` contract before emitting T2N1 CONTROL.  A no-detection frame holds
the last accepted target and emits `TASK3_MODEL_REJECTED`, while confidence,
area, coordinate and one-frame target-step limits remain enforced.

This is a real control-path integration and a reproducible contract test, not an
in-Guest ONNX runtime performance claim.  The K230/Starry `.kmodel` path remains
hardware-specific and is tracked as a separate StarryOS/NPU extension.  The
model name, version, SHA256 and source path are emitted by `TASK3_MODEL_READY`.

## 5. Controller, Baseline, and Latency Measurement

### 5.1 Controller

- baseline: `output = clamp(Kp * error + bias, 0, 1000)`, Kp=2, bias=0;
- AI mode: `output = clamp(P output + model(features) * 1000, 0, 1000)`;
  the Zephyr side still applies the final clamp and the Safe fallback;
- request-response loop: the next CONTROL is sent only after the STATUS
  arrives (`request_in_flight` tracking); the model output enters
  CONTROL.value and is correlated with the request ID through the logs.

### 5.2 Latency Measurement Method (Same-Side Round Trip)

Measurements are taken on the **Linux controller's own clock**; no
cross-guest clock synchronization is required:

- `rtt_ms = STATUS receive time - CONTROL send time` (`main.rs`
  `on_status`, the same Linux-side `Instant` clock, integer milliseconds);
- inference time `infer_us` is timed separately with `Instant` before the
  send (microsecond resolution);
- `task3_metrics.py` parses `TASK3_STATUS_RECEIVED`/`TASK3_INFER` log lines
  and aggregates mean/p95.

**Cycle-level latency composition**: 100 ms rate-limit sleep + model
inference + network transport (software switch / direct connect) + RTOS
processing and plant update + STATUS return. Therefore `rtt_ms` is a
**whole-cycle latency**, not a pure network round trip; the extra ~13 ms of
AI mode over baseline mostly comes from inference time being inside the
window.

**Error sources and precision bounds**:

| Source | Explanation |
|---|---|
| Simulation jitter | QEMU TCG scheduling jitter; absolute values carry no hard-real-time meaning |
| Sampling granularity | the 100 ms request-response period only observes whole-cycle completion instants |
| Log sampling | only completed cycles are counted; failed/retransmitted cycles are excluded |
| Time resolution | `rtt_ms` is integer milliseconds (truncated, ±1 ms rounding); `infer_us` is microseconds |
| Clock boundary | no shared cross-guest time source; same-side measurement avoids clock skew but cannot separate one-way times |

Suggested reading: `rtt_ms` compares the **cycle-level control cadence** of
AI versus baseline and the system's end-to-end latency magnitude; it is
accurate to the millisecond level and no sub-millisecond conclusions are
drawn.

## 6. Extension: Link-Fault Safe Recovery

> Note: this extension is not part of the five official requirements and is
> kept as an optional capability (`scripts/task3/run-task3-switch-fault.sh`
> with the `virtnet drop` blackout, the recovery-path implementation, and
> the `results/task3/switch/fault-*/` evidence).

### 6.1 Fault Injection

Every frame in both directions is dropped for a timed window on the guest
link, simulating a runtime link outage; forwarding resumes automatically
when the window ends. The hypervisor-level blackout gate
(`virtnet drop on/off`) acts directly on the software-switch port boundary
while both protocol stacks keep running:

```text
blackout 25s->~35s (all frames dropped) -> both sides exhaust
retransmission/heartbeat and enter Safe -> blackout ends -> heartbeat
recovers -> reliable stream resynchronizes -> the control loop resumes
```

(In the QEMU socket direct-connect environment the equivalent fault is
injected by `ack_drop_proxy.py --blackout-*` dropping frames on the link;
see the earlier evidence under `results/task3/fault/`.)

### 6.2 Recovery Path Implementation

The completed code contains the following designs, which guarantee a safe
exit and recovery after a link break:

1. **Safe entry**: when retransmission is exhausted (`RetryExhausted`) or a
   heartbeat times out (`HeartbeatTimeout`), both sides enter Safe and the
   controller resets the application-level in-flight flag;
2. **Reliable-stream resynchronization**: on Safe→Active the protocol
   resets `next_tx/next_rx/pending` and both sides resynchronize from
   sequence number 1 (Rust `task2-net-protocol` and the Zephyr C side are
   consistent, with regression tests);
3. **ACK race tolerance**: when a STATUS arrives before its CONTROL's ACK,
   the next CONTROL is deferred until the Acknowledged event
   (`TASK2_CONTROL_DEFERRED`) rather than treated as fatal;
4. **Send robustness**: non-blocking UDP sends retry bounded on
   `WouldBlock` (`send_datagram`, 500 ms).

## 7. Experimental Evidence

### 7.1 AI/baseline comparison (software-switch link: 3 AI + 3 baseline, ~35 s each)

| Metric | AI (n=3) | baseline (n=3) |
|---|---|---|
| Overall RMSE | 40.7 / 40.7 / 41.6 | 195.9 / 196.9 / 197.6 |
| t300 segment RMSE (0-5s) | 66.7 / 66.7 / 69.7 | 79.7-81.3 |
| t800 segment RMSE (5-15s) | 47.5 / 50.9 / 53.1 | 236.5-240.7 |
| t500 segment RMSE (15-25s) | 29.6 / 30.2 / 30.4 | 192.4 |
| t500 steady-state error | ~2 (498 vs 500) | ~192 (308 vs 500) |
| t500 settling time (5% band) | 1250-1588 ms | never converges |
| Guest inference time | mean ~7.4-8.9 ms (QEMU TCG) | - |
| Cycle-level latency (whole cycle) | mean ~167 / ~174 / ~192 ms | mean ~159-162 ms |

Raw data: `results/task3/switch/` (per-run `run.log` + both pcaps +
`summary.csv` + `comparison.png`). The T2N1 frame ledgers of the two
captures are identical (`verify_pcap.py` PASS: 871 frames per side, CONTROL
204 + STATUS 205 + ACK 410 + HEARTBEAT 52).
**The same experiment on the QEMU socket direct-connect environment** (the
earlier experiment environment; transport = QEMU socket pair + filter-dump
capture): 3 AI + 3 baseline runs of ~39 s each, overall RMSE 29.2-29.3
versus 190.6-191.3, cycle latency ~104 ms versus ~91 ms; data in
`results/task3/run-{1..6}.csv`. The AI-versus-baseline conclusion is
identical across both environments; the software-switch link has higher
latency and a higher RMSE (see the latency characteristics in §9).

### 7.2 Link-Fault Safe Recovery (extension)

| Event | Software-switch link | QEMU socket direct-connect environment |
|---|---|---|
| Blackout window | 25s->~35s, `virtnet drop` drops everything both ways | 25s->35s, the proxy drops 102 frames (both ways) |
| Safe entry | both sides enter Safe on RetryExhausted / HeartbeatTimeout | same |
| Recovery | reliable stream resynchronizes from sequence 1 after `TASK2_RECOVERED` | same |
| Resumed loop | 29 STATUS cycles after recovery (4.8s, 0 protocol errors) | 82 STATUS cycles after recovery, cycle latency ~74-109 ms, 0 protocol errors |

Evidence: `results/task3/switch/fault-switch-fault-run1/` (guest log + both
pcaps) and `results/task3/fault/` (guest/proxy logs + both pcaps + SHA-256).

The final-head YOLO replays are archived separately so that the model choice and
the runtime head are unambiguous:

- `results/task3/switch/final-head-yolo-replay-v2/` is the normal switch loop;
  both 320-frame pcaps pass the Task2 ledger verifier.
- `results/task3/switch/fault-final-head-yolo-blackout-v2/` is the same YOLO
  controller under a 25--35 s switch blackout. It records Safe entry, link
  restoration, resynchronization, and resumed control through 45 s; both
  727-frame pcaps pass the verifier.

The Zephyr slot-0 image used by these replays is recorded in each manifest. Its
`embedded:fixture-replay` model marker is intentionally treated as a contract
and safety-path proof, not as a claim of real ONNX inference inside the Guest.

### 7.3 Protocol fault injection on the real Guest wire

The P3 proxy can inject one syntactically valid but semantically invalid CONTROL
after sequence 1. The runner waits for protocol rejection markers before
quitting, and the verifier checks the injected frame in the RTOS-side pcap, the
proxy injection record, and the Linux/RTOS error logs:

| Injection | Wire evidence | Rejection evidence |
|---|---|---|
| `out-of-order` | `CONTROL sequence=99` | RTOS `TASK2_PROTOCOL_ERROR out_of_order=99`; Linux `TASK2_REMOTE_ERROR code=OutOfOrder` |
| `invalid-parameter` | `CONTROL sequence=2`, value `1001` | RTOS invalid-parameter rejection; Linux `TASK2_REMOTE_ERROR code=InvalidParameter` |

Evidence directories:

- `results/task3/fault-current-head-yolo-injection-out-of-order/`
- `results/task3/fault-current-head-yolo-injection-invalid-parameter-v2/`

The commands are:

```bash
bash scripts/task3/run-task3-fault.sh <label> yolo injection out-of-order
bash scripts/task3/run-task3-fault.sh <label> yolo injection invalid-parameter
```

`verify_protocol_injection.py` accepts the endpoint-specific spelling of the
invalid-payload marker (`invalid_payload` in Rust or `invalid_parameter` in
Zephyr), while requiring the common remote `ErrorCode` and the captured wire
frame. This is protocol rejection evidence, not a physical-link fault claim or
a real ONNX runtime benchmark.

### 7.4 Evidence-Chain Tooling

| Capability | Implementation |
|---|---|
| Capture | `virtnet capture on/off/dump`: records every frame at the port boundary (timestamp + frame bytes) and streams classic pcap out of the shell (`switch.vm1.pcap` / `switch.vm2.pcap`) |
| Fault injection | `virtnet drop on/off`: a hypervisor-level blackout gate that drops all frames in both directions while both protocol stacks keep running |
| Port audit | `virtnet show`: the port table (VM/MAC/state) plus the blackout/capture switches |
| Automation | `serial_console.py` drives the whole lifecycle over the QEMU serial socket (boot → capture → blackout → recovery → pcap export → QMP quit), orchestrated by `run-task3-switch.sh` / `run-task3-switch-fault.sh` |

## 8. Reproduction Commands

```bash
# Training (fixed seed; dagger refreshes model.json when it finishes)
python3 scripts/task3/generate_dataset.py --train-episodes 400 --val-episodes 40
python3 scripts/task3/train_model.py --epochs 60
python3 scripts/task3/dagger_train.py --iterations 6 --epochs 25 --closed-loop-weight 3
python3 scripts/task3/export_golden.py && cargo test -p task3-model

# Build the guests (baseline / CNN / YOLO)
scripts/task3/build-quant-variants.sh
# requires the Zephyr SDK and source; slot 0 = software-switch link,
# slot 30 = QEMU socket-pair topology
TASK2_ZEPHYR_VIRTIO_SLOT=0 bash scripts/test/net-dual-guest/build-zephyr-task2.sh

# Experiments (same frozen scenario; software-switch link is the current data plane)
bash scripts/task3/run-task3-switch.sh cnn-runX cnn
bash scripts/task3/run-task3-switch.sh baseline-runX baseline
bash scripts/task3/run-task3-switch.sh yolo-runX yolo
bash scripts/task3/run-task3-switch-fault.sh yolo-fault-runX yolo

# Earlier experiment flow on the QEMU socket direct-connect environment
# (data under results/task3/)
bash scripts/task3/run-task3-experiment.sh ai-runX ai
bash scripts/task3/run-task3-experiment.sh baseline-runX baseline
bash scripts/task3/run-task3-fault.sh fault-runX

# Metrics (software-switch link example)
python3 scripts/test/net-dual-guest/task3_metrics.py <logs...> \
  --out-dir results/task3/switch --label switch --modes ai,ai,ai,baseline,baseline,baseline \
  --plot results/task3/switch/comparison.png

# Current three-mode quantitative report.  YOLO replay overhead is reported
# separately from network RTT and RTOS control latency.
python3 scripts/task3/quantify-model-runs.py <logs...> \
  --modes baseline,baseline,baseline,cnn,cnn,cnn,yolo,yolo,yolo \
  --out-dir results/task3/quant-20260821
```

The current batch is archived under `results/task3/quant-20260821/` and
`results/task3/switch/quant-{baseline,cnn,yolo}-{1,2,3}/`. The YOLO mode is
`embedded:fixture-replay`; its replay timing is not an ONNX-runtime benchmark.

## 9. Known Limitations and Honest Claims

- All conclusions rest on QEMU SIL validation; no physical board or
  hard-real-time guarantee is claimed;
- Extrapolation beyond the frozen scenario is limited: the model is only
  validated on the randomized training distribution;
- The t800 target (800) exceeds the plant's sustainable ceiling (~760,
  ~880 with the disturbance); the AI approaches ~790-810 instead of
  reaching the target;
- The baseline is the frozen Kp=2 pure P controller, not deliberately
  de-tuned;
- The current batch has three interleaved runs each for baseline, CNN and YOLO
  fixture replay. It demonstrates reproducibility and contract behavior, not a
  statistically significant sample;
- Cycle-level latency includes the rate limit and inference and must not
  be read as pure network RTT (see §5.2);
- Safe recovery is validated as an extension; `set_link` is ineffective in this
  environment (a platform boundary), so fault injection uses the
  `virtnet drop` blackout gate (the P3-proxy blackout in the QEMU socket
  direct-connect environment);
- **RX delivery characteristics of the software-switch link**: delivery
  depends on `poll_dma` in the vCPU run loop rather than the immediate IRQ
  of a directly connected device. The measured median control period is
  ~130 ms (rtt-bound) with periodic ~300 ms spikes (about 2 per second,
  correlated with vCPU wake races at heartbeat delivery); the period stays
  within the 5-10 Hz design range. The spikes make the AI model's time
  window non-uniform, which raises the RMSE from ~29 on the socket
  direct-connect environment to ~41, but the ~4.8x AI-versus-baseline gap
  and every conclusion remain intact;
- In the board form factor the physical NIC only serves external
  management; guest-to-guest traffic does not traverse a physical link.

## 10. Completion-Definition Checklist

### The five official requirements

| Requirement | Status |
|---|---|
| ① Linux guest runs an NN inference application and sends model outputs over T2N1 | ✅ |
| ② RTOS adjusts control per AI output and performs observable actions (logs/plant state) | ✅ |
| ③ Complete loop (input → inference → cross-guest → control → state feedback) | ✅ |
| ④ End-to-end latency measured with method/error sources/precision bounds explained | ✅ (§5.2) |
| ⑤ Fixed-parameter baseline comparison with at least two metrics | ✅ (RMSE/settling time/overshoot/latency/inference time) |

### Extensions and deliverables

| Item | Status |
|---|---|
| Link-fault safe recovery (blackout → Safe → resynchronization → resumed loop) | ✅ (2 reproductions; not an official requirement) |
| Raw logs, CSV, model hash, reproduction commands | ✅ (`results/task3/` + this document) |
| Existing Task-2 tests/evidence do not regress | ✅ (20 protocol tests + 20 Python tests) |
| Demo video | deferred (outside this branch) |
