# Test and evidence report

Report date: 2026-08-04. The retained QEMU evidence was produced from base
commit `263f89d8f3d0481d2712224a7b517a73b1165fb3` plus the then-uncommitted
competition implementation. The final AxVisor RT and QEMU IVC campaigns share
source snapshot SHA-256
`8594ab76e903dd179db5f1aa91546c03a7d759454d300b2ac6c665933ab0216a`.
The base commit alone does not contain the implementation. The retained
physical Orange Pi StarryOS captures used base commit
`f808646899f51fde9addfbe60976f6667c760beb` plus the later uncommitted worktree
recorded in `results/orangepi-starry-reference/metadata.json`. They are
reported separately rather than relabeling the historical QEMU archives. The
current same-board StarryOS manual/neural campaign is a distinct, preregistered
clean-commit capture from `f4ced37584964aba56e07ff060ae58374608bc26` and is
reported below without rewriting the historical archives.

## 1. Requirement status

| Requirement/evidence | Status | What the retained evidence establishes |
| --- | --- | --- |
| CPU-partition implementation | Complete | Global mask validation, maximum-matched initial placement, FDT CPU consistency, two-phase vCPU task preparation, activation-time revalidation, rollback, and frozen-registry behavior fail closed. |
| Linux guest with at least two vCPUs | Complete QEMU gate | The dedicated QEMU Task 1 gate requires exactly two online Linux CPUs. The physical replacement profile separately proves two online StarryOS vCPUs. |
| AxVisor idle/stress/soak validation | Complete | The physical StarryOS controlled-interference five-pair matrix and two >=30-minute soak runs pass the preregistered M2 gate. A separate five-pair guest CPU1 stress campaign is retained with its mixed dispatch-tail result and is not relabelled as isolation evidence. |
| Native RTOS comparison | Complete retained reference | Native Zephyr v4.3.0 runs comparable periodic/dispatch loops under idle and verified CPU stress, with platform differences stated. |
| Bidirectional guest IP path | Complete | Linux in QEMU or StarryOS on Orange Pi at `10.0.0.1` and Zephyr at `10.0.0.2` exchange CONTROL, STATUS, and ACK over UDP/IPv4 and two virtio-net devices on isolated segment 1. |
| Application protocol and reliability | Complete | Versioned framing, CRC, typed errors, receive window, retry, duplicate suppression, timeout, session restart logic, and safe fallback are implemented and tested. |
| Cross-guest normal communication | Complete formal physical campaign | Five StarryOS manual/neural pairs produce ten validated 1,800-command halves with zero application errors/timeouts/retransmissions/recoveries and zero RTOS duplicates/protocol errors. |
| Physical Orange Pi communication | Complete formal physical campaign | The preregistered ten-half campaign runs on board `bf61f4d4a1d994ad`, verifies `backend=native`, synchronizes and snapshots every result disk, and restores `/dev/mmcblk1p2` as ext4 `rw` after the campaign. |
| Cross-guest ACK-loss recovery | Complete formal physical campaign | Three deterministic 1-in-5 first-ACK loss runs each recover all 20 losses, suppress all 20 duplicate applications, and pass the exact fault and lifecycle gates. |
| Neural/RTOS closed loop and manual comparison | Complete formal physical campaign | StarryOS manual and native-neural policies run on the same Orange Pi, binaries, topology, trajectory, and sample count. RMSE and IAE favor neural in 5/5 pairs; overshoot and mixed latency results are disclosed below. |
| StarryOS replacement path | Complete for physical Tasks 1, 2, and 3 | Two-vCPU StarryOS runs the physical RT probes, Linux-ABI controller, virtio block/network, UDP protocol, neural inference, and closed-loop feedback. Linux/QEMU remains historical reference evidence rather than the sole Task 1/manual result. |
| Error notification | Complete formal physical campaign | Three runs each inject the five preregistered malformed classes, receive the exact cross-guest ERROR code/reason evidence, and then complete 100/100 normal commands. |
| Guest restart recovery | Complete formal physical campaign | Three actual VM-reset runs reject retired CONTROL and stale STATUS/ACK frames, observe safe fallback, establish a new session, and complete all post-reset commands. |
| Isolation/access control | Implemented and regression-tested | No host NIC/default route exists; segment separation, exact unicast, anti-spoofing, and secure unknown-unicast drop are unit-tested. No third-guest runtime negative capture is claimed. |
| Demonstration video | Outstanding | The storyboard is complete; the actual approximately five-minute recording is not. |
| Dev-target PR | Outstanding | The Windows/WSL local synchronization does not claim an upstream push, conflict check, or PR. |

