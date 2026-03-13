# axdriver_virtio

Wrappers of devices in the [virtio-drivers](https://docs.rs/virtio-drivers) crate that implement traits from the axdriver_* crates. For use in `no_std` environments.

Part of the [axdriver_crates](https://github.com/arceos-org/axdriver_crates) workspace.

## Features

- `alloc` – enable allocator support in virtio-drivers
- `block` – VirtIO block device (requires `axdriver_block`)
- `gpu` – VirtIO GPU (requires `axdriver_display`)
- `net` – VirtIO net (requires `axdriver_net`)

## License

GPL-3.0-or-later OR Apache-2.0 OR MulanPSL-2.0. See repository root LICENSE.
