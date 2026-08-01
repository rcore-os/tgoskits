# Approximately five-minute demonstration storyboard

This is a recording plan and evidence checklist. It is **not** a generated or
completed video. The post-IRQ-gate neural/manual cross-guest runs and native
Zephyr baseline are available, as are the final shared/partitioned
idle/stress/soak results and deterministic cross-guest ACK-loss run. The actual
video still has to be recorded.

## Before recording

- Use the committed implementation revision and show its hash plus a clean
  worktree when commit/PR work is authorized. Until then, show the base commit,
  dirty status, and source snapshot hash honestly.
- Pre-build the Linux and Zephyr images; retain their hashes and build logs.
- Retain the full QEMU console log independently of the screen recording.
- Prepare two readable terminal panes: one following Linux/controller lines,
  the other following Zephyr/AxVisor lines from the same raw log. Do not imply
  separate serial devices if both panes are filtered views of one console.
- Prepare the exact normal, fault-injection, manual, neural, idle, stress, and
  native-baseline result directories used in the video.
- Verify the retained normal-run source-log hashes before recording:
  `6c7f7e2e404a5c8ef8a9a3f632a24169b35d8be6a8c0ac496775bf9d32a07eb8`
  for neural and
  `39ac8deaf5382490a007bfd47ec7384989c64c6092eed70ac8ff682c076d8a57`
  for manual. Also verify ACK-loss log
  `f15c88c6671db67934ce178e3f113b65ac2811a1538a0c36412f6c156bd279fd`.
- Generate RT sample-series plots from the retained AxVisor RT logs and control
  time-series plots from `results/host-ai-reference/raw.csv`, labeling the
  latter as host functional evidence. Use the validated cross-guest and native
  summaries for their aggregate tables. The native console retains aggregate
  records only, not individual samples; do not reconstruct or imply a native
  sample-series plot.
- Disable notifications and enlarge terminal fonts; keep command lines and
  timestamps visible.

## Timeline

### 0:00-0:25 — Goal and provenance

Show the title, repository revision, platform, QEMU version, Rust toolchain,
Zephyr version/compiler, and image hashes. State the one-sentence goal:
two-vCPU Linux neural control of a Zephyr endpoint over isolated UDP/IP on
AxVisor.

Evidence on screen:

```sh
git rev-parse HEAD
git status --short
qemu-system-aarch64 --version
rustc +nightly-2026-07-15 --version
```

### 0:25-1:05 — Resource and isolation design

Show the CPU/memory/device table from [`design.md`](design.md) and briefly point
out:

- Linux vCPUs dedicated to pCPUs 1 and 2;
- Zephyr on pCPU0 and pCPU3 left out of guest affinity masks;
- non-overlapping guest memory regions;
- virtio-net MMIO/IRQ assignments 56 and 64;
- fixed MAC/IP identities in switch segment 1; and
- no host NIC, NAT, bridge, vsock, shared-memory, or hypercall data channel.

Say the limitation aloud: guest-vCPU partitioning does not yet isolate every
AxVisor task/interrupt, and RR guest preemption is not claimed.

### 1:05-1:45 — Boot both guests

Run or replay an uncut capture of the exact full QEMU command in
[`reproduce.md`](reproduce.md). Show AxVisor accepting the partition, the
Zephyr golden-vector pass, both MAC/IP values, and `IVC-RTOS-READY`. The mixed
IVC log does not emit an online CPU-count assertion. Replay the separate
validated two-vCPU Linux RT/partition gate for that proof, and label it as a
separate run.

Do not use an edited success marker without keeping the original complete log.
If either guest fails, stop the recording and fix/rerun instead of narrating it
as success.

### 1:45-2:35 — Bidirectional protocol and reliability

Use split filtered views of the same run:

- left: Linux CONTROL sequence, returned STATUS, and aggregate result;
- right: Zephyr applied sequence, actuator/temperature, ACK, and counters.

Overlay the 32-byte header fields briefly. Then replay the retained
deterministic ACK-loss run and show a retransmission, a duplicate suppressed
without a second actuator application, and eventual recovery. Show a malformed
request producing a typed ERROR only from the host/unit evidence unless a new
cross-guest capture is recorded; do not imply that the retained ACK-loss QEMU
run injected malformed traffic. Show its controller-silence interval producing
safe mode with actuator 0.

