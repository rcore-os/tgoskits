# Starry Apache App

This case runs Apache httpd smoke checks inside StarryOS through the app runner.

Default QEMU config runs the smoke test:

```bash
cargo xtask starry app run -t apache --arch riscv64
```

Before marking any tracker item as passed, run the same smoke script in a Linux
Alpine environment and compare Apache behavior against StarryOS.

```bash
sh apps/starry/apache/smoke/apache-smoke-tests.sh
```

Current layout:

- `smoke/`: CI/app-entry smoke script only.
- `phase/`: future focused Apache phase tests.
- `debug/`: future issue-specific probes.
- `stress/`: future pressure tests, separate from phase correctness tests.

The prebuild step injects:

- `/usr/bin/apache-smoke-tests.sh`
- `/usr/bin/apache-phase20-tests.sh`
- `/usr/bin/apache-phase30-tests.sh`
- `/usr/bin/apache-phase40-tests.sh`
- `/usr/bin/apache-phase50-tests.sh`
- `/usr/bin/apache-phase55-tests.sh`
- `/usr/bin/apache-phase70-tests.sh`
- `/usr/bin/apache-phase80-tests.sh`
- `/usr/bin/apache-alpine-mirror.sh`
