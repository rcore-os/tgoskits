# axdriver_virtio

[![Crates.io](https://img.shields.io/crates/v/axdriver_virtio)](https://crates.io/crates/axdriver_virtio)
[![Docs.rs](https://docs.rs/axdriver_virtio/badge.svg)](https://docs.rs/axdriver_virtio)
[![CI](https://github.com/arceos-org/axdriver_crates/actions/workflows/deploy.yml/badge.svg?branch=main)](https://github.com/arceos-org/axdriver_crates/actions/workflows/deploy.yml)

Wrappers of devices in the [virtio-drivers](https://docs.rs/virtio-drivers) crate that implement traits from the axdriver_* crates. For use in `no_std` environments.

Part of the [axdriver_crates](https://github.com/arceos-org/axdriver_crates) workspace.

## Features

- `alloc` – enable allocator support in virtio-drivers
- `block` – VirtIO block device (requires `axdriver_block`)
- `gpu` – VirtIO GPU (requires `axdriver_display`)
- `net` – VirtIO net (requires `axdriver_net`)

## License

GPL-3.0-or-later OR Apache-2.0 OR MulanPSL-2.0. See repository root LICENSE.
