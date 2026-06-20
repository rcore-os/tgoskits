# ISSUE-005: x86 Short Connection Timeout

## Status

- State: investigating
- Scope: x86_64 Starry nginx phase31 short-connection loop
- Impact: can fail `nginx-runner.sh all` at `phase31` with `NGINX_PHASE31_TEST_FAILED`

## Symptom

- `phase31` runs many short HTTP connections against nginx on `127.0.0.1:8080`.
- On x86_64, the loop can exceed the per-probe or per-phase timeout budget.
- The terminal failure usually appears as:

```text
NGINX_RUNNER_PHASE_BEGIN phase=phase31
NGINX_PHASE31_TEST_FAILED
NGINX_PHASE31_LOG: 100 short connections
NGINX_RUNNER_PHASE_FAIL phase=phase31 rc=1
NGINX_RUNNER_FAILED phase=phase31
```

Older logs also showed:

```text
NGINX_PHASE31_LOG: watchdog timeout
Terminated
```

## Confirmed Observations

- riscv64 passed the same nginx phase path.
- x86_64 standalone `phase31` can fail, so the issue is not only cross-phase state from `all`.
- A focused short-connection debug run once failed around iteration 143 with `curl rc=7` and `connection refused` while nginx received `SIGTERM`.
- Raising the debug runner budget to 1800s made the timeout-wrapped short-connection debug pass 120 iterations.
- Running the same debug loop without per-request `timeout` also passed 120 iterations.
- The x86 loop is still very slow: `curl` reports tens of milliseconds of HTTP time, but the wall-clock loop averages several seconds per request.
- A timing microbench shows the slow path starts before nginx/socket handling:

| Bench | x86_64 | riscv64 |
| --- | ---: | ---: |
| shell arithmetic, 100 iters | 1s | 0s |
| shell builtin `:`, 100 iters | 1s | 0s |
| `/bin/true`, 100 iters | 32s | 10s |
| `/bin/busybox true`, 100 iters | 38s | 10s |
| `/bin/echo ok`, 100 iters | 36s | 15s |
| `/bin/sleep 0`, 100 iters | 30s | 19s |
| `/bin/date +%s`, 20 iters | 9s | 2s |
| `/bin/sleep 1`, 5 iters | 6s | 5s |
| `timeout 5 /bin/true`, 100 iters | 63s | 23s |
| direct nginx `curl`, 40 iters | 91s | 31s |
| `timeout 10 curl`, 40 iters | 106s | 34s |

- `sleep 1` is roughly correct, so the primary issue is not a simple timer-frequency error.
- Shell builtins are fast, while every external command has high fixed cost, so the primary issue is likely in x86 process launch/exec/exit/wait or user/kernel transition overhead.
- `timeout` adds another external process and timer/signal layer, which makes the phase31 `timeout curl` loop more vulnerable.

## Current Hypothesis

- The HTTP request itself is not the main cost.
- The x86_64 path spends excessive wall time in short-lived external command handling.
- The current strongest suspects are x86 `execve` page-table work, x86 user/kernel transition overhead, and vfork/exec/wait scheduling latency.
- The original `timeout 5 curl ...` assertion is therefore close to the x86 timing cliff and can fail nondeterministically.

## Debugging Work So Far

- Reproduced the original failure shape from `phase31`: `NGINX_PHASE31_TEST_FAILED` followed by `NGINX_RUNNER_FAILED phase=phase31`.
- Verified that standalone x86_64 `phase31` can fail, so the issue is not only caused by earlier phases in `all`.
- Added `nginx-3-1-short-connection-debug.sh` to run the same short-connection pattern with per-iteration curl status, stderr, nginx process state, socket state, and log tails.
- The first timeout-wrapped debug run failed around iteration 143 with `curl rc=7` and `connection refused`; nginx had received `SIGTERM`, which matched the runner/default watchdog budget expiring rather than a stable nginx accept failure.
- Raised the debug runner budget to 1800s and confirmed timeout-wrapped short connections can complete 120 iterations on x86_64.
- Disabled per-request `timeout` in a direct debug run and confirmed 120 iterations also pass, but still take several hundred seconds.
- Added `nginx-3-1-x86-timing-debug.sh` to split shell-only work from external-command, timeout, and curl loops.
- Added a riscv64 timing-debug QEMU config and ran the same timing script as an architecture baseline.

