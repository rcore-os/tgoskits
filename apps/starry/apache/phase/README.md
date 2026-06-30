# Apache Phase Tests

Phase tests are focused Apache feature and StarryOS semantic checks.

Invoke them with `cargo xtask starry app qemu -t apache --arch <arch>` and the
matching `apps/starry/apache/qemu/phase/qemu-<arch>-phaseXX.toml`.

Before running a phase test on StarryOS, run the same script or equivalent
commands in Linux Alpine and keep the Linux result as the behavior oracle.
