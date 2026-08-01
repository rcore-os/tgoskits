# AxVisor real-time reference

This directory retains the final QEMU AArch64 feature-off/feature-on campaign
for AxVisor guest-vCPU CPU partitioning. `shared` allows both Linux vCPUs on
all four emulated pCPUs; `partitioned` pins them one-to-one to pCPU2 and pCPU3.
Every normal run records exactly 10,000 samples per metric after 100 warm-up
iterations at a 1 ms period. The partitioned stress soak uses the same sample
count at a 10 ms period, for three 100-second measured windows.

## Results

All latency values are nanoseconds. CPU percentages come from paired guest
`/proc/stat` snapshots around the complete measurement window.

| Profile/workload | CPU0 / CPU1 busy | Periodic jitter p99 / max | Dispatch p99 / max | Timer-IRQ proxy p99 / max |
| --- | ---: | ---: | ---: | ---: |
| shared idle | 35.563% / 0.142% | 236,864 / 1,183,648 | 164,704 / 398,832 | 229,424 / 433,488 |
| shared CPU stress | 2.124% / 100.000% | 245,120 / 694,096 | 148,240 / 541,376 | 226,608 / 438,800 |
| partitioned idle | 36.301% / 0.176% | 231,328 / 1,222,368 | 154,080 / 372,928 | 225,056 / 1,298,736 |
| partitioned CPU stress | 1.995% / 100.000% | 237,264 / 944,512 | 137,584 / 372,256 | 240,832 / 454,880 |
| partitioned CPU-stress soak | 1.667% / 100.000% | 333,072 / 6,690,800 | 145,440 / 275,760 | 280,720 / 6,388,560 |

Under the paired CPU-stress workload, partitioning reduced dispatch p99 by
7.19% and dispatch maximum by 31.24%. It also reduced periodic-jitter p99 by
3.20%, while the jitter maximum and both timer-IRQ proxy tails were worse.
Under idle, dispatch p99/maximum improved by 6.45%/6.49% and jitter p99 by
2.34%, but jitter maximum and timer-IRQ maximum were worse. The evidence
therefore supports deterministic placement and selected dispatch-tail
improvements, not a universal latency improvement or a hardware worst-case
bound.

The soak ran from `2026-07-31T00:21:24Z` to `00:34:24Z`; the measured loops
account for 300 seconds and the remaining interval is build, boot, warm-up,
between-metric setup, and shutdown. Its 6.69 ms observed maximum shows why
short-run percentiles must not be presented as a worst-case guarantee.

## Evidence contract

Each run has three run-specific retained artifacts:

- `*-summary.json`: validated metrics and a complete copy of the metadata;
- `*-metadata.json`: pre-run source/config/image/probe hashes, workload,
  affinity, UTC interval, and QEMU exit code; and
- `*-raw-console.log.gz`: deterministic gzip of the exact analyzed console.

The directory also retains the common 718,296-byte
[`axvisor-rt-probe`](axvisor-rt-probe) used by all five captures. It is a
statically linked AArch64 ELF with SHA-256
`8b3f6e7471dc9ecf60d5b64ab5f3c3a4657af8743fde1aa6b1772358c62806da`.

The metadata and summary store the SHA-256 of the **uncompressed** raw log.
Verify any retained log without changing its bytes:

```sh
sha256sum axvisor-rt-probe
gzip -cd shared-idle-raw-console.log.gz | sha256sum
```

The five expected uncompressed hashes, in table order, are:

```text
638c72d723ead40f7f4ca2ae5fb7362219c95e8bd9b482588035848f155003fd
5179ad02eba344606dff53853c312b295b89b7ae89135697fa68c194655590cc
0010d39af45494b01e359d9ddb9b85553591431ae5592bc8d608d169e5434d37
9361542d542a141462c1504d12cc450438e2f05fce0c5bd044731de7aff4d76c
729a04ad0572a14c0c268910dc73e739a40709f4f508835827e0b4f3767883c2
```

All five runs report QEMU exit code zero and source snapshot
`8594ab76e903dd179db5f1aa91546c03a7d759454d300b2ac6c665933ab0216a`
on base commit `263f89d8f3d0481d2712224a7b517a73b1165fb3`. The worktree was intentionally
dirty because the competition implementation was not yet committed. The
snapshot binds the tracked binary diff and the pruned untracked source
manifest; generated build trees, `tmp/`, and `competition/results` are not
treated as source.

The 2 GiB per-run rootfs copies and generated guest runner are not retained
here. Their pre-run hashes are preserved in every metadata file, and the runner
recreates them from the recorded inputs. The exact common static probe is
retained above; duplicate per-run copies and driver-console logs are omitted.

## Reproduction

From the repository root in the recorded environment, run each case into a
new directory. For example:

```sh
scripts/benchmark/axvisor-rt/run.sh \
  --rootfs tmp/axbuild/rootfs/rootfs-aarch64-alpine.img \
  --probe competition/results/axvisor-rt-reference/axvisor-rt-probe \
  --output tmp/competition/axvisor-rt/reproduction-shared-stress \
  --profile shared --iterations 10000 --warmup 100 \
  --period-us 1000 --workload cpu-stress

scripts/benchmark/axvisor-rt/run.sh \
  --rootfs tmp/axbuild/rootfs/rootfs-aarch64-alpine.img \
  --probe competition/results/axvisor-rt-reference/axvisor-rt-probe \
  --output tmp/competition/axvisor-rt/reproduction-partitioned-stress \
  --profile partitioned --iterations 10000 --warmup 100 \
  --period-us 1000 --workload cpu-stress

scripts/benchmark/axvisor-rt/run.sh \
  --rootfs tmp/axbuild/rootfs/rootfs-aarch64-alpine.img \
  --probe competition/results/axvisor-rt-reference/axvisor-rt-probe \
  --output tmp/competition/axvisor-rt/reproduction-partitioned-soak \
  --profile partitioned --iterations 10000 --warmup 100 \
  --period-us 10000 --workload cpu-stress
```

Use `idle` for the paired idle cases. The method, analyzer command, stress
lifecycle checks, and metric boundaries are documented in
[`scripts/benchmark/axvisor-rt/README.md`](../../../scripts/benchmark/axvisor-rt/README.md).

The timerfd metric is a Linux userspace observation of a virtual timer event;
it is not direct hypervisor interrupt-injection latency. QEMU TCG, WSL2 host
scheduling, different host activity, and serial/build activity outside the
individual sample intervals remain sources of run-to-run variation.
