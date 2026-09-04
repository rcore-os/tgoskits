# icpc-bench 样例输出（QEMU 双 Guest，2026-07-24）

运行：`./scripts/task2/run-icpc-bench.sh`

```
ICPC_BENCH_CSV
seq,rtt_us,ok
1000,10803,1
1001,2834,1
1002,4220,1
1003,2915,1
1004,3087,1
1005,1945,1
1006,1780,1
1007,1865,1
1008,2379,1
1009,3228,1
1010,2252,1
1011,2032,1
1012,2122,1
1013,2019,1
1014,2110,1
1015,2031,1
1016,2061,1
1017,1942,1
1018,2166,1
1019,2062,1
ICPC_BENCH_SUMMARY msgs=20 ok=20 fail=0 p50_us=2122 p99_us=10803 msg_per_s=332.49
icpc-bench pass
```

说明：

- 首包 RTT 偏高（ARP/冷启动）
- P50 ≈ 2.1 ms，有效吞吐 ≈ 332 msg/s（20 次 HEARTBEAT 停等往返）
- CSV 字段：`seq,rtt_us,ok`
