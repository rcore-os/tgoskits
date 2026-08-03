# Approximately five-minute demonstration storyboard

This is a recording plan and evidence checklist. It is **not** a generated or
completed video. The primary demonstration is the retained physical Orange Pi
5 Plus run: a two-vCPU StarryOS guest executes the neural controller and drives
a Zephyr guest over isolated UDP/IP on AxVisor. The QEMU Linux manual/neural,
ACK-loss, AxVisor RT, and native Zephyr results remain supporting comparison
evidence. The actual video still has to be recorded.

## Before recording

- Use the committed implementation revision and show its hash plus a clean
  worktree when commit/PR work is authorized. Until then, show the base commit,
  dirty status, and source snapshot hash honestly.
- Pre-build and stage the StarryOS kernel, two finite rootfs images, guest DTB,
  Zephyr board images, and AxVisor binary; retain their hashes and build logs.
- Run or replay the maintained `run-orangepi-5-plus.sh full` capture. Retain the
  complete physical serial log and generated JSON independently of the screen
  recording, then confirm the TF-card Linux system was restored.
- Prepare two readable terminal panes: one following StarryOS/controller
  lines, the other following Zephyr/AxVisor lines from the same CH340 capture.
  State that these are filtered views of one shared physical UART.
- Prepare the exact normal, fault-injection, manual, neural, idle, stress, and
  native-baseline result directories used in the video.
- Verify the retained normal-run source-log hashes before recording:
  `023ff07b40b4936453eee6d4bbd57bca1c1699e7305dc1af5fe601a5d67492d9`
  for the physical full run and
  `8dd16dbcc7608305da9fcf13f393a54e410e16ef26da63ca5c2821878efbf265`
  for physical smoke. For the supporting QEMU evidence, verify
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

Show the title, repository revision, Orange Pi/RK3588 platform, WSL2 automation
host, U-Boot, Rust toolchain, Zephyr version/compiler, and image hashes. State
the one-sentence goal: two-vCPU StarryOS neural control of a Zephyr endpoint
over isolated UDP/IP on AxVisor, with unattended boot, analysis, filesystem
sync, and Linux recovery.

Evidence on screen:

```sh
git rev-parse HEAD
git status --short
rustc +nightly-2026-07-15 --version
cat competition/results/orangepi-starry-reference/metadata.json
```

### 0:25-1:05 — Resource and isolation design

Show the CPU/memory/device table from [`design.md`](design.md) and briefly point
out:

- StarryOS vCPUs dedicated to pCPUs 1 and 2;
- Zephyr on pCPU0, with pCPU3 left out of all guest affinity masks;
- non-overlapping guest memory regions;
- virtio-net MMIO/IRQ assignments 56 and 64;
- fixed MAC/IP identities in switch segment 1; and
- no host NIC, NAT, bridge, vsock, shared-memory, or hypercall data channel.

Say the limitation aloud: guest-vCPU partitioning does not yet isolate every
AxVisor task/interrupt, and RR guest preemption is not claimed.

### 1:05-1:45 — Boot both guests

Run or replay an uncut capture of the physical full command in
[`reproduce.md`](reproduce.md). Show U-Boot entering AxVisor, AxVisor accepting
the partition, the `IVC-STARRY-BOOT` record ending in `vcpus=2`, and StarryOS configuring
`10.0.0.1/24`, the Zephyr golden-vector/MAC checks, and
`IVC-RTOS-READY bind=10.0.0.2:5500`. Finish by showing the strict analyzer
summary, AxVisor filesystem-sync confirmation, and the restored Linux rootfs.

Do not use an edited success marker without keeping the original complete log.
If either guest fails, stop the recording and fix/rerun instead of narrating it
as success.

### 1:45-2:35 — Bidirectional protocol and reliability

Use split filtered views of the same run:

- left: StarryOS CONTROL sequence, returned STATUS, and aggregate result;
- right: Zephyr applied sequence, actuator/temperature, ACK, and counters.