End this section with the measured request success, errors, timeouts,
recoveries, RTT percentiles/max, throughput, and the positive
`IVC-SWITCH-TX`/`FORWARD`/`NOTIFY` forwarding counters from the retained
cross-guest result—not the host loopback reference. Show anti-spoof and drop
policy counters from the focused unit/regression evidence unless a new
malicious-traffic cross-guest capture is recorded; the retained QEMU runs do
not provide runtime drop totals.

### 2:35-3:45 — Neural closed loop

Show one complete sample path:

```text
temperature/status -> 4 inputs -> 4x6x1 inference -> actuator CONTROL
-> Zephyr applies/steps plant -> STATUS -> next observation
```

Replay the same setpoint/disturbance scenario once with the fixed 500-permille
baseline and once with the neural policy. Show live actuator and measured
temperature changes, then show the validated cross-guest aggregate table for
RMSE, integrated absolute error, and maximum overshoot. If a time-series plot
or settling time is shown, derive it from
`results/host-ai-reference/raw.csv`, label it as deterministic host functional
evidence, and state that manual settling was not reached while neural settling
was 27.9 seconds. Do not present those host-series values as cross-guest timing
or imply that the cross-guest summaries retain individual control samples.

Show the full input-before-inference to matching-status latency definition and
its p50/p95/p99/maximum. The retained `full_loop` metric includes observation,
policy evaluation, encoding, transport, RTOS application/plant step, returned
STATUS plus ACK, and response decoding; keep the `pre_send` and `transport`
sub-intervals visibly distinguished.

### 3:45-4:35 — Real-time validation

Show the guest probe definitions and retained idle/stress/soak summaries for
periodic jitter, scheduler dispatch, and the emulated timer-IRQ response proxy.
Show actual duration, pCPU/vCPU affinity, CPU load distribution, and maximum
latency. Then show the native Zephyr/equivalent baseline using the same nominal
period and comparable load.

State the mixed outcome: partitioned stress improved dispatch p99/maximum by
7.19%/31.24%, but jitter maximum and both timer-IRQ proxy tails worsened. Show
the 300-second measured soak and its 6.69 ms largest observed jitter rather
than presenting partitioning as globally faster.

Label QEMU TCG results as relative engineering measurements and the timerfd
metric as a userspace IRQ-response proxy, not a direct hypervisor injection
measurement.

### 4:35-5:00 — Reproduction and conclusion

Show the `competition/` entry page, image-build commands, exact QEMU command,
and result directories. Recap only claims backed by the displayed artifacts:

- two-vCPU Linux boot and deterministic guest placement;
- isolated IP-based bidirectional Linux/Zephyr link;
- reliable typed protocol and safe fallback;
- observable neural-to-RTOS closed loop; and
- measured control and real-time comparisons.

Close with any remaining limitations. Keep the final frame on the source hash,
reproduction guide, and test report.

## Final integrity checklist

- [ ] Video duration is close to five minutes.
- [ ] Source revision is committed and shown, or the pre-commit source
      fingerprint and dirty state are explicitly disclosed.
- [ ] Commands shown match retained metadata.
- [ ] Linux visibly has at least two online vCPUs.
- [ ] Both guest MAC/IP identities and UDP port are visible.
- [ ] Data visibly flows in both directions over IP.
- [ ] Retry, duplicate suppression, error notification, timeout safe fallback,
      and recovery are demonstrated.
- [ ] Neural inference is visibly upstream of the network command.
- [ ] RTOS control action and status feedback are visible.
- [ ] Manual and neural runs use the same scenario.
- [ ] Cross-guest aggregate control metrics are shown; any sample-series plot
      or settling time is derived from and labeled as host functional CSV
      evidence.
- [ ] Full-loop latency includes inference and states its clock/error method.
- [ ] Idle, stress, and soak results show sample-log provenance; native RTOS
      results show exact aggregate-console provenance and the no-sample-series
      limitation.
- [ ] QEMU/IRQ/partition limitations are spoken or shown.
- [ ] No host-loopback number is labeled cross-guest.
- [ ] No planned/template value is presented as measured.
- [ ] The complete unedited console log and all plotted raw data are archived.