## 2. AxVisor real-time campaign

### Method

The same source, QEMU 10.0.3 Cortex-A72 TCG machine, two-vCPU Linux image,
probe, and 1 ms measurement inputs are used for the four paired cases.
`shared` permits both vCPUs on pCPUs 0-3; `partitioned` assigns vCPU0 only
pCPU2 and vCPU1 only pCPU3. This is a feature-off/feature-on policy comparison,
not an unmodified-`dev` historical binary comparison.

Each metric retains exactly 10,000 samples after 100 warm-up iterations:

- `periodic_jitter`: lateness of absolute `clock_nanosleep` deadlines;
- `dispatch_latency`: eventfd signal to a higher-priority same-CPU reader; and
- `emulated_irq_response`: timerfd deadline to userspace resume, a virtual
  timer IRQ proxy rather than direct interrupt-injection latency.

Stress is a separately pinned busy-loop probe on guest CPU1. The analyzer
requires exact READY/ACTIVE/STOPPED/CLEANED PID/CPU/affinity records,
non-zombie liveness, explicit termination, and at least 50% CPU1 busy time.
The percentages below are guest CPU0/CPU1 `/proc/stat` load, not host-pCPU
utilization.

### Results

All latency cells are p99/maximum nanoseconds.

| Profile/workload | Guest load | Jitter | Dispatch | Timer-IRQ proxy | Raw-log SHA-256 |
| --- | ---: | ---: | ---: | ---: | --- |
| shared idle | 35.563% / 0.142% | 236,864 / 1,183,648 | 164,704 / 398,832 | 229,424 / 433,488 | `638c72d723ead40f7f4ca2ae5fb7362219c95e8bd9b482588035848f155003fd` |
| shared stress | 2.124% / 100.000% | 245,120 / 694,096 | 148,240 / 541,376 | 226,608 / 438,800 | `5179ad02eba344606dff53853c312b295b89b7ae89135697fa68c194655590cc` |
| partitioned idle | 36.301% / 0.176% | 231,328 / 1,222,368 | 154,080 / 372,928 | 225,056 / 1,298,736 | `0010d39af45494b01e359d9ddb9b85553591431ae5592bc8d608d169e5434d37` |
| partitioned stress | 1.995% / 100.000% | 237,264 / 944,512 | 137,584 / 372,256 | 240,832 / 454,880 | `9361542d542a141462c1504d12cc450438e2f05fce0c5bd044731de7aff4d76c` |
| partitioned stress soak | 1.667% / 100.000% | 333,072 / 6,690,800 | 145,440 / 275,760 | 280,720 / 6,388,560 | `729a04ad0572a14c0c268910dc73e739a40709f4f508835827e0b4f3767883c2` |

Partitioned stress improved dispatch p99/maximum by 7.19%/31.24% and jitter
p99 by 3.20%, but jitter maximum and both timer-IRQ proxy tails worsened.
Partitioned idle improved dispatch p99/maximum by 6.45%/6.49% and jitter p99
by 2.34%, while jitter maximum and especially timer-IRQ maximum worsened. The
claim is deterministic placement and selected dispatch-tail improvement, not
universal latency improvement.

The soak uses a 10 ms period: 10,000 samples give 100 seconds per metric and
300 seconds measured total. Its metadata interval is
`2026-07-31T00:21:24Z` through `00:34:24Z` (13 minutes), including build, boot,
warm-up, setup, transitions, and shutdown. The 6.69 ms largest observed jitter
is retained rather than hidden.

Complete summaries, per-run provenance, and compressed analyzer-input logs are
under [`results/axvisor-rt-reference`](results/axvisor-rt-reference/).

## 3. Final cross-guest normal runs

Both runs use four AxVisor pCPUs, dedicated Linux vCPUs on pCPUs1/2, Zephyr on
pCPU0, identical memory/device/network configuration, 1,800 commands, and a
100 ms nominal control period. The analyzer requires one terminal controller
record, final RTOS progress, exact counts, `IVC-LINUX-DONE exit=0`, successful
QEMU completion, monotonic percentile families, and zero normal-run fault
counters.

