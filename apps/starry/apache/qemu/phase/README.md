# Apache Phase QEMU

This directory contains QEMU configs for single Apache phase reruns.

Use `cargo xtask starry app qemu -t apache --arch <arch> --qemu-config apps/starry/apache/qemu/phase/qemu-<arch>-phaseXX.toml`.

Phase20 validates prefork startup, request handling, and clean stop. Restart
and lifecycle behavior are covered in phase50.
