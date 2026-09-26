# resume833: CPU1 IPI dispatch/handler diagnostic

Source `69a33650763538692fafea27c869870ed0313642`, base `dev@05175ca38823b631a73777b0130226ddfa558439`. This is a temporary `qperf-metrics` probe of the cross-CPU futex wakeup path, not a production optimization. The patch is `probe.patch.gz` (decompressed SHA-256 `9ff8425f41037e8df2c8878141236315fab166bca9d4f107a24d7958b518d93b`). Diagnostic image SHA-256 `68ff5e32dead62e21fa96c67572fcf8acf3c6b4a591f9c228ac534fb58196ad1`; frozen benchmark SHA-256 `94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`.

OrangePi-5-Plus-2 session `9cec28d2-bdd2-4b72-96d9-ef7ae3026a36` was released. Four focused rounds (FIFO 1/2, OTHER 1/2) each produced 20000/20000 samples with zero `not_parked` and zero missed deadlines. Instrumented p50 values were FIFO 36459/37041 ns and OTHER 40250/40250 ns; they are not native full20 acceptance measurements. `run1/results.json` SHA-256 `5ffd6a5f05eb284e5a0444290be9781332d87b7213374fb172cc8c90a96559cd`; `run1/serial.log` SHA-256 `bf9e76142a1fe78a862ae8d2592d57f9d378f4e8ab13e98a7c6fa5aed8895f6f`.

| Round | Dispatch/handler count | Mean dispatch | Mean handler | Mean enclosing excess |
|---|---:|---:|---:|---:|
| FIFO 1 | 21749/21749 | 2554 ns | 1373 ns | 1181 ns |
| FIFO 2 | 21666/21666 | 2619 ns | 1372 ns | 1247 ns |
| OTHER 1 | 21346/21346 | 2637 ns | 1424 ns | 1213 ns |
| OTHER 2 | 21339/21339 | 2661 ns | 1432 ns | 1229 ns |

Each 250 ns histogram places the dispatch median in bucket 10 (2500-2750 ns) and the handler median in bucket 5 (1250-1500 ns). These are separate aggregate distributions, not paired per-IPI differences. Dispatch measurement begins before the per-CPU pin and ends after registry dispatch; handler measurement starts inside the registered action. Thus the enclosing excess includes the CPU pin/context-depth setup, registry lock and scan, action wrapper, and timestamp overhead. It does **not** isolate a removable registry cost. The roughly 1.2 us/dispatch excess is smaller than the roughly 8-9 us cross-CPU gap observed in the native full20 rows, so a registry-only edit is not justified by this evidence.

`cargo fmt`, `cargo xtask clippy --package ax-plat` (three configurations), `cargo xtask clippy --package ax-runtime` (26 configurations), and the diagnostic Starry release build succeeded. `cargo xtask clippy --package starry-kernel` stopped during configuration 10/72 because the filesystem ran out of space writing Cargo incremental cache; it is **not** a passing check. The probe is to be reversed after archiving. No production runtime code or acceptance result changes: latest valid uninstrumented full20 remains G1/G2 at 11/20, worst 58.00% of Linux RT.
