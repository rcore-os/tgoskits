# Starry Apache App

Apache on StarryOS uses the same top-level directory split as `apps/starry/nginx`.

## Modes

Guest entrypoint: `/usr/bin/apache-runner.sh <mode>`

| mode | use | CI |
|---|---|---|
| `smoke` | default app entry | ✅ |
| `phase <id>` | rerun one phase script | operator-run |
| `all` | smoke + all phases, with each stage run in a fresh guest state | operator-run |
| `stress` | not implemented in `apache-runner.sh` | operator-run |
| `debug <name>` | probe for a specific issue | operator-run |

## Usage

```bash
cargo xtask starry app qemu -t apache --arch x86_64

# Manual Apache phase rerun
cargo xtask starry app qemu -t apache --arch x86_64 \
  --qemu-config apps/starry/apache/qemu/phase/qemu-x86_64-phase20.toml
```

## Layout

- `runner/`: unified guest entrypoint and shared helpers.
- `smoke/`: CI smoke script.
- `phase/`: Apache feature phases.
- `qemu/all/`: QEMU configs for the `all` flow.
- `qemu/phase/`: QEMU configs for single-phase reruns.
- `qemu/debug/`: QEMU configs for specific-issue debug runs.
- `debug/`: issue probes, wrappers, and notes.
- `stress/`: pressure tests kept out of the default runner flow.

`all` reuses the shared package-install sentinel and isolates each stage before
moving to the next stage.

## Guest assets

Prebuild injects:

- `/usr/bin/apache-runner.sh`
- `/usr/bin/apache-runner-lib.sh`
- `/usr/bin/apache-smoke-tests.sh`
- `/usr/bin/apache-phase20-tests.sh`
- `/usr/bin/apache-phase30-tests.sh`
- `/usr/bin/apache-phase40-tests.sh`
- `/usr/bin/apache-phase50-tests.sh`
- `/usr/bin/apache-phase55-tests.sh`
- `/usr/bin/apache-phase70-tests.sh`
- `/usr/bin/apache-phase80-tests.sh`
- `/usr/bin/apache-mpm-prefork-wait.sh`
- `/usr/bin/apache-phase20-restart.sh`
- `/usr/bin/apache-mpm-thread-futex.sh`
- `/usr/bin/apache-accept-mutex.sh`
- `/usr/bin/apache-htaccess-pathwalk.sh`
- `/usr/bin/apache-sendfile-mmap-range.sh`
- `/usr/bin/apache-graceful-signal.sh`
- `/usr/bin/apache-cgi-pipe-exec.sh`
- `/usr/bin/apache-log-append-reopen.sh`
- `/usr/bin/apache-alpine-mirror.sh`

## Known Issue Notes

- `debug/ISSUE-001-phase20-prefork-readiness.md` records the phase20 readiness
  overspecification finding and the debug probe used to investigate it.
