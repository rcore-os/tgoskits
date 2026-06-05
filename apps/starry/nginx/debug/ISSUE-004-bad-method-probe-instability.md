# ISSUE-004: BAD Method Probe Instability

## Status

- State: deferred (temporarily bypassed in phase2)
- Scope: `BAD / HTTP/1.1` verification path in stage 2
- Impact: does not block phase2 main flow

## Symptom

- In some runs, `busybox nc` returns an empty response (`<empty>`) for both:
  - `BAD / HTTP/1.1`
  - `GET / HTTP/1.1`
- This can make a strict raw-probe assertion fail even when nginx behavior is normal.

## Confirmed Observations

- `curl -X BAD http://127.0.0.1:8080/...` stably returns `405`.
- `nc.openbsd` raw BAD request stably returns `HTTP/1.1 405 Not Allowed`.
- `busybox nc` may return `<empty>` under the same request pattern.

## Current Decision

- Keep phase2 BAD-method node as bypass (known issue log only).
- Do not use `busybox nc` as pass/fail oracle for this node.
- Continue tracking through debug scripts:
  - `nginx-2-0-bad-method-debug.sh`
  - `nginx-2-0-bad-method-matrix.sh`

## Repro Commands

```bash
cargo xtask starry app qemu -t nginx --arch x86_64 --qemu-config apps/starry/nginx/qemu-x86_64-bad-method-debug.toml
cargo xtask starry app qemu -t nginx --arch x86_64 --qemu-config apps/starry/nginx/qemu-x86_64-bad-method-matrix.toml
```

Optional phase2 retest:

```bash
cargo xtask starry app qemu -t nginx --arch riscv64 --qemu-config apps/starry/nginx/qemu-riscv64-phase2.toml
cargo xtask starry app qemu -t nginx --arch x86_64 --qemu-config apps/starry/nginx/qemu-x86_64-phase2.toml
```

## Next Follow-up

- Add a minimal socket-level probe outside nginx to isolate whether this is:
  - `busybox nc` behavior under StarryOS, or
  - StarryOS socket timing/EOF behavior exposed by `busybox nc`.
- After root cause is confirmed and fixed, restore strict assertion for the phase2 BAD-method node.
