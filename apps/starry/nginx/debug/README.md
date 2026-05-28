# Nginx Debug Tests

This directory stores flexible debug scripts for single issue reproduction and diagnosis.

Current scripts:

- `nginx-http-basic-tests.sh`: early HTTP basic script kept for issue-level debugging.
- `nginx-multiworker-hang-analysis.md`: root-cause record for Starry multi-worker quit/hang path.
- `nginx-multiworker-quit-tests.sh`: minimal multi-worker quit/reap regression with short timeouts.
- `qemu-riscv64-multiworker.toml`: qemu config that runs the debug multi-worker regression directly.
- `qemu-x86_64-multiworker.toml`: x86_64 qemu config for the same regression.

Rule:

- Debug scripts are free-form and can focus on one syscall or one behavior path.
- Debug scripts are not connected to tgoskits nginx CI entry.
