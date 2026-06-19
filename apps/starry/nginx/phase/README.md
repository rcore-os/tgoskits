# Nginx Phase Tests

Phase tests are organized by stage unit `x-x`.

Current scripts:

- `nginx-1-3-lifecycle-tests.sh`: covers phase 1.1, 1.2, and 1.3 lifecycle checks.
- `nginx-2-0-http-basic-tests.sh`: covers phase 2 HTTP basic semantics.

Rule:

- Each phase script must include all checks of that phase, including checks already covered by smoke.
- Phase scripts are not auto-discovered by tgoskits nginx CI. Run them manually through
  `cargo xtask starry app qemu -t nginx --arch <arch> --qemu-config apps/starry/nginx/qemu/phase/qemu-<arch>-phaseXX.toml`,
  which enters the guest via `/usr/bin/nginx-runner.sh phase phaseXX`.
