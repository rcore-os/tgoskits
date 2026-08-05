# Approximately five-minute demonstration storyboard

This is a recording plan and evidence checklist. It is **not** a generated or
completed video. The primary demonstration is the retained physical Orange Pi
5 Plus matrix: a two-vCPU StarryOS guest executes native, ONNX Runtime CPU, or
RKNN NPU neural control and drives a Zephyr guest over isolated UDP/IP on
AxVisor. Same-board manual/neural, ACK-loss, malformed-ERROR, restart, and
real-time campaigns provide the scored comparisons; QEMU Linux and native
Zephyr results remain clearly labeled historical references. The actual video
still has to be recorded.

## Before recording

- Use a clean committed worktree. For the frozen ORT campaign, show source
  `0110647de52f5e2ad6b550cb594780d7506ffecf`; if the recording uses a later
  documentation commit, also show that commit and explain that it did not
  replace the frozen run source.
- Pre-build and stage the StarryOS kernel, native/RKNN/ORT finite rootfs
  images, guest DTBs, Zephyr board image, models, and AxVisor binary; retain
  their hashes and build logs.
- Replay an uncut retained run or the campaign host log. A new five-run formal
  campaign is unnecessary unless executable artifacts change. If a live run is
  used, invoke the maintained runner and retain the complete serial log and
  generated JSON independently of the screen recording.
- Prepare two readable terminal panes: one following StarryOS/controller
  lines, the other following Zephyr/AxVisor lines from the same CH340 capture.
  State that these are filtered views of one shared physical UART.
- Prepare the exact formal directories used on screen: native manual/neural
  v5; ACK-loss, ERROR, and restart; RKNN NPU v8; ORT CPU v4; real-time
  idle/stress/soak; and the separately labeled native baseline.
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
- Verify the ORT v4 preregistration, summary, and campaign-checksum hashes:
  `04768defc09ce5e9a0069ead59bd01ea9fc696b32f46fdcd3619797327beded4`,
  `57edb5f8a1fc79bcbd43fb3fd77aec25151e7d773985a56b12e6d3530d14d3f9`,
  and `601b435f376841dcfbb54e0c8bbac5fd9e6ffb09e4c08c4f67e73f2934d85a25`.
  Show ORT model hash
  `3582869baf9b8cec722208d06f66acd680a64128b52875d22e7f0e43f2ed7887`,
  RKNN model hash
  `2ad3fecedc9767ee57cbcd31787f70297a8f8e2cfcdc8e07b81b949566d53bb8`,
  and the exact provider/Runtime records rather than relying on filenames.
- Generate RT plots from retained sample series and control plots from the
  formal physical raw CSVs. If the older
  `results/host-ai-reference/raw.csv` plot is also shown, label it as host-only
  functional evidence. The native Zephyr console retains aggregate records
  only; do not reconstruct or imply a native sample-series plot.
- Disable notifications and enlarge terminal fonts; keep command lines and
  timestamps visible.

## Timeline

### 0:00-0:25 — Goal and provenance

Show the title, repository revision, Orange Pi/RK3588 platform, WSL2 automation
host, U-Boot, Rust toolchain, Zephyr version/compiler, and image/model hashes.
State the one-sentence goal: two-vCPU StarryOS control of a Zephyr endpoint
over isolated UDP/IP on AxVisor, with native/ORT-CPU/RKNN-NPU inference and
unattended boot, analysis, filesystem sync, and Linux recovery.

Evidence on screen:

