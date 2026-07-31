# Test Report

This document is the reviewer-facing test summary for the redcola Quancheng Lab
2026 AxVisor contest artifact. It points to the detailed evidence documents and
records the key PASS gates.

## Test Scope

The test set covers:

- native Zephyr RTOS baseline;
- AxVisor-hosted Zephyr e1000 IPv4/UDP;
- Linux/RTOS dual-guest IPv4/UDP communication;
- QCZ1 reliable UDP control protocol;
- AI inference to RTOS control closed loop;
- Linux and RTOS 1 ms periodic probes under 0/1/2/4 Linux-worker pressure;
- three-run stability under 2-worker Linux pressure;
- artifact reproducibility and static preflight checks.

## Startup Validation

The integrated script requires the following markers:

```text
QC_DUAL_GUEST_UDP_ECHO_RESULT=PASS
QC_RT_PERIODIC_RESULT=PASS
QC_RTOS_PERIODIC_RESULT=PASS
QC_QCZ1_RELIABLE_RESULT=PASS
QC_AI_CONTROL_RESULT=PASS
QC_QCZ1_GUEST_DEMO=PASS
QC_DUAL_GUEST_LINUX_INIT=PASS
result=PASS
```

Known integrated passing run:

```text
Linux guest vCPU count: 2
plain UDP: 20/20 PASS
QCZ1 reliable UDP: 10/10 PASS
AI control: 10/10 PASS
tcpdump kernel drops: 0
```

Latest short final-demo rehearsal archive:

```text
archive SHA256: 064e37dca1aec17cc6e7e3169aa80ebb4987a3920978073e5e9cf825b0618eb7
plain UDP: 20/20 PASS, mean/max RTT 3.705 ms / 31.001 ms
QCZ1 reliable UDP: 10/10 PASS, duplicate ACKs 2, retransmits 0
AI control: 10/10 PASS, end-to-end mean/max 1.563 ms / 1.754 ms
Linux periodic: 2000 samples, p99 1.823 ms, max 2.873 ms
RTOS periodic: 1000 samples, p99 1.427 ms, max 7.008 ms
tcpdump captured packets: 88
tcpdump kernel drops: 0
```

## Communication Reliability

Native Zephyr reliable UDP campaign:

```text
rounds: 10/10 PASS
control messages: 200/200
duplicate ACK checks: 40
ACK p95: 2.059 ms
```

Integrated dual-guest run:

```text
plain UDP: 20/20 PASS
plain UDP RTT mean/max: 2.943 ms / 19.039 ms
QCZ1 reliable UDP: 10/10 PASS
duplicate ACKs: 2
retransmits: 0
QCZ1 latency mean/max: 3.112 ms / 8.108 ms
tcpdump captured packets: 88
tcpdump kernel drops: 0
```

The analyzer also records application errors, timeout/recovery counters,
latency distributions and effective application throughput. See
`docs/protocol.md` and `docs/network-topology.md`.

## Realtime Baseline and AxVisor Runs

Native Zephyr baseline:

```text
board: qemu_cortex_a53
benchmark: tests/benchmarks/latency_measure
reported metrics: 47
preemptive k_yield context switch: 2400 ns
ISR return to interrupted thread: 1071 ns
maximum reported primitive latency: 46703 ns
result marker: PROJECT EXECUTION SUCCESSFUL
```

AxVisor long-sample comparison:

```text
0 workers: Linux p99 2.789 ms, RTOS p99 0.613 ms, AI 10/10
1 worker : Linux p99 2.559 ms, RTOS p99 1.228 ms, AI 10/10
2 workers: Linux p99 4.347 ms, RTOS p99 0.727 ms, AI 10/10
4 workers: Linux p99 41.868 ms, RTOS p99 1.256 ms, AI 10/10
```

The 4-worker run intentionally overcommits the 2-vCPU Linux guest. It raises
Linux-side latency but keeps RTOS periodic, UDP, QCZ1 and AI-control gates
passing.

See `docs/realtime-evaluation.md` and `results/realtime-comparison.csv`.

