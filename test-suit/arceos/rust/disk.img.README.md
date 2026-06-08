The unified `fs-basic` test expects a writable raw disk image at the path wired
by the QEMU config:

`tmp/axbuild/runtime-assets/arceos/rust/disk.img`

The axbuild test flow is expected to prepare that runtime asset before launching
QEMU cases that enable `fs-basic` or `all`.
