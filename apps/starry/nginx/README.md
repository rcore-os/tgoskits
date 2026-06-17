# Starry Nginx App

This app organizes nginx tests into four directories:

- `smoke/`: CI entry smoke test (the only connected nginx test in tgoskits)
- `phase/`: stage-unit tests named by `x-x`, such as `nginx-1-3-lifecycle-tests.sh`
- `stress/`: pressure test planning and future scripts
- `debug/`: flexible issue-focused scripts

## CI-Connected Entry

Only smoke is connected:

```bash
cargo xtask starry app qemu -t nginx --arch riscv64
```

`qemu-*.toml` starts `/usr/bin/nginx-smoke-tests.sh` only.

## Local Development CLI

Unified local CLI script:

```bash
# Run smoke test only
./apps/starry/nginx/nginx-cli-tests.sh smoke

# Run a single phase
./apps/starry/nginx/nginx-cli-tests.sh phase00   # env / rlimit
./apps/starry/nginx/nginx-cli-tests.sh phase12   # lifecycle 1-2
./apps/starry/nginx/nginx-cli-tests.sh phase13   # lifecycle 1-3
./apps/starry/nginx/nginx-cli-tests.sh phase20   # HTTP basic
./apps/starry/nginx/nginx-cli-tests.sh phase31   # short connection
./apps/starry/nginx/nginx-cli-tests.sh phase32   # keepalive
./apps/starry/nginx/nginx-cli-tests.sh phase33   # slow header
./apps/starry/nginx/nginx-cli-tests.sh phase41   # sendfile off
./apps/starry/nginx/nginx-cli-tests.sh phase42   # sendfile on
./apps/starry/nginx/nginx-cli-tests.sh phase43   # range
./apps/starry/nginx/nginx-cli-tests.sh phase50   # request body
./apps/starry/nginx/nginx-cli-tests.sh phase60   # log / fs
./apps/starry/nginx/nginx-cli-tests.sh phase70   # signal lifecycle
./apps/starry/nginx/nginx-cli-tests.sh phase90   # config feature

# Run all (smoke + all phases above; excludes debug/ and stress/)
./apps/starry/nginx/nginx-cli-tests.sh all
```

## Phase QEMU Retest Entries

The app provides dedicated phase retest QEMU configs:

These entries are for local verification and are not wired into CI.

```bash
cargo xtask starry app qemu -t nginx --arch x86_64 --qemu-config apps/starry/nginx/qemu-x86_64-phase1.toml
cargo xtask starry app qemu -t nginx --arch riscv64 --qemu-config apps/starry/nginx/qemu-riscv64-phase1.toml
cargo xtask starry app qemu -t nginx --arch x86_64 --qemu-config apps/starry/nginx/qemu-x86_64-phase2.toml
cargo xtask starry app qemu -t nginx --arch riscv64 --qemu-config apps/starry/nginx/qemu-riscv64-phase2.toml
```

For lifecycle retest on 4 arches:

```bash
cargo xtask starry app qemu -t nginx --arch x86_64 --qemu-config apps/starry/nginx/qemu-x86_64-phase1-2.toml
cargo xtask starry app qemu -t nginx --arch riscv64 --qemu-config apps/starry/nginx/qemu-riscv64-phase1-2.toml
cargo xtask starry app qemu -t nginx --arch aarch64 --qemu-config apps/starry/nginx/qemu-aarch64-phase1-2.toml
cargo xtask starry app qemu -t nginx --arch loongarch64 --qemu-config apps/starry/nginx/qemu-loongarch64-phase1-2.toml
cargo xtask starry app qemu -t nginx --arch x86_64 --qemu-config apps/starry/nginx/qemu-x86_64-phase1-3.toml
cargo xtask starry app qemu -t nginx --arch riscv64 --qemu-config apps/starry/nginx/qemu-riscv64-phase1-3.toml
cargo xtask starry app qemu -t nginx --arch aarch64 --qemu-config apps/starry/nginx/qemu-aarch64-phase1-3.toml
cargo xtask starry app qemu -t nginx --arch loongarch64 --qemu-config apps/starry/nginx/qemu-loongarch64-phase1-3.toml
```

## Build/Prepare Logic

`prebuild.sh` injects smoke/phase entries and shared mirror helper into guest overlay:

- `/usr/bin/nginx-smoke-tests.sh`
- `/usr/bin/nginx-phase12-tests.sh`
- `/usr/bin/nginx-phase1-tests.sh`
- `/usr/bin/nginx-phase2-tests.sh`
- `/usr/bin/nginx-alpine-mirror.sh`

Mirror helper: `apps/starry/nginx/nginx-alpine-mirror.sh`.

The mirror helper pins `apk` to the Alpine release branch of the running rootfs
(read from `/etc/alpine-release`, e.g. `v3.23`) instead of the moving
`latest-stable` alias. This keeps installed packages on the same musl/ABI as the
rootfs base; `latest-stable` can advance to a newer Alpine release whose binaries
need a newer musl (for example the `renameat2` symbol) and then fail to relocate
or crash on the current rootfs. Override the branch with `NGINX_APK_BRANCH` when
needed.
