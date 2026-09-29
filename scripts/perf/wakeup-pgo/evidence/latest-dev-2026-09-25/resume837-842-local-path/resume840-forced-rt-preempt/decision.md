# resume840: unchanged-image forced RT preemption discriminator

This is a **diagnostic benchmark variant**, not the frozen full20. Exact kernel source is `69a33650763538692fafea27c869870ed0313642` on `dev@05175ca38823b631a73777b0130226ddfa558439`. The unchanged, uninstrumented weighted-PGO G image SHA-256 is `9e9847a433cd99808d7f511372d454eb2cf5412a94e42187bcab64780ebfb8a3`; its cpufreq-off configuration was independently measured near 816 MHz in earlier PMU diagnostics, not synchronously in these eight rounds. Frozen benchmark SHA-256 is `94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`; rebuilt control SHA-256 is `6f582832f3e10d3d447d8f06afc706c0684c6f80a0b9346369b4dc59d922e713`; priority variant SHA-256 is `fabee7b6b3e312b07248d9d797cd3e3b40db406bac9f218d3516c66a1c3abe8c`. The diagnostic variant gives only the same-CPU FIFO receiver priority 81 while the sender remains priority 80; a startup marker proves that condition. It does not alter the kernel or original frozen binary.

The first Plus-2 session `run1` saw prior Starry serial output and timed out waiting for U-Boot; it was released with no measurement. `run2` booted the G image but the copied `/tmp` `main.c` had `warmup=0`, `handoff_samples=1`; six rebuilt-binary rows were invalid and retained. The original frozen-binary rows happened to be valid but do not rescue the invalid group. The source was corrected to the repository's 1000/20000/10000 defaults, both diagnostic binaries rebuilt and hashes changed. `run3` power-cycled within its own lease, verified U-Boot PLL registers, and completed eight interleaved focused rows on OrangePi-5-Plus-2. Each row has 20000/20000 samples, zero `not_parked` and `missed_deadlines`, and matching histogram count; the session was released.

| Starry binary/policy | p50 rounds (ns) | Median (ns) |
| --- | ---: | ---: |
| rebuilt control FIFO | 11958, 11958 | 11958 |
| forced FIFO | 10792, 10792 | 10792 |
| frozen FIFO | 11958, 12250 | 12104 |
| rebuilt control OTHER | 14584, 14875 | 14729.5 |

`run3/results.json` SHA-256 is `b7d25096cbcaef45620ac67c0422d90df918fb8ed94c16c523520d27c55d6b8e`, and raw serial SHA-256 is `111bd5fb648c3b0e586f7f88c49d3397d83edb055fe7480b27811083b91c7a3a`. The priority variant improves Starry FIFO p50 by 1166 ns relative to its source-equivalent rebuilt control, yet does not approach the Linux RT frozen equal-priority 8458 ns row. That cross-condition comparison alone cannot isolate the cause; the matching Linux RT diagnostic is `resume841`. No runtime source change, full20 candidate gain or 90% acceptance result was produced.