| Metric | Manual fixed | Neural |
| --- | ---: | ---: |
| Sent / acknowledged | 1,800 / 1,800 | 1,800 / 1,800 |
| Application errors / timeouts | 0 / 0 | 0 / 0 |
| Retransmissions / recoveries | 0 / 0 | 0 / 0 |
| RTOS accepted / duplicates / protocol errors | 1,800 / 0 / 0 | 1,800 / 0 / 0 |
| Full-loop p50 / p95 / p99 / max | 3,902 / 4,670 / 5,423 / 19,656 us | 3,894 / 4,652 / 5,657 / 20,917 us |
| Pre-send p50 / p95 / p99 / max | 3 / 4 / 8 / 269 us | 13 / 17 / 45 / 376 us |
| Transport p50 / p95 / p99 / max | 3,898 / 4,667 / 5,420 / 19,555 us | 3,880 / 4,632 / 5,645 / 20,622 us |
| Effective throughput | 9.963 msg/s | 9.962 msg/s |
| RMSE | 9,258.906 mC | 5,932.491 mC |
| Integrated absolute error | 1,429,224.700 mC*s | 686,993.400 mC*s |
| Maximum overshoot | 6,840 mC | 13,428 mC |

Neural control improves RMSE by 35.93% and integrated absolute error by
51.93%, while maximum overshoot is worse. The weights are checked-in,
hand-parameterized 4x6x1 dense/ReLU controller parameters; there is no external
training dataset claim.

For Task 2, `transport_*` covers pre-send serialization plus the UDP/IP,
virtio, RTOS action/status, ACK, and response-decode path. For Task 3,
`full_loop_*` additionally starts before observation construction and selected
policy inference. Both are Linux same-clock round trips; they do not subtract
unrelated guest clock epochs, and recorded resolution is one microsecond.

The neural raw-log SHA-256 is
`6c7f7e2e404a5c8ef8a9a3f632a24169b35d8be6a8c0ac496775bf9d32a07eb8`;
manual is
`39ac8deaf5382490a007bfd47ec7384989c64c6092eed70ac8ff682c076d8a57`.
Both compressed logs and summaries are retained in
[`results/axvisor-ivc-reference`](results/axvisor-ivc-reference/).

## 4. Physical Orange Pi 5 Plus validation

### Retained single-run reference

The physical profile ran on an RK3588 Orange Pi 5 Plus with 16 GiB DRAM. WSL2
owned the CH340 serial connection at 1,500,000 baud, built and staged the
StarryOS kernel, guest DTB, finite ext4 rootfs, and Zephyr v4.3.0 image over SSH
while holding the board lease, started AxVisor through U-Boot, and restored the
TF-card Linux system afterward. StarryOS reported two online vCPUs and ran the
Linux-ABI neural controller at `10.0.0.1`; Zephyr used `10.0.0.2`.

The full neural run completed the same 1,800-command plant trajectory used by
the QEMU neural profile:

| Metric | Physical result |
| --- | ---: |
| Sent / acknowledged | 1,800 / 1,800 |
| Errors / timeouts / retransmissions / recoveries | 0 / 0 / 0 / 0 |
| RTOS accepted / applied / STATUS / ACK | 1,800 / 1,800 / 1,800 / 1,800 |
| RTOS duplicates / protocol errors | 0 / 0 |
| Full-loop p50 / p95 / p99 / max | 6,751 / 11,265 / 11,695 / 14,405 us |
| Pre-send p50 / p95 / p99 / max | 17 / 17 / 17 / 35 us |
| Transport p50 / p95 / p99 / max | 6,734 / 11,249 / 11,678 / 14,388 us |
| Effective throughput | 9.995 msg/s |
| RMSE / integrated absolute error | 5,932.491 mC / 686,993.400 mC*s |
| Maximum overshoot | 13,428 mC |

The maintained 20-command smoke also passed: 20/20 acknowledgements, zero
controller or RTOS faults, full-loop p50/p95/p99/max
`4,694/6,952/6,952/8,915 us`, transport
`4,677/6,936/6,936/8,899 us`, and `9.970 msg/s`. In both runs, the analyzer
required the two-vCPU StarryOS boot record, StarryOS network setup, complete
controller and RTOS metrics, StarryOS completion, Zephyr poweroff, AxVisor
filesystem-sync confirmation, and final Linux restoration. The board returned
to kernel `6.1.43-rockchip-rk3588` with `/dev/mmcblk1p2` ext4 `rw`.