```sh
git show --no-patch --oneline 0110647de52f5e2ad6b550cb594780d7506ffecf
git status --short
rustc +nightly-2026-07-15 --version
sha256sum \
  competition/results/orangepi-5-plus/ort-control-full-formal-20260805-v4/ort-full/{preregistration.json,campaign-summary.json,campaign-checksums.sha256}
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

Show the single-source model branch: fixed JSON weights generate Rust/ONNX;
ONNX produces `.ort` for `CPUExecutionProvider` and `.rknn` for RKNN Runtime.
For RKNN only, point out host-owned NPU power/clock/reset initialization, three
guest core-MMIO windows and identity DMA, with no passed-through NPU IRQ,
IOMMU, PMU, or CRU.

Say the limitation aloud: guest-vCPU partitioning does not yet isolate every
AxVisor task/interrupt, and RR guest preemption is not claimed.

### 1:05-1:45 — Boot both guests

Run or replay an uncut capture of the physical full command in
[`reproduce.md`](reproduce.md). Show U-Boot entering AxVisor, AxVisor accepting
the partition, the `IVC-STARRY-BOOT` record ending in `vcpus=2`, StarryOS
configuring `10.0.0.1/24`, the Zephyr golden-vector/MAC checks, and
`IVC-RTOS-READY bind=10.0.0.2:5500`. In an ORT run, show Runtime `1.25.0`,
`CPUExecutionProvider`, and the model hash; in the adjacent RKNN evidence, show
Runtime `2.3.2`, driver `0.9.8`, `/dev/dri/card1`, positive device time, and
`host_submit=false`. Finish with strict analysis, snapshot fsck, AxVisor
filesystem sync, and restored Linux `/dev/mmcblk1p2 ext4 rw`.

Do not use an edited success marker without keeping the original complete log.
If either guest fails, stop the recording and fix/rerun instead of narrating it
as success.

### 1:45-2:35 — Bidirectional protocol and reliability

Use split filtered views of the same run:

- left: StarryOS CONTROL sequence, returned STATUS, and aggregate result;
- right: Zephyr applied sequence, actuator/temperature, ACK, and counters.

Overlay the 32-byte header fields briefly. Show that the physical analyzer
requires matching controller and RTOS counters instead of trusting a terminal
success line. For ORT v4, explain the frozen terminal protocol: compact metric
records are emitted twice; after a 500 ms drain, Zephyr emits five short
poweroff records at 100 ms spacing because all consoles share one UART. A
complete copy is required and conflicting complete copies fail analysis.

Replay one run from each physical fault campaign: deterministic first-ACK
loss and exact-once duplicate suppression; one of each of the five malformed
classes producing its typed ERROR followed by normal recovery; and an actual
guest reset showing safe fallback, stale-frame rejection, a new session, and
post-reset control. These are 3/3 physical StarryOS/Zephyr campaigns, not
host/unit substitutes.

End this section with the five-run aggregate request success, errors, timeouts,
recoveries, cold-start/steady deadline partition, full-loop percentiles and
maximum, throughput, and matching controller `sent`/`acknowledged` plus RTOS
`accepted`/`applied` counters—not the host loopback reference. Show virtual-switch
forwarding, anti-spoof, and drop-policy counters from the focused
unit/regression evidence unless a new runtime metrics snapshot is recorded;
the retained QEMU runs do not provide runtime switch totals.

### 2:35-3:45 — Neural closed loop

Show one complete sample path:

```text
temperature/status -> 4 inputs -> 4x6x1 inference -> actuator CONTROL
-> Zephyr applies/steps plant -> STATUS -> next observation
```

Use a physical StarryOS run for the neural path and show live actuator and
measured temperature changes. Then show the same-board five-pair fixed
500-permille/native-neural summary: RMSE and IAE improve in 5/5 pairs, maximum
overshoot is worse in 5/5, and latency direction is mixed. Derive any control
time-series plot from a displayed formal physical raw CSV. If the older host
functional plot or its 27.9-second settling value is shown, label it
prominently as host-only and do not merge it into physical timing claims.

Show the physical input-before-inference to matching-status latency definition
and its p50/p95/p99/maximum. Then compare the formal ONNX-derived backends:
both complete 9,000/9,000 ACK; RKNN NPU full-loop p99 is
`13456..13611 us`, while ORT CPU is `12023..12273 us`. Each has five misses,
all at sequence 1, and zero misses in the remaining 8,995 cycles. Say plainly
that the tiny model proves NPU offload but not acceleration; do not label ORT
as NPU or compare RKNN device time and ORT wall time as identical instruments.

The retained `full_loop` metric includes
observation, StarryOS policy evaluation, encoding, transport, RTOS
application/plant step, returned STATUS plus ACK, and response decoding; keep
the `pre_send` and `transport` sub-intervals visibly distinguished.

### 3:45-4:35 — Real-time validation

Lead with the physical StarryOS controlled-interference five-pair matrix and
the two physical soak runs, then show the guest CPU1-stress matrix with its
mixed dispatch-tail outcome. Use the retained QEMU guest probe definitions and
native Zephyr baseline only as explicitly labeled supporting references. Show
actual duration, pCPU/vCPU affinity, CPU load distribution, p99, and maximum.

Show the qualified physical scenario where worst latency improves and the
scenarios where it does not; do not turn one favorable pair into a universal
claim. For the historical QEMU matrix, state its mixed outcome: partitioned
stress improved dispatch p99/maximum by 7.19%/31.24%, while jitter maximum and
both timer-IRQ proxy tails worsened. Keep the 300-second measured soak and its
6.69 ms largest observed jitter visible.

Label QEMU TCG results as relative engineering measurements and the timerfd
metric as a userspace IRQ-response proxy, not a direct hypervisor injection
measurement.

### 4:35-5:00 — Reproduction and conclusion

Show the `competition/` entry page, WSL2 image/staging commands, physical board
run/campaign command, independent checksum/reaggregation commands, strict
summary, and retained result directories. Recap only claims backed by the
displayed artifacts:

- two-vCPU StarryOS boot and deterministic guest placement on Orange Pi;
- isolated IP-based bidirectional StarryOS/Zephyr link;
- reliable typed protocol and safe fallback;
- same-board manual/neural and three physical fault campaigns;
- one fixed model executing as native CPU, ORT CPU, and real RK3588 NPU;
- observable neural-to-RTOS physical closed loop and automatic Linux restore;
  and
- scenario-limited real-time improvements with separately labeled historical
  QEMU/native-RTOS comparisons.

Close with any remaining limitations. Keep the final frame on the source hash,
reproduction guide, and test report.

## Final integrity checklist

- [ ] Video duration is close to five minutes.
- [ ] Clean recording revision and each frozen campaign source revision are
      distinguished on screen.
- [ ] Commands shown match retained metadata.
- [ ] ORT preregistration/summary/campaign checksums and the independently
      reproduced summary are shown.
- [ ] StarryOS visibly has two online vCPUs on the physical board.
- [ ] Both guest MAC/IP identities and UDP port are visible.
- [ ] Data visibly flows in both directions over IP.
- [ ] Retry, duplicate suppression, error notification, timeout safe fallback,
      and recovery are demonstrated.
- [ ] ACK-loss, malformed-ERROR, and actual restart are labeled as separate
      3/3 physical campaigns.
- [ ] Neural inference is visibly upstream of the network command.
- [ ] Native CPU, ORT `CPUExecutionProvider`, and RKNN hardware-NPU identities
      are shown without calling ORT an NPU backend.
- [ ] RTOS control action and status feedback are visible.
- [ ] Manual and neural runs use the same scenario.
- [ ] Cross-guest aggregate control metrics are shown; each sample-series plot
      names its physical or host-only raw CSV provenance.
- [ ] Full-loop latency includes inference and states its clock/error method.
- [ ] The five sequence-1 cold-start misses and zero misses over the remaining
      8,995 cycles are both shown; no sample is silently discarded.
- [ ] NPU offload is demonstrated without claiming tiny-model acceleration or
      equating RKNN device time with ORT wall time.
- [ ] Idle, stress, and soak results show sample-log provenance; native RTOS
      results show exact aggregate-console provenance and the no-sample-series
      limitation.
- [ ] QEMU/IRQ/partition limitations are spoken or shown.
- [ ] No host-loopback number is labeled cross-guest.
- [ ] No planned/template value is presented as measured.
- [ ] The complete unedited console log and all plotted raw data are archived.
- [ ] ORT v4 compact terminal metrics are shown as two copies and RTOS poweroff
      as five copies after the 500 ms drain; corruption is not edited into a
      pass.
- [ ] Strict physical JSON analysis passes, AxVisor sync is confirmed, and the
      TF-card Linux rootfs is visibly restored read-write.
