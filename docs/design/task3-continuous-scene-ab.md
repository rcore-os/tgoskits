# Task 3 continuous-scene A/B design

## Problem and success criteria

The existing Task 3 fixture compares five independent still images. It proves
that one YOLO result can be bounded before it becomes a controller target, but
it does not exercise temporal tracking, a real hazard transition, Stop
latching, or Reset recovery.

The continuous-scene experiment uses sampled frames from two real videos:

- KITTI road footage for moving cars, buses, and trucks;
- an overhead pedestrian sequence for entry into a central restricted zone.

The experiment succeeds when all of the following are observable in logs and
the final result bundle:

1. accepted vehicle detections produce targets in `0..=1000`, with no target
   step larger than 100;
2. a COCO person whose center enters the configured restricted zone, or a
   knife/scissors detection, sends T2N1 `Stop`;
3. Stop remains latched when later frames contain safe vehicles;
4. only an explicit T2N1 `Reset` returns the controller to Tracking;
5. three consecutive absent, invalid, low-confidence, small, or irrelevant
   observations fail safe to a latched Stop;
6. fixed-perception versus YOLO results and fixed-P versus temporal-CNN
   controller results are reported separately.

## Frozen policy

All normalized coordinates use integer thousandths. COCO class IDs are
`person=0`, `car=2`, `bus=5`, `truck=7`, `knife=43`, and `scissors=76`.

The central pedestrian danger zone is inclusive:

```text
x = 350..650
y = 300..1000
```

Tracking retains the existing minimum confidence 600, minimum area 10,
target range `0..=1000`, and maximum step 100. A rejected observation holds
the previous target for two sampled frames; the third consecutive rejection
latches Stop. This makes a weak hazard detection unable to silently resume
vehicle tracking while avoiding a one-frame emergency stop on detector noise.

## State and actions

```text
Tracking + vehicle             -> CONTROL(target), Tracking
Tracking + hazard              -> Stop, StoppedLatched
Tracking + unusable frame x1-2 -> Hold previous target, Tracking
Tracking + unusable frame x3   -> Stop, StoppedLatched
StoppedLatched + any frame     -> Stop, StoppedLatched
StoppedLatched + Reset         -> Reset, Tracking
```

The semantic state machine lives in `task3-model` and has no OS, network, or
model-runtime dependency. Board runners translate its decisions into the
already-defined T2N1 `SetOutput`, `Stop`, and `Reset` actions.

## A/B boundaries

The perception comparison uses the same frozen video samples:

- baseline: target 500 and no semantic hazard recognition;
- YOLO: vehicle-center tracking plus the frozen safety policy above.

Metrics are vehicle recall, center-x MAE, hazard recall, false stops,
hazard-to-Stop latency, and no-detection behavior.

The control comparison replays the same accepted target and Stop/Reset event
sequence into both controllers:

- baseline: fixed `Kp=2` proportional controller;
- AI: frozen temporal CNN controller.

Metrics are RMSE, IAE, settling time, overshoot, CONTROL-to-STATUS RTT, and
controller inference time. A perception miss must not be presented as a
controller-quality result.

## Risks and non-goals

The deployed ncnn adapter currently selects one highest-confidence candidate;
therefore the frozen samples must be preflighted to ensure the intended
vehicle or hazard is the selected candidate. Multi-object tracking and full
NMS on the guest are follow-up work, not prerequisites for this bounded A/B.

The board does not process the original video at 30 FPS. Frames are sampled at
a fixed cadence because one guest inference is approximately 1.6 seconds.
The quick run uses about 12 frames, and the evidence run uses about 24 frames;
neither is a long-duration soak test.

## RKNN/NPU hybrid integration

This branch keeps the semantic state machine in `task3-model` and changes only
the perception adapter.  The RKNN process pinned to StarryOS vCPU1 publishes
one atomically replaced event record after each inference.  The T2N1 process
on vCPU0 validates the record, applies `VideoSafetyController`, and translates
the resulting decision into the existing `SetOutput`, `Stop`, or `Reset`
protocol action.  Zephyr remains the sole control executor and returns STATUS.

The event record contains a strictly increasing generation, frozen event ID,
event kind, normalized detection fields, and `CLOCK_MONOTONIC` inference start
and end timestamps.  The controller records CONTROL send and STATUS receipt
with the same StarryOS monotonic clock.  Therefore inference-start-to-STATUS is
measured without Linux/RTOS clock synchronization; CONTROL-to-STATUS RTT is a
second, narrower measurement.  Timestamp quantization is nanoseconds, while
the practical precision is bounded by RKNN publication, the controller poll
period, guest scheduling, and network delivery.

The fixed-perception arm traverses the same frozen manifest and image paths but
publishes a fixed target of 500 and performs no semantic hazard recognition.
The scheduler, vCPU affinity, NPU topology, Zephyr image, protocol, and control
executor stay unchanged between arms.  The only intended A/B difference is
the perception decision source.

### Alternatives considered

- Parsing RKNN stdout was rejected because console output is intentionally
  silent during real-time sampling and is not an atomic interface.
- Implementing Stop latching in the C++ benchmark was rejected because it
  would duplicate the tested Rust state machine and make policy ownership
  ambiguous.
- Restarting the RKNN executable once per frame was rejected because repeated
  model initialization would dominate the measured latency.
- A shared-memory queue would reduce polling overhead, but it expands the
  cross-process synchronization surface; the bounded twelve-event experiment
  does not need that complexity.

The event record is an experiment-local interface, versioned in its readiness
marker and rejected on missing, duplicate, unknown, or out-of-range fields.
The existing generation/target record remains accepted only by legacy runs;
the continuous-scene runner requires the new version and never silently falls
back.