The full raw-console SHA-256 is
`023ff07b40b4936453eee6d4bbd57bca1c1699e7305dc1af5fe601a5d67492d9`;
the smoke raw-console SHA-256 is
`8dd16dbcc7608305da9fcf13f393a54e410e16ef26da63ca5c2821878efbf265`.
Deterministically compressed logs, generated JSON summaries, artifact and
configuration hashes, timestamps, and reproduction commands are retained in
[`results/orangepi-starry-reference`](results/orangepi-starry-reference/).

Multiple guest and host consoles share the physical UART and can lose spans.
Controller and RTOS terminal metrics are therefore split into short records,
emitted twice with pacing, and accepted only when at least one complete copy
exists and all complete copies agree. The analyzer never infers omitted
metrics from a completion marker. These are one full and one smoke hardware
observation, not a repeated statistical campaign.

### Formal same-board StarryOS control campaign

The formal campaign
[`starry-ivc-control-formal-20260804-v5`](results/orangepi-5-plus/starry-ivc-control-formal-20260804-v5/)
was preregistered before capture and ran from clean commit
`f4ced37584964aba56e07ff060ae58374608bc26`. It used the frozen
AB/BA/AB/BA/AB order on physical board `bf61f4d4a1d994ad`. All five pairs and
all ten halves passed: each half contains 1,800 contiguous raw samples,
1,800/1,800 acknowledgements, `starry.backend=native`, zero application
errors/timeouts/retransmissions/recoveries, zero RTOS duplicates/protocol
errors, and a verified manifest/raw/gzip/lifecycle chain. The final board check
again found `/dev/mmcblk1p2` mounted as ext4 `rw` and passed a synchronized
write/read/remove probe.

The profile-level values below are medians across the five runs except where a
worst-of-runs value is shown. A positive paired delta is defined as favorable
to neural for the lower-is-better metrics.

| Metric | Starry manual | Starry neural native | Paired result |
| --- | ---: | ---: | --- |
| Valid full runs / samples per run | 5 / 1,800 | 5 / 1,800 | 10/10 halves validated |
| RMSE (mC) | 9,258.906 | 5,932.491 | neural lower by 35.93%; favorable in 5/5 |
| IAE (mC*s) | 1,429,224.7 | 686,993.4 | neural lower by 51.94%; favorable in 5/5 |
| Maximum overshoot (mC) | 6,840 | 13,428 | neural higher by 96.32%; unfavorable in 5/5 |
| Full-loop p99 median / worst (us) | 11,721 / 11,995 | 11,742 / 12,000 | neural favorable in 2/5; median paired delta -5 us |
| Full-loop max median / worst (us) | 104,349 / 126,733 | 105,435 / 108,442 | neural favorable in 3/5; median paired delta +1,180 us |
| Deadline misses per run | 1 | 1 | equal in 5/5 |
| Throughput median (msg/s) | 9.994927 | 9.994888 | effectively equal; neural favorable in 3/5 |

The defensible conclusion is therefore limited: the fixed neural policy
consistently improves RMSE and IAE on this deterministic trajectory, but it
does not improve overshoot and does not establish a latency advantage. In
particular, the lower neural worst-of-runs full-loop maximum must not be used to
hide the mixed 3/5 pairwise maximum or 2/5 p99 directions.

The frozen preregistration SHA-256 is
`88233934bc4080ee3695951ffda2d27ebf235c6a9d389ba83c3015afcf913776`;
the independently reproducible `campaign-summary.json` SHA-256 is
`1dd3f8ff52a09fd795395c7fab19587de0786b7b014e9df4f0efb08b502aca62`.
An earlier complete capture,
[`starry-ivc-control-formal-20260804-v4`](results/orangepi-5-plus/starry-ivc-control-formal-20260804-v4/),
is deliberately excluded: aggregation failed because the analyzer omitted the
already observed `backend=native` field from `summary.starry`. The original ten
raw captures and failure marker remain unchanged. A regression-first fix added
the required field at the clean v5 commit; v5 was newly preregistered and
captured rather than reconstructed from v4.

