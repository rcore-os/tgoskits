# ARM GIC Driver

A Rust driver for the ARM Generic Interrupt Controller (GIC), designed for bare-metal and OS kernel environments.

## Features

- **Multi-version Support**: Compatible with GICv1, GICv2, GICv3
- **Memory Safety**: Written in safe Rust with zero-cost abstractions
- **No Standard Library**: `#![no_std]` compatible for embedded environments
- **Type Safety**: Strong typing for interrupt IDs and register access
- **Comprehensive Testing**: Extensive test suites for different GIC versions

### Basic Usage

```rust
use arm_gic_driver::v3::*;

let mut gic = unsafe { Gic::new(0xF901_0000.into(), 0xF902_0000.into()) };
gic.init();

// Every CPU should initialize its own CPU interface
let mut cpu = gic.cpu_interface();
cpu.init_current_cpu().unwrap();

// It implements `Send` and `Sync` traits, so it can be used as static value in trap handler easily.
let trap = cpu.trap_operations();

// Enable an Timer interrupt
let irq_id = IntId::ppi(14);
gic.set_irq_enable(irq_id, true);

// Set interrupt priority
gic.set_priority(irq_id, 0x80);

// Acknowledge and handle interrupts group1
let ack = trap.ack1();
if !ack.is_special() {
    trap.eoi1(ack);
    if trap.eoi_mode() {
        trap.dir(ack);
    }
}
```

## Architecture Support

This driver supports multiple ARM GIC versions:

- **GICv1**: Legacy interrupt controller
- **GICv2**: Supports up to 8 CPU cores
- **GICv3**: Scalable to many cores, supports message-based interrupts

## Building

### Prerequisites

- Rust nightly toolchain (specified in `rust-toolchain.toml`)
- QEMU (for running platform tests)

## Testing

The project includes comprehensive test suites for different platforms and GIC versions:

### Running Platform Tests

```bash
# Test GICv2 on AArch64
./test.sh

# Test with EL2 support
./test-el2.sh

# Test GICv3
./test_v3.sh
```

## API Overview

### Core Types

- `VirtAddr`: Type-safe virtual address wrapper
- `IntId`: Interrupt identifier with validation
- `v2::Gic`, `v3::Gic`: Version-specific GIC implementations

## Examples

See the `itest/` directory for comprehensive examples of using the driver in different scenarios.

## Contributing

Contributions are welcome! Please ensure that:

1. All tests pass: `cargo test --workspace`
2. Code is properly formatted: `cargo fmt`
3. No clippy warnings: `cargo clippy`
4. Platform tests pass on relevant architectures

## License

This project is licensed under the Mulan PSL v2 License. See the [LICENSE](LICENSE) file for details.
