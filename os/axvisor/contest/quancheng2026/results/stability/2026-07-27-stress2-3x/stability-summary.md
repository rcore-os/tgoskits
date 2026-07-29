# AxVisor dual-guest 2-worker 3-run stability summary

Date: 2026-07-27

Remote root: `/tmp/qc_multirun_stress2_20260727_035234`

Command per round:

```bash
./scripts/run_axvisor_dual_guest_qcz1_ai.sh --timeout 180 --linux-rt-samples 10000 --linux-stress-workers 2 --linux-stress-seconds 0
```

All three rounds completed with `analysis_result=PASS`, empty bad-scan logs, UDP/QCZ1/AI success, and tcpdump kernel drops `0`. The run uses a dual-guest setup: a 2-vCPU Linux guest drives the probes and AI/control client while the RTOS guest provides the IPv4/UDP endpoint and QCZ1 control service.

## Per-round evidence

| Round | Result | Evidence SHA256 | UDP | QCZ1 | AI | Linux mean/p99/max ns | RTOS mean/p99/max ns | UDP mean/max us | QCZ1 mean/max us | AI e2e mean/max us | Drops |
|---:|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | PASS | `fbe83e24d41cc3cc1c9172656de3212e4e044625c0837f6ba7c8ec3f941ddb26` | 20/20 | 10/10 | 10/10 | 1275322/16139568/41942128 | 103672/864272/4739680 | 3573/32804 | 2591/6173 | 1585/2230 | 0 |
| 2 | PASS | `6437349e481dd3b5282abe27a34085e4a0d26b214cd7a88478ff7532446f7a16` | 20/20 | 10/10 | 10/10 | 991262/4300992/9572688 | 115154/933488/4327488 | 6172/33907 | 5592/26197 | 5177/24792 | 0 |
| 3 | PASS | `38aac4038f06ae1731125cea46e6afce0b18d0cc5f0845ef7a562676a8cc97f5` | 20/20 | 10/10 | 10/10 | 1185251/7174032/23575136 | 118194/984736/4949296 | 6309/33870 | 3441/16016 | 2861/6457 | 0 |

## Aggregate across 3 rounds

| Metric | Min | Mean | Max |
|---|---:|---:|---:|
| Linux periodic mean ns | 991262 | 1150612 | 1275322 |
| Linux periodic p99 ns | 4300992 | 9204864 | 16139568 |
| Linux periodic max ns | 9572688 | 25029984 | 41942128 |
| RTOS periodic mean ns | 103672 | 112340 | 118194 |
| RTOS periodic p99 ns | 864272 | 927499 | 984736 |
| RTOS periodic max ns | 4327488 | 4672155 | 4949296 |
| UDP RTT mean us | 3573 | 5351 | 6309 |
| UDP RTT max us | 32804 | 33527 | 33907 |
| QCZ1 reliable mean us | 2591 | 3875 | 5592 |
| QCZ1 reliable max us | 6173 | 16129 | 26197 |
| AI end-to-end mean us | 1585 | 3208 | 5177 |
| AI end-to-end max us | 2230 | 11160 | 24792 |

## Evidence files

Local files:

- `round1-evidence.tar.gz`, `round1-evidence.tar.gz.sha256`, `round1-realtime-summary.json`, `round1-realtime-report.md`, `round1-summary.txt`, `round1-run.log`, `round1-analyze.log`, `round1-badscan.log`
- `round2-evidence.tar.gz`, `round2-evidence.tar.gz.sha256`, `round2-realtime-summary.json`, `round2-realtime-report.md`, `round2-summary.txt`, `round2-run.log`, `round2-analyze.log`, `round2-badscan.log`
- `round3-evidence.tar.gz`, `round3-evidence.tar.gz.sha256`, `round3-realtime-summary.json`, `round3-realtime-report.md`, `round3-summary.txt`, `round3-run.log`, `round3-analyze.log`, `round3-badscan.log`
- `stability-summary.csv`
- `stability-summary.md`

Notes:

- `round*-badscan.log` files are empty.
- `.sha256` records match the tarballs listed above.
- The RTOS periodic probe uses the Zephyr-side busy-wait periodic method, so it is reported as a guest RTOS timing probe rather than a full scheduler sleep benchmark.