## 5. Cross-guest fault campaigns

### Retained single-run ACK-loss reference

The fault Zephyr image suppresses only the first ACK for each selected fresh
sequence while returning STATUS. Linux retransmits after 100 ms; Zephyr treats
that command as a duplicate and returns STATUS plus ACK without reapplying the
actuator or stepping the plant.

| Metric | Result |
| --- | ---: |
| Sent / acknowledged | 100 / 100 |
| Application errors / terminal timeouts | 0 / 0 |
| Retransmissions / recoveries | 20 / 20 |
| Fresh accepted / applied | 100 / 100 |
| ACKs dropped / duplicates suppressed | 20 / 20 |
| STATUS / ACK / ERROR frames sent | 120 / 100 / 0 |
| RTOS protocol errors | 0 |
| Full-loop p50 / p95 / p99 / max | 3,953 / 110,808 / 111,484 / 111,548 us |
| Effective throughput | 9.769 msg/s |

The analyzer verifies the identical exact injection and duplicate sequence set
`{5, 10, 15, ..., 100}`, terminal counters, ordering, and source-log SHA-256
`f15c88c6671db67934ce178e3f113b65ac2811a1538a0c36412f6c156bd279fd`.
The 100 ms retry delay intentionally dominates p95 and above. The terminal log
also observes controller-silence safe fallback to actuator zero.

### Formal ACK-loss campaign

The physical
[`starry-ivc-ack-loss-formal-20260803`](results/orangepi-5-plus/starry-ivc-ack-loss-formal-20260803/)
campaign completed 3/3 registered runs. Each run contains 100 contiguous
samples and the exact `{5, 10, ..., 100}` loss set, with 20 retransmissions, 20
recoveries, 20 duplicate receives, 100 controller acknowledgements, and only
100 RTOS applications. Every manifest, gzip twin, snapshot/fsck, lifecycle,
Linux-restoration, and final board-pool gate passed.

### Formal malformed/ERROR campaign

The physical
[`starry-ivc-error-formal-20260804`](results/orangepi-5-plus/starry-ivc-error-formal-20260804/)
campaign completed 3/3 registered runs. Each run observed exactly one version,
length, CRC, message-type, and session-transition fault with matching sequence,
ERROR code, and reason evidence from the controller and Zephyr. After injection,
each run completed 100/100 normal commands with zero controller faults and
exactly five ERROR/protocol-error responses. All lifecycle and Linux-restoration
gates passed.

### Formal guest-restart campaign

The physical
[`starry-ivc-restart-formal-20260804`](results/orangepi-5-plus/starry-ivc-restart-formal-20260804/)
campaign completed 3/3 registered runs from clean commit
`6adf49e09ce91b53d2573cb8d34c60dc6a9ec47c`. In every run, VM 1 was actually
reset after 20 pre-reset commands, safe fallback was observed, and a new
session completed 100 post-reset commands. The exact contract rejected one
retired CONTROL and ignored one stale STATUS and one stale ACK per run; no old
session data entered the new session. Manifest/raw/gzip/lifecycle and final
Linux ext4 `rw` gates all passed. The campaign-summary SHA-256 is
`935db25de96c83267b8d11ea8e55a2909a42a07ca8f2614cef687dce153e2302`.

## 6. Native Zephyr real-time baseline

The native comparison runs upstream Zephyr v4.3.0 directly on QEMU
`qemu_cortex_a53`, without AxVisor or a Linux guest. Both cases use a 1 ms
absolute period, 100 warm-up expirations, 10,000 measured deadlines, no console
output during measurement, and explicit runtime-accounted load.

| Workload | Metric | p50 | p99 | p99.9 | Maximum | Actual duration |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| idle | periodic wake lateness | 65,104 ns | 186,256 ns | 597,792 ns | 841,264 ns | 10,000,197 us |
| idle | timer-to-task dispatch | 22,896 ns | 40,288 ns | 101,280 ns | 162,896 ns | 10,000,197 us |
| CPU stress | periodic wake lateness | 65,472 ns | 669,408 ns | 3,943,120 ns | 6,236,608 ns | 10,000,313 us |
| CPU stress | timer-to-task dispatch | 26,368 ns | 134,912 ns | 281,024 ns | 1,141,536 ns | 10,000,313 us |

