# resume867: board-facing transfer retry

Reuse the exact resume866 qperf-only image SHA256
`54ee6a87398275a571f917ac55526947ea5eb2f440c75e6aad55fbf8af39537f`
and frozen benchmark SHA256
`94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`.
The only change from invalid resume866 is that the Starry guest downloads
and uploads session files through the board-facing `192.168.1.2:2999`.
The host accesses the service at `10.3.10.194:2999`. No kernel source,
image, benchmark, policy, sample count or measurement order changes.

Acquire a fresh OrangePi-5-Plus-1 session, check PLL and SMP, run five
independent `thread_futex_same_cpu` processes in OTHER/FIFO/OTHER/FIFO/OTHER
order, save complete serial, per-round benchmark output, before/after
user-return counters and session identity. Each round must have 20000/20000
samples, zero `not_parked` and `missed_deadlines`, zero exit and monotonic
counters. Do not splice with resume866 or call qperf p50 native full20.
If board-facing transfer still fails, reject this retry without a gain claim.