Overlay the 32-byte header fields briefly. Show that the physical analyzer
requires matching controller and RTOS counters instead of trusting a terminal
success line; explain that short terminal records are paced and emitted twice
because all consoles share one UART. Then replay the supporting QEMU
deterministic ACK-loss run and show a retransmission, a duplicate suppressed
without a second actuator application, and eventual recovery. Show a malformed
request producing a typed ERROR only from host/unit evidence unless a new
cross-guest capture is recorded. Show the ACK-loss run's controller-silence
interval producing safe mode with actuator 0.

End this section with the physical full-run request success, errors, timeouts,
recoveries, full-loop/transport percentiles and maximum, throughput, and the
matching controller `sent`/`acknowledged` plus RTOS `accepted`/`applied`
counters—not the host loopback reference. Show virtual-switch
forwarding, anti-spoof, and drop-policy counters from the focused
unit/regression evidence unless a new runtime metrics snapshot is recorded;
the retained QEMU runs do not provide runtime switch totals.

### 2:35-3:45 — Neural closed loop

Show one complete sample path:

```text
temperature/status -> 4 inputs -> 4x6x1 inference -> actuator CONTROL
-> Zephyr applies/steps plant -> STATUS -> next observation
```

Use the physical StarryOS run for the neural path and show live actuator and
measured temperature changes. Then show the matched QEMU Linux fixed
500-permille and neural summaries for the controlled comparison. The physical
run reproduces the neural RMSE, integrated absolute error, and maximum
overshoot. If a time-series plot or settling time is shown, derive it from
`results/host-ai-reference/raw.csv`, label it as deterministic host functional
evidence, and state that manual settling was not reached while neural settling
was 27.9 seconds. Do not present those host-series values as cross-guest timing
or imply that the cross-guest summaries retain individual control samples.

Show the physical input-before-inference to matching-status latency definition
and its p50/p95/p99/maximum. The retained `full_loop` metric includes
observation, StarryOS policy evaluation, encoding, transport, RTOS
application/plant step, returned STATUS plus ACK, and response decoding; keep
the `pre_send` and `transport` sub-intervals visibly distinguished.

### 3:45-4:35 — Real-time validation

Label this as supporting QEMU evidence. Show the guest probe definitions and
retained idle/stress/soak summaries for periodic jitter, scheduler dispatch,
and the emulated timer-IRQ response proxy. Show actual duration, pCPU/vCPU
affinity, CPU load distribution, and maximum latency. Then show the native
Zephyr/equivalent baseline using the same nominal period and comparable load.

State the mixed outcome: partitioned stress improved dispatch p99/maximum by
7.19%/31.24%, but jitter maximum and both timer-IRQ proxy tails worsened. Show
the 300-second measured soak and its 6.69 ms largest observed jitter rather
than presenting partitioning as globally faster.

Label QEMU TCG results as relative engineering measurements and the timerfd
metric as a userspace IRQ-response proxy, not a direct hypervisor injection
measurement.

### 4:35-5:00 — Reproduction and conclusion

Show the `competition/` entry page, WSL2 image/staging commands, physical board
run command, strict summary, and retained result directories. Recap only claims
backed by the displayed artifacts:

- two-vCPU StarryOS boot and deterministic guest placement on Orange Pi;
- isolated IP-based bidirectional StarryOS/Zephyr link;
- reliable typed protocol and safe fallback;
- observable neural-to-RTOS physical closed loop and automatic Linux restore;
  and
- separately labeled QEMU manual/control and real-time comparisons.

Close with any remaining limitations. Keep the final frame on the source hash,
reproduction guide, and test report.

## Final integrity checklist

- [ ] Video duration is close to five minutes.
- [ ] Source revision is committed and shown, or the pre-commit source
      fingerprint and dirty state are explicitly disclosed.
- [ ] Commands shown match retained metadata.
- [ ] StarryOS visibly has two online vCPUs on the physical board.
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
- [ ] Strict physical JSON analysis passes, AxVisor sync is confirmed, and the
      TF-card Linux rootfs is visibly restored read-write.
