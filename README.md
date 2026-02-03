# axdevice

**axdevice** is a reusable, OS-agnostic device abstraction layer designed for virtual machines. It allows dynamic device configuration and MMIO emulation in `no_std` environments, making it suitable for hypervisors or operating systems targeting RISC-V or AArch64.

## ✨ Highlights

- 📦 **Componentized**: Designed as a modular crate to be integrated into any OS or hypervisor.
- 🧩 **Flexible device abstraction**: Supports dynamic device registration and MMIO handling.
- 🛠️ **No `std` required**: Uses `alloc` and `core` only, suitable for bare-metal development.
- 🧵 **Thread-safe**: Devices are stored using `Arc`, ready for multicore use.
- 🧱 **Easily extensible**: Just plug in device types via `axdevice_base::BaseDeviceOps`.

## 📦 Structure

- `config.rs`: Defines `AxVmDeviceConfig`, a wrapper for device configuration input.
- `device.rs`: Defines `AxVmDevices`, manages and dispatches MMIO to registered devices.

## 📐 Dependency Graph

```text
               +-------------------+
               |  axvmconfig       | <- defines EmulatedDeviceConfig
               +-------------------+
                         |
                         v
+------------------+     uses      +-----------------------+
|  axdevice        +-------------->+  axdevice_base::trait |
|  (this crate)    |               +-----------------------+
+------------------+                      ^
        |                                 |
        v                                 |
+------------------+                      |
|  axaddrspace     | -- GuestPhysAddr ----+
+------------------+
```

## 🔁 Usage Flow

```text
[1] Load VM device config (Vec<EmulatedDeviceConfig>)
        ↓
[2] Create AxVmDeviceConfig
        ↓
[3] Pass into AxVmDevices::new()
        ↓
[4] MMIO access triggers handle_mmio_{read,write}
        ↓
[5] Device selected by GuestPhysAddr
        ↓
[6] Forwarded to BaseDeviceOps::handle_{read,write}()
```

## 🚀 Example

```rust
use axdevice::{AxVmDeviceConfig, AxVmDevices};

// Step 1: Load configuration (e.g. from .toml or hypervisor setup)
let config = AxVmDeviceConfig::new(vec![/* EmulatedDeviceConfig */]);

// Step 2: Initialize devices
let devices = AxVmDevices::new(config);

// Step 3: Emulate MMIO access
let _ = devices.handle_mmio_read(0x1000_0000, 4);
devices.handle_mmio_write(0x1000_0000, 4, 0xdead_beef);
```

## 📦 Dependencies

- [`axvmconfig`](https://github.com/arceos-hypervisor/axvmconfig.git)
- [`axaddrspace`](https://github.com/arceos-hypervisor/axaddrspace.git)
- [`axdevice_base`](https://github.com/arceos-hypervisor/axdevice_crates.git)
- `log`, `alloc`, `cfg-if`, `axerrno`

## License

Axdevice is licensed under the Apache License, Version 2.0. See the [LICENSE](./LICENSE) file for details.