The idle case reports 988 permille idle; stress reports 1,000 permille
non-idle and 985 permille stress work. The final isolated runs recorded zero
idle coalescings and 68 measured stress coalescings; every coalesced deadline
remains in the fixed sample set. The validated summaries are retained under
[`results/native-zephyr-reference`](results/native-zephyr-reference/).
The exact console and build logs are retained there as deterministic gzip
streams. The console records are post-measurement aggregates: the native
benchmark did not serialize an individual-sample series, so the summaries
cannot be independently recomputed from per-sample native data.

This is an equivalent-method comparison, not the same platform: CPU model,
CPU count, OS, timer/dispatch implementation, virtualization boundary, and
stress placement differ. It is neither an AxVisor result nor a hardware bound.

## 7. Supplemental host evidence

The host UDP reference uses the same Rust protocol/state machines and 100
commands with first-ACK loss every fifth sequence. It records 100/100 success,
20 recoveries, 20 exactly-once duplicate suppressions, and no terminal error.
It is useful regression evidence but is not labeled cross-guest or Zephyr
latency. See [`results/host-network-reference`](results/host-network-reference/).

The host restart reference accepts sequence 1 from one session, then sequence
1 from a new controller session, and rejects delayed traffic from the retired
session. It does not demonstrate a cross-guest reconnect. See
[`results/host-restart-reference`](results/host-restart-reference/).

The deterministic host CSV independently binds the 1,800-step manual/neural
plant comparison and is retained under
[`results/host-ai-reference`](results/host-ai-reference/). It is functional
cross-check evidence, not guest timing.

## 8. Isolation and protocol assurance

The full profile has no host NIC, default route, bridge, NAT, vsock data path,
shared-memory channel, or hypercall application channel. Its network identities
are fixed as follows:

| Endpoint | MAC | IPv4 | UDP role |
| --- | --- | --- | --- |
| Linux controller | `52:54:00:00:00:01` | `10.0.0.1/24` | ephemeral source port to `10.0.0.2:5500` |
| Zephyr endpoint | `52:54:00:00:00:02` | `10.0.0.2/24` | listens on UDP port `5500` |

Linux installs only the connected `10.0.0.0/24` route. The guest init script
installs no firewall rule, and no host firewall rule is part of this experiment
because the segment has no host-facing interface. Isolation is instead enforced
at the AxVisor switch boundary by segment membership, exact unicast identity,
source-MAC anti-spoofing, and secure unknown-unicast drop.

Regressions cover no cross-segment delivery, no unknown-unicast flood, source
anti-spoofing, no reflected unicast, same-segment multicast only, duplicate
identity rejection, bounded topology, and virtio descriptor/address/header
validation. This is strong policy and parser/driver evidence, but not a runtime
penetration test with a malicious third guest.

The Rust and C protocol suites cover version, type, payload length, session,
sequence/timestamp, error code, CRC, exact payloads, malformed input, typed
ERROR behavior, ACK retry, duplicate/out-of-order handling, session restart,
and safe fallback. CONTROL, STATUS, ACK, all five registered malformed ERROR
classes, deterministic ACK loss, and actual guest restart are now demonstrated
on the physical cross-guest path. A malicious third-guest runtime capture is
still not claimed.

## 9. Executed validation

The final implementation sweep completed against the synchronized working tree:

- the AxVisor RT harness passed 24 Python tests plus its shell/C integration
  test; the IVC harness passed 28 Python contract tests and the strict Zephyr
  host-logic C suite; the isolated Zephyr baseline passed five Python and seven
  C tests;
- syntax/static harness checks passed for nine Bash scripts, two POSIX shell
  scripts, ten Python modules, and the applicable shellcheck rules;
- all five full-profile `axvmconfig check --config-path` invocations returned a
  valid configuration;
- the final `axvm`/`axvm-types` run passed 107 `axvm` unit tests, 18 architecture
  boundary tests, 20 focused error/FDT/passthrough/vCPU integration tests, two
  `axvm-types` unit tests, three `axvm-types` error-contract tests, and doc tests;
- the duplicate secondary `CPU_ON` regression was demonstrated failing before
  the startup-reservation fix, then passing in the four-test initial-placement
  contract suite after the fix;
