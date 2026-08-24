# Ten-minute physical stability run

Configuration: 2-vCPU Linux in `stress-rt` mode, cyclictest interval 1 ms,
duration 600 seconds, priority 90 and affinity CPU1; stress-ng affinity CPU0.
RT-Thread runs its 300-sample, 10 ms periodic probe concurrently on its own
assigned pCPU.

The capture ends with all required terminal markers:

* 60 `RT_PROGRESS` records reaching 593.78 seconds;
* `PERIODIC LATENCY COMPLETE samples=300`;
* `RT_CYCLICTEST_COMPLETE`;
* `RT_INIT_DONE scenario=stress-rt`;
* driver result `TASK1_LINUX2_MATRIX_COMPLETE`.

Linux cyclictest result:

| samples | min us | avg us | P90 us | P95 us | P99 us | P99.9 us | max us | overflow |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 600,000 | 58 | 192 | 195 | 198 | 204 | 212 | 486 | 0 |

Concurrent RT-Thread result: 300/300, mean 308.557 us, P99 499.125 us,
P99.9/max 532.958 us, and no sample above 1 ms.

The formal boot and console logs contain no `panic`, `ESR_EL2`, IRQ 26 fatal,
or generic fatal marker. The exact RAM image identity is recorded in the
top-level `artifact-hashes.txt`; the generated image itself is omitted from
Git to keep the evidence branch small.