## AI Closed-Loop Result

Representative integrated dual-guest AI result:

```text
QC_AI_REQUESTS=10
QC_AI_SUCCESSES=10
QC_AI_FAILURES=0
QC_AI_INFER_MEAN_US=66
QC_AI_E2E_MEAN_US=2186
QC_AI_E2E_MAX_US=3389
QC_AI_CONTROL_ERROR_MEAN=207
QC_MANUAL_CONTROL_ERROR_MEAN=240
QC_AI_CONTROL_RESULT=PASS
```

Representative native smoke result:

```text
AI control messages: 20/20
end-to-end mean: 1.118 ms
AI mean error: 129.003
manual mean error: 204.640
```

The two reported comparison dimensions are control quality and timing. See
`docs/ai-control-evaluation.md`.

## Stability

The 2-worker pressure configuration was repeated three times:

```text
rounds: 3/3 PASS
UDP: 20/20 in every round
QCZ1: 10/10 in every round
AI: 10/10 in every round
tcpdump kernel drops: 0 in every round
bad-scan logs: empty
Linux periodic p99 range: 4.301-16.140 ms
RTOS periodic p99 range: 0.864-0.985 ms
AI e2e max range: 2.230-24.792 ms
```

See `results/stability/2026-07-27-stress2-3x/stability-summary.md`.

## Reproducibility and Artifact Checks

Current contest-directory preflight:

```text
python3 -m py_compile scripts/*.py linux/*.py: PASS
bash -n scripts/*.sh linux/*.sh: PASS
QCZ1 STATUS negative selftest via documented runner/preflight path: PASS
artifact scan for images/logs/pyc/tarballs: 0
git diff --check: PASS
dry-run staged path count: 38
outside contest dry-run path count: 0
```

Current PR-head runtime proof, using the same dual-guest runner in unprivileged
QEMU hub mode:

```text
command:
  os/axvisor/contest/quancheng2026/scripts/run_axvisor_dual_guest_qcz1_ai.sh
  --repo /path/to/tgoskits
  --evidence-dir /home/kali/qc-evidence/qc_pr1703_status_selftest_runner_hub_20260731_092251
  --timeout 300
  --linux-rt-samples 2000
  --net-mode hub
QC_QCZ1_STATUS_NEGATIVE_SELFTEST=PASS
qemu_status=0
net_mode=hub
QC_RTOS_PERIODIC_RESULT=PASS
QC_RT_PERIODIC_RESULT=PASS
QC_DUAL_GUEST_UDP_ECHO_RESULT=PASS
QC_QCZ1_RELIABLE_STATUS_OK=1
QC_QCZ1_RELIABLE_RESULT=PASS
QC_AI_STATUS_OK=1
QC_AI_CONTROL_RESULT=PASS
QC_QCZ1_GUEST_DEMO=PASS
QC_DUAL_GUEST_LINUX_INIT=PASS
result=PASS
QC_UDP_SUCCESSES=20
QC_QCZ1_LATENCY_MEAN_US=4957
QC_AI_E2E_MEAN_US=3714
QC_AI_E2E_MAX_US=18067
```

This current-head run uses QEMU `hubport` to avoid requiring privileged TAP
and bridge setup in the remote validation session. It validates the same
Linux/RTOS guest IP path, QCZ1 application protocol, AI control loop, guest
marker sequence, and the pre-QEMU STATUS timeout/malformed negative selftest.
TAP bridge state and tcpdump packet-capture counters are not claimed for this
unprivileged current-head run.

The first-stage commit boundary remains:

```text
os/axvisor/contest/quancheng2026/
```

No AxVisor core files, image files, temporary scripts or generated raw logs
belong in the first-stage contest-material commit.

## Evidence References

Main local evidence roots on the Windows host are listed in `README.md` and
`results/CURRENT_STATUS_2026-07-26.md`. The current source/documentation bundle
is generated under:

```text
contest-package/FINAL_UPLOAD_MANIFEST_2026-07-27.md
contest-package/2026-07-27-demo-rehearsal-latest-evidence/
```