- focused suites passed for GICv3, `ax-task`, `somehal`, `arm_vcpu`, virtio-net,
  virtio-blk, the virtual switch, `axdevice`, `arm_vgic`, `axvmconfig`, and
  `ivcproto`, including parser, isolation, DAIF/entry-hook, passthrough-SPI,
  PSCI, virtual-timer, stage-2 memory, restart, and ACK-recovery contracts;
- targeted `cargo xtask clippy --package ...` checks passed across 37 relevant
  package/feature combinations, including the final `axvm-types` and `axvm`
  variants. The exact target-configured release AxVisor command also passed
  Clippy with `-D warnings`, and
  `cargo +nightly-2026-07-15 fmt --all -- --check` passed;
- final-tree AArch64 AxVisor Linux+Zephyr compilation passed, followed by the
  dedicated two-vCPU Linux QEMU gate (`1/1` pass with
  `AXVISOR_DEDICATED_PARTITION_PASS`); a final RISC-V SMP compile-only AxVisor
  build also passed;
- both normal/fault Zephyr v4.3.0 endpoint builds passed with verified
  entry/load layout; and
- all 24 retained JSON records parsed, all 14 gzip archives passed integrity
  validation, all documented relative links resolved, retained-artifact hashes
  matched their metadata, and the final `git diff --check` passed.

The later physical-board additions additionally passed the prepublished-GIC
backend deadlock regression, shared vCPU startup helper contracts, six
parallel-safe PL011 integration tests, GICv3 redistributor and guest-FDT
idle-state regressions, both Orange Pi success-regex contracts, four physical
VM-config checks, and the repository-owned `arm_vcpu` umbrella flow with its
`.axci` fixture. `ivcproto` passed 21 library and four binary tests, including
the physical UART record budget/redundancy contract, and both of its Clippy
feature configurations. Both finite Zephyr board images and both physical
AxVisor profiles compiled.

The maintained board runner then passed one strict 20-command smoke and one
strict 1,800-command full neural run. The current analyzer was rerun over both
decompressed retained logs: uncompressed hashes matched metadata, generated
metrics matched the retained summaries exactly, and both lifecycle checks
proved AxVisor filesystem sync plus automatic TF-card Linux restoration.

The subsequent IVC implementation sweep passed 126/126 Python and host-logic
tests, including the fail-first regression for retaining and requiring the
StarryOS backend identity. The current analyzer replayed all ten v4 raw
UART/CSV pairs as `backend=native`; those replays diagnosed the analyzer defect
but were not substituted for formal capture. The fresh v5 archive then passed
all ten per-run analyzers, campaign aggregation, final Linux-root verification,
and an independent reaggregation from the canonical Windows archive. The
reaggregated campaign summary was byte-identical to the captured summary.

The retained five-run RT comparison and three-run QEMU IVC campaign remain
pinned to the source/config/image hashes recorded with those measurements. The
later secondary-CPU startup hardening was validated by the complete host
contract suites, both final target builds, and the dedicated two-vCPU QEMU
gate; the performance campaign was not silently relabelled as a
final-working-tree capture.

## 10. Measurement limits and remaining work

QEMU TCG, WSL2 host scheduling, host activity, guest scheduling, and platform
differences contribute to observed tails. Serial output is outside individual
RT sample intervals; IVC progress logging can still perturb surrounding guest
scheduling. At 1,500,000 baud, long records on the shared physical UART may
lose spans even when software print locking prevents interleaving. Physical
automation therefore requires redundant paced short records, rejects
conflicting complete copies, and separately validates lifecycle markers;
missing metrics are never inferred from a completion line. Reported maxima are
observed sample maxima, not proven WCET or hardware bounds. The timerfd metric
is a userspace IRQ-response proxy, not direct injection/handler timing. FIFO
CPU partitioning does not establish bounded preemption of a non-yielding
passthrough guest or isolate every host task/physical interrupt.

The three technical tasks, reproducible commands, source/config/image hashes,
same-board manual/neural baseline, and all three formal physical fault profiles
are present. Formal remaining deliverables are:

1. complete M4's deterministic ONNX source, RKNN NPU path, and ONNX Runtime CPU
   feasibility gate without presenting CPU emulation as NPU execution;
2. record the actual approximately five-minute demonstration video; and
3. when authorized, verify a conflict-free dev target, push, and submit the
   required PR.

A third-guest runtime isolation capture would strengthen the policy evidence,
but is not misrepresented as existing or treated as a completed artifact here.
