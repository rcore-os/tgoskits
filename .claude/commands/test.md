---
name: test
description: Run builds and tests in the Docker CI container
args:
  - name: scope
    type: string
    required: false
    default: quick
    enum: [quick, full, fmt, clippy, starry, arceos, axvisor]
  - name: arch
    type: string
    required: false
    default: all
    enum: [aarch64, riscv64, x86_64, loongarch64, all]
---

# /test — Run CI checks in Docker container

## Dispatch

| Invocation | Action |
|------------|--------|
| `/test` | Run quick checks (fmt + clippy + sync-lint) |
| `/test quick` | Run quick checks |
| `/test full` | Run full CI matrix (all OSes, all architectures) |
| `/test fmt` | Run `cargo fmt --all -- --check` |
| `/test clippy` | Run `cargo xtask clippy` |
| `/test starry aarch64` | Run StarryOS QEMU tests for aarch64 |
| `/test arceos riscv64` | Run ArceOS QEMU tests for riscv64 |
| `/test axvisor` | Run Axvisor QEMU tests for all 3 architectures |
| `/test starry all` | Run StarryOS QEMU tests for all 4 architectures |

## Implementation

### Step 1: Map the invocation to a local-ci.sh command

- `fmt`: run docker directly — `docker run --rm -v "$PWD:/workspace" -w /workspace tgoskits-ci bash -c "cargo fmt --all -- --check"`
- `clippy`: `docker run --rm -v "$PWD:/workspace" -w /workspace tgoskits-ci bash -c "cargo xtask clippy"`
- `quick`: `bash .claude/scripts/local-ci.sh quick`
- `full`: `bash .claude/scripts/local-ci.sh full`
- `<os>` + `<arch>`: `bash .claude/scripts/local-ci.sh test <os> <arch>`
- `<os>` + `all`: run `local-ci.sh test <os> <arch>` for each supported architecture of that OS

### Step 2: Ensure Docker images exist

Run `bash .claude/scripts/local-ci.sh quick` first if images are not yet built (this call ensures base image).

### Step 3: Execute

Run the mapped command. Capture stdout and stderr.

### Step 4: Save results

Save results to `.claude/cache/last-ci-result.json`:

```json
{
  "timestamp": "<ISO timestamp>",
  "status": "pass|fail",
  "command": "<what was run>",
  "results": [
    {"command": "cargo fmt --all -- --check", "pass": true, "output": "..."},
    {"command": "cargo xtask clippy", "pass": false, "output": "error: ..."}
  ]
}
```

### Step 5: Report

If all passed: "All checks passed."
If failures: list each failing command with its error output.
