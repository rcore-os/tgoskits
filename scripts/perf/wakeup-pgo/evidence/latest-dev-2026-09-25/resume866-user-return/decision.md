# resume866: invalid board run

This run does not provide user-return timing data. The qperf-only image
`54ee6a87398275a571f917ac55526947ea5eb2f440c75e6aad55fbf8af39537f`
was built from `b292a098bb` plus the temporary probe, with frozen
benchmark SHA256
`94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`.
The Starry build, `cargo fmt`, ax-runtime clippy 26/26, starry-kernel
clippy 72/72 and `cargo xtask test` 68/68 passed.

OrangePi-5-Plus-1 session
`5b4fb9dd-1135-46c8-a322-973ef5004ea8` booted this image with
the expected PLL and SMP count. However, the guest could not download
the session script from `10.3.10.194:2999`: six connection refusals
followed by repeated timeouts. The 15-attempt loop ended at the shell
prompt without `RESUME866_DONE`; no benchmark round or counter snapshot
was collected. The host could reach `10.3.10.194:2999`, but not the
board-facing `192.168.1.2:2999` during this attempt, so the transfer
network state differs from the successful resume864 run. The board
client waited for its explicit completion marker, timed out, and sent
DELETE for the session; the following GET reported `state: releasing`.
No native performance inference is valid. The probe remains temporary
only and must be removed from the main worktree. Retrying requires a
fresh session and confirmed board-facing file access, not reusing this
invalid boot as one of the planned five rounds.
