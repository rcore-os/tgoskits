# Apache All QEMU

This directory contains QEMU configs for the Apache `all` flow.

Use `cargo xtask starry app qemu -t apache --arch <arch> --qemu-config apps/starry/apache/qemu/all/qemu-<arch>.toml`.

`all` runs smoke, then phases 20, 30, 40, 50, 55, 70, and 80 in order.
