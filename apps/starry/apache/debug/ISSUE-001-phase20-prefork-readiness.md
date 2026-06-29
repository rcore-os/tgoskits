# ISSUE-001: phase20 Prefork Readiness Overspecification

## Status

- State: investigating
- Scope: Apache `phase20` prefork readiness check on StarryOS
- Impact: can make `apache-runner.sh all` fail at `phase20` with `APACHE_RUNNER_FAILED`

## Symptom

- `phase20` starts Apache, serves `/` and `/server-status?auto`, then exits with:

```text
APACHE_PHASE20_LOG: BEGIN worker pool ready
APACHE_PHASE20_LOG: BEGIN request handling
APACHE_PHASE20_LOG: BEGIN stop cleanup
APACHE_RUNNER_PHASE_FAIL phase=phase20 rc=1
APACHE_RUNNER_FAILED phase=phase20 rc=1
```

- The guest logs also show:

```text
[mpm_prefork:error] AH00161: server reached MaxRequestWorkers setting, consider raising the MaxRequestWorkers setting
```

- The access log still shows successful requests:

```text
"GET / HTTP/1.1" 200
"GET /server-status?auto HTTP/1.1" 200
```

## Confirmed Observations

- Apache prefork does start and answer HTTP requests.
- The failure is not a hard startup failure.
- The visible error log line is an Apache MPM warning, not a crash.
- The original phase20 script was checking restart-cycle behavior in addition to the phase20 scope.
- The phase50 lifecycle test covers `restart` and `HUP` behavior.

## Current Hypothesis

- `phase20` is constrained more tightly than its prefork startup and request-handling goal requires.
- The readiness check should only require:
  - Apache is up
  - `server-status?auto` is readable
  - the response indicates `ServerMPM: prefork`
  - basic requests still work

## Debugging Work So Far

- Aligned the phase20 config with the test plan's `StartServers 1`, `MinSpareServers 1`, `MaxRequestWorkers 2` shape.
- Removed restart-cycle validation from phase20.
- Added a focused debug probe:
  - `apps/starry/apache/debug/apache-phase20-restart.sh`
  - `apps/starry/apache/qemu/debug/qemu-x86_64-phase20-restart.toml`
- Kept restart/lifecycle validation in phase50, where it already fits the test scope.

## Repro Commands

```bash
cargo xtask starry app qemu -t apache --arch x86_64 --qemu-config apps/starry/apache/qemu/phase/qemu-x86_64-phase20.toml
cargo xtask starry app qemu -t apache --arch x86_64 --qemu-config apps/starry/apache/qemu/debug/qemu-x86_64-phase20-restart.toml
```

## Next Follow-up

- Confirm whether the remaining phase20 failure is still caused by the readiness gate or by a real prefork cleanup bug.
- If the debug probe passes, keep phase20 limited to startup, request handling, and clean stop.
