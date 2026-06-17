# Nginx Debug Tests

This directory stores flexible debug scripts for single issue reproduction and diagnosis.

Current scripts:

- `nginx-http-basic-tests.sh`: early HTTP basic script kept for issue-level debugging.
- `nginx-2-0-bad-method-debug.sh`: focused probe for stage 2.0 BAD method (`BAD / HTTP/1.1`) instability.
- `nginx-3-1-short-connection-debug.sh`: focused reproduction for phase31 short-connection timeout behavior.
- `nginx-3-1-x86-timing-debug.sh`: x86_64 timing microbench for process, timeout, and curl loops.

Rule:

- Debug scripts are free-form and can focus on one syscall or one behavior path.
- Debug scripts are not auto-discovered by tgoskits nginx CI. Run them manually through
  `cargo xtask starry app qemu -t nginx --arch <arch> --qemu-config apps/starry/nginx/qemu/debug/<config>.toml`,
  which enters the guest via `/usr/bin/nginx-runner.sh debug <name>`.
