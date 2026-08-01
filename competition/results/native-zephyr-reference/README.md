# Native Zephyr real-time reference

This directory records one idle and one sustained CPU-stress run of the native
Zephyr v4.3.0 baseline on QEMU `qemu_cortex_a53`. It is a comparison control
without AxVisor or guest virtualization; it is not an AxVisor result, a
cross-guest result, a hardware real-time guarantee, or a long-duration soak.

## Results

Both cases use a 1 ms absolute period, discard 100 warm-up expirations, and
retain exactly 10,000 measured deadlines. Values below are nanoseconds, except
for duration.

| Workload | Metric | Count | Min | Mean | p50 | p90 | p99 | p99.9 | Max | Actual duration |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| idle | periodic wake lateness | 10,000 | 40,864 | 74,623 | 65,104 | 102,224 | 186,256 | 597,792 | 841,264 | 10,000,197 us |
| idle | timer-to-task dispatch | 10,000 | 6,176 | 22,117 | 22,896 | 28,976 | 40,288 | 101,280 | 162,896 | 10,000,197 us |
| CPU stress | periodic wake lateness | 10,000 | 31,712 | 97,118 | 65,472 | 113,376 | 669,408 | 3,943,120 | 6,236,608 | 10,000,313 us |
| CPU stress | timer-to-task dispatch | 10,000 | 6,032 | 27,825 | 26,368 | 32,656 | 134,912 | 281,024 | 1,141,536 | 10,000,313 us |

Zephyr runtime accounting verified the selected workload:

| Workload | Load window | Non-idle | Idle | Benchmark | Stress | Verified stress work |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| idle | 10,100,820 us | 11 permille | 988 permille | 10 permille | 0 permille | 0 blocks |
| CPU stress | 10,101,056 us | 1,000 permille | 0 permille | 14 permille | 985 permille | 7,916,855 blocks (783,765/s) |

The final completion records report coalescing in the measured and warm-up
windows separately: idle recorded 0 measured and 0 warm-up coalescings; stress
recorded 68 measured and 0 warm-up coalescings. Every coalesced measured
deadline remains in the fixed 10,000-sample set with the eventual task
observation timestamp, which exposes rather than hides the tail. The stress
run has higher mean, p99, p99.9, and maximum wake lateness, but one QEMU capture
is insufficient for a general performance claim.

## Evidence and reproduction

- [`idle-summary.json`](idle-summary.json) and
  [`stress-summary.json`](stress-summary.json) are validated, tracked summaries.
- [`source-provenance.json`](source-provenance.json) is the post-run clean-tree
  attestation reused by both analyzers.
- [`metadata.json`](metadata.json) records source/tool versions, original capture
  paths, retained gzip paths and hashes, method, and limitations.
- The exact console and build logs are retained as deterministic `gzip -n -9`
  streams. Generated binaries and `.config` files remain outside this directory;
  their byte counts and hashes remain in the validated summaries.

| Workload | Artifact | Retained gzip | Uncompressed bytes | Uncompressed SHA-256 | Gzip bytes | Gzip SHA-256 |
| --- | --- | --- | ---: | --- | ---: | --- |
| idle | console | [`idle-qemu.log.gz`](idle-qemu.log.gz) | 1,362 | `20a6901db7395f37b7bfdae110ef63d69235ed8fd11b52d1e2a92565ef1b57b1` | 649 | `cb861bb09c980b1099520057f587c4a152836a3b8a75696e54d4d1d572c51ce7` |
| idle | build | [`idle-build.log.gz`](idle-build.log.gz) | 17,129 | `f5e9bfe4f78413a5c66e214a28bc15a070000051c1713192d48bcb946135e5be` | 2,962 | `a9367dfed1fcfdd6daba689668a29facd058c0b92536ebc2e817107e3ae48d76` |
| CPU stress | console | [`stress-qemu.log.gz`](stress-qemu.log.gz) | 1,418 | `767b40de4244c5368281de2804f2445e9be6ffb06fcb69e01ac5781037fc9f42` | 672 | `92d66690e48018b13fb76cbc0229955527651cef65447906f88afb605f5dfbc7` |
| CPU stress | build | [`stress-build.log.gz`](stress-build.log.gz) | 17,293 | `8a18262902329e991b3524d49d99f31d6ff22d94f2571d65f8090e2e08d00273` | 2,973 | `bb6b35ea7ffb200a23defc85c7d074bf3d8f30e5b3719fa7129e8ef614c1137c` |

Verify both the retained stream and its original bytes without normalizing line
endings:

```sh
sha256sum idle-qemu.log.gz
gzip -cd idle-qemu.log.gz | sha256sum
```

Prepare the pinned environment and create a new, non-overwriting campaign from
the repository root in WSL or another Linux shell:

```sh
bash competition/rt-baseline/zephyr/prepare.sh
bash competition/rt-baseline/zephyr/run.sh all \
  tmp/competition/rt-baseline/zephyr/reproduction-1
```

The benchmark implementation, metric boundaries, dependency overrides, and
comparison boundary are documented in
[`competition/rt-baseline/zephyr/README.md`](../../rt-baseline/zephyr/README.md).

## Interpretation limits

This is one approximately ten-second capture per workload under QEMU TCG on a
WSL2 host. It does not establish long-term stability or a hardware worst-case
bound. The native app uses one Cortex-A53 CPU and an in-kernel lower-priority
stress thread; the standalone AxVisor runner uses a Cortex-A72 machine, four
host pCPUs, a two-vCPU Linux guest, and places stress on the guest's other CPU.
The operating system, CPU model, topology, timer/dispatch path, and
virtualization boundary all differ.

The retained Windows-hosted source checkout uses `core.autocrlf=true`; Git's
post-run status is clean after applying the canonical line-ending filter. The
attestation records the exact tag object, peeled commit, and Git-index hash and
was captured after both QEMU runs. The analyzers rechecked that bounded
provenance without repeating the multi-minute NTFS worktree scan.

Individual samples were stored in fixed in-guest arrays and reduced only after
measurement, but were not serialized to the console. The retained native
baseline therefore preserves exact aggregate console records, not an
independently recomputable sample series. No console output occurred while the
measured samples were being collected.
