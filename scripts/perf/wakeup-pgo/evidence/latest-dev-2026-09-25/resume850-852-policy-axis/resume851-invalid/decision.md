# resume851: invalid receiver-policy diagnostic

The unchanged Starry G image SHA-256
`9e9847a433cd99808d7f511372d454eb2cf5412a94e42187bcab64780ebfb8a3`
was run twice on OrangePi-5-Plus-2 with diagnostic binary SHA-256
`c9e96efb297c64017c55052368a3470cb717f83375c266213b1bb820163addc5`.
The predeclared six-process order was FIFO, OTHER, OTHER, FIFO, FIFO, OTHER.
Each process attempted 1000 warmup and 20000 measured iterations per arm.
No kernel, frozen benchmark or PR production source was changed. Both board
sessions were released; `cargo xtask board ls` later showed 4/4 available.

`starry-run1` completed the guest script with nonzero overall result; its
runner asserted before downloading per-process files. The serial log SHA-256
is `fe0b1183ac8de87a594fba4cf57ee2c7369e032f6d717b1de57085d25c47fb82`,
and it shows the last `wake-cost` process exited nonzero. This is a runner
evidence-loss bug, not a valid comparison. The runner was changed only to
download all six raw logs before checking the aggregate status.

`starry-run2` retained every process log. FIFO rounds 1, 4 and 5, and OTHER
round 2, printed 20000 samples for each arm and exited zero. OTHER rounds
3 and 6 exited one with `RESUME851_INVALID ... missed=3` and `missed=2`.
Their raw-log SHA-256s are
`d3b9fb9264b70753d1d2c149a60b19e7c873eaf7f0871cf4f9b486a9a685bf25`
and `ab9e615e96bbfb5f56bd8f3687b00c74c0f2e0114e6b01d439b56fcc005ef553`.
The full serial SHA-256 is
`88c7a14ee5d84eba2b94b8404ffcee9d309a4a2ca077928ee6a01ee7759ce1bd`.
`missed` combines unexpected empty, mismatch and match wake returns, so
the precise failure type is unknown. The 50 us settle may be too short for
the lower-priority OTHER receiver to park; this is a hypothesis, not a
diagnosed kernel fault.

Decision: **invalid**. Do not splice successful FIFO/OTHER rows, compare
their p50s, or run Linux RT as though the Starry gate passed. A distinct
follow-up must change only the out-of-window park settle and add per-arm
failure counters, then require a complete valid six-round group on each
kernel. Latest valid uninstrumented full20 remains 11/20 at 90%, worst
58.00%; this experiment produced no performance gain.