## Phase Workaround

- `apps/starry/nginx/phase/nginx-3-1-short-connection-tests.sh` keeps the same HTTP assertion: 100 independent `curl -fsS` short connections must succeed.
- The per-curl wall-clock timeout was raised from 5s to 15s through `SHORT_CONN_CURL_TIMEOUT`.
- This does not weaken the response correctness check; it only avoids treating the current x86 external-command overhead as an nginx failure.
- The runner phase budget remains the outer bound. The measured x86 timing debug still fits well below the standalone phase budget after this per-operation relaxation.

## Code Pointers

- `os/StarryOS/kernel/src/syscall/task/clone.rs`: `CloneArgs::do_clone`, vfork setup and parent wait.
- `os/StarryOS/kernel/src/syscall/task/execve.rs`: `do_execve`, per-exec address-space creation, ELF load, and page-table switch.
- `os/StarryOS/kernel/src/task/mod.rs`: `ProcessData::wait_vfork_done` / `notify_vfork_done`.
- `components/axcpu/src/x86_64/uspace.rs`: x86 user context entry/exit and per-run FS/GS MSR handling.
- `components/axcpu/src/x86_64/trap.S`: `enter_user` syscall/iret return path.
- `components/axcpu/src/x86_64/context.rs`: task context switch and user page-table switch.

## Suspicious Implementation Details

- `execve` creates and installs a fresh address space for every external command. On x86_64 this includes kernel mapping copy, CR3 switch, and TLB effects that shell builtins avoid.
- x86 user entry/exit does FS/GS MSR handling around `UserContext::run()`. Short-lived commands execute many syscall/user-return cycles during dynamic loader startup and process exit.
- Fresh x86 `UserContext::new()` after `execve` may initially return through the `iretq` path instead of the faster syscall-return path.
- BusyBox shell likely uses `vfork`/`CLONE_VFORK` for external commands. The parent blocks in `ProcessData::wait_vfork_done()` until the child execs or exits, so scheduling or wakeup latency is paid once per command.
- `timeout curl` adds another external process plus timer/signal/wait behavior, which explains why it is significantly slower than direct `curl` and why phase31 is sensitive to a 5s per-operation deadline.

## Debug Scripts

- `nginx-3-1-short-connection-debug.sh`: nginx short-connection reproduction with detailed curl/nginx/socket logs.
- `nginx-3-1-x86-timing-debug.sh`: microbench to compare shell loop, `/bin/true`, `timeout true`, direct curl, and timeout-wrapped curl.

## Repro Commands

```bash
cargo xtask starry app qemu -t nginx --arch x86_64 --qemu-config apps/starry/nginx/qemu/debug/qemu-x86_64-short-connection-debug.toml
cargo xtask starry app qemu -t nginx --arch x86_64 --qemu-config apps/starry/nginx/qemu/debug/qemu-x86_64-short-connection-direct-debug.toml
cargo xtask starry app qemu -t nginx --arch x86_64 --qemu-config apps/starry/nginx/qemu/debug/qemu-x86_64-timing-debug.toml
cargo xtask starry app qemu -t nginx --arch riscv64 --qemu-config apps/starry/nginx/qemu/debug/qemu-riscv64-timing-debug.toml
```

## Next Follow-up

- The bottleneck appears before networking: inspect x86 short-lived process execution first.
- Compare `execve` page-table switching costs on x86, especially CR3/TLB work in `components/axcpu/src/x86_64/context.rs` and address-space replacement in `do_execve`.
- Inspect x86 user/kernel round-trip overhead in `components/axcpu/src/x86_64/uspace.rs` and `components/axcpu/src/x86_64/trap.S`.
- Inspect vfork/exec/wait wakeup latency in `CloneArgs::do_clone`, `ProcessData::wait_vfork_done`, `notify_vfork_done`, and `sys_waitpid`.
