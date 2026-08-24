# Sustained real-YOLO Task 1 A/B

Validated runs: 6. Every accepted run has contiguous periodic samples, a matching completion count, the requested real ncnn/YOLO inference count, and no fatal marker.

| Scheduler | Runs | Median P99 | Median P99.9 | Median max | YOLO mean | YOLO P99 | Throughput |
|---|---:|---:|---:|---:|---:|---:|---:|
| rr | 3 | 39.527 ms | 49.869 ms | 56.072 ms | 2247.231 ms | 2787.263 ms | 4.672/min |
| fp-rr | 3 | 0.574 ms | 7.886 ms | 9.611 ms | 2967.488 ms | 79424.584 ms | 12.346/min |

Median per-run P99 changes from 39.527 ms to 0.574 ms (98.548% reduction).

The per-run and scheduler-median CSV files remain the source of truth; this report does not claim a native-RTOS bound or hide YOLO tail-latency/throughput trade-offs.
