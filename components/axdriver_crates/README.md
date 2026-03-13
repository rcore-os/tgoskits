# axdriver_crates

Crates for building device driver subsystems in the `no_std` environment:

- [axdriver_base](https://github.com/arceos-org/axdriver_crates/tree/main/axdriver_base): Common interfaces for all kinds of device drivers.
- [axdriver_block](https://github.com/arceos-org/axdriver_crates/tree/main/axdriver_block): Common traits and types for block storage drivers.
- [axdriver_net](https://github.com/arceos-org/axdriver_crates/tree/main/axdriver_net): Common traits and types for network device (NIC) drivers.
- [axdriver_display](https://github.com/arceos-org/axdriver_crates/tree/main/axdriver_display): Common traits and types for graphics device drivers.
- [axdriver_pci](https://github.com/arceos-org/axdriver_crates/tree/main/axdriver_pci): Structures and functions for PCI bus operations.
- [axdriver_virtio](https://github.com/arceos-org/axdriver_crates/tree/main/axdriver_virtio): Wrappers of some devices in the [virtio-drivers](https://docs.rs/virtio-drivers) crate, that implement traits in the `axdriver`-series crates.
