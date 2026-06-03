# Starry Nginx App

This app organizes nginx tests into four directories:

- `smoke/`: CI entry smoke test (the only connected nginx test in tgoskits)
- `phase/`: stage-unit tests named by `x-x`, such as `nginx-1-3-lifecycle-tests.sh`
- `stress/`: pressure test planning and future scripts
- `debug/`: flexible issue-focused scripts

## CI-Connected Entry

Only smoke is connected:

```bash
cargo xtask starry app run -t nginx --arch riscv64
```

`qemu-*.toml` starts `/usr/bin/nginx-smoke-tests.sh` only.

## Local Development CLI

Unified local CLI script:

```bash
./apps/starry/nginx/nginx-cli-tests.sh smoke
./apps/starry/nginx/nginx-cli-tests.sh phase1
./apps/starry/nginx/nginx-cli-tests.sh phase2
./apps/starry/nginx/nginx-cli-tests.sh all
```

## Build/Prepare Logic

`prebuild.sh` injects only smoke entry and shared mirror helper into guest overlay:

- `/usr/bin/nginx-smoke-tests.sh`
- `/usr/bin/nginx-alpine-mirror.sh`

Mirror helper: `apps/starry/nginx/nginx-alpine-mirror.sh`.
