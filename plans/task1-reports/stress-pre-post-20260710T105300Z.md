# Task 1 Stress Pre vs Post Comparison (20260710T105300Z)

Generated from existing logs (parser corrected: strip shell prefix; exact period_ms match).

## Topology
| Profile | Linux | RT guest |
|---|---|---|
| pre-opt | linux-smp2 (nice=10) | arceos-rt-smp1-pre-opt (no vcpu_priorities) |
| post-opt | linux-smp2 (nice=10) | arceos-rt-smp1 (vcpu_priorities=[-20]) |

## Stress RT latency (180000 samples / period)
### pre-opt
```
RT_LATENCY mode=guest period_ms=1 samples=180000 mean_jitter_ns=76710 p99_jitter_ns=263312 p999_jitter_ns=482448 max_jitter_ns=10045856
RT_LATENCY mode=guest period_ms=10 samples=180000 mean_jitter_ns=104957 p99_jitter_ns=327904 p999_jitter_ns=578400 max_jitter_ns=10110880
```
### post-opt
```
RT_LATENCY mode=guest period_ms=1 samples=180000 mean_jitter_ns=75908 p99_jitter_ns=258320 p999_jitter_ns=446416 max_jitter_ns=10147056
RT_LATENCY mode=guest period_ms=10 samples=180000 mean_jitter_ns=100010 p99_jitter_ns=309648 p999_jitter_ns=527568 max_jitter_ns=10049008
```
## P99 jitter improvement (pre → post)
| period_ms | pre-opt P99 (ns) | post-opt P99 (ns) | improvement |
|---:|---:|---:|---:|
| 1 | 263312 | 258320 | 1.9% |
| 10 | 327904 | 309648 | 5.6% |

## P999 jitter improvement
| period_ms | pre-opt P999 (ns) | post-opt P999 (ns) | improvement |
|---:|---:|---:|---:|
| 1 | 482448 | 446416 | 7.5% |
| 10 | 578400 | 527568 | 8.8% |

## Raw logs
- pre-opt: [`mixed-stress-pre-opt-20260710T105300Z.log`](./mixed-stress-pre-opt-20260710T105300Z.log)
- post-opt: [`mixed-stress-round1-20260710T091425Z.log`](./mixed-stress-round1-20260710T091425Z.log)
