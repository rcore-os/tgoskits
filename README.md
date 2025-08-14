# axvmconfig

[![CI](https://github.com/arceos-hypervisor/axvmconfig/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/arceos-hypervisor/axvmconfig/actions/workflows/ci.yml)

axvmconfig is a Virtual Machine configuration tool and library for [AxVisor](https://github.com/arceos-hypervisor/axvisor). It supports parsing, validating, and generating TOML configuration files for ArceOS virtual machines, enabling users to quickly deploy and manage VMs across multiple architectures. **[Work in Progress]**

## Features

- ✅ **Multi-architecture support**: riscv64, aarch64, x86_64
- ✅ **TOML configuration**: Parse and validate VM configuration files
- ✅ **Template generation**: Command-line tool to generate configuration templates

## Usage

### Command Line Tool

#### Validate configuration file

```bash
axvmconfig check --config-path path/to/config.toml
```

#### Generate configuration template

```bash
# Basic usage
axvmconfig generate -a riscv64 -k arceos-riscv64.bin -l 0x80200000

# Complete example with all options
axvmconfig generate \
  -a aarch64 \
  -i 1 \
  -n "MyVM" \
  -t 1 \
  -c 2 \
  -e 0x40080000 \
  -k arceos-aarch64.bin \
  -l 0x40080000 \
  --image-location fs \
  --cmdline "console=ttyS0" \
  -O output/
```

#### Command Line Options

```
-a --arch: Target architecture (riscv64/aarch64/x86_64)
-i --id: VM ID (default: 0)
-n --name: VM name (default: "GuestVM")
-t --vm-type: VM type (0=HostVM, 1=RTOS, 2=Linux, default: 1)
-c --cpu-num: Number of CPUs (default: 1)
-e --entry-point: Entry point address (default: 1)
-k --kernel-path: Kernel image path
-l --kernel-load-addr: Kernel load address
   --image-location: Image location ("fs"|"memory", default: "fs")
   --cmdline: Kernel command line arguments
-O --output: Output directory
-h, --help: Print help
```

## Configuration File Format

### Basic Configuration Example

```toml
[base]
id = 1
name = "GuestVM-riscv64"
vm_type = 1
cpu_num = 1
phys_cpu_sets = [1]

[kernel]
entry_point = 0x80200000
kernel_path = "arceos-riscv64.bin"
kernel_load_addr = 0x80200000
image_location = "fs"

# Memory regions format: [base_addr, size, flags, type]
memory_regions = [
    [0x80000000, 0x1000000, 0x7, 1]  # 16M RAM
]

[devices]
# Emulated devices format: [name, base_gpa, length, irq_id, emu_type, config_list]
emu_devices = []

# Passthrough devices format: [name, base_gpa, base_hpa, length, irq_id]
passthrough_devices = [
    ["PLIC@c000000", 0x0c000000, 0x0c000000, 0x210000, 0x1],
    ["UART@10000000", 0x10000000, 0x10000000, 0x1000, 0x1],
]

# Interrupt modes: no_irq | emulated | passthrough
interrupt_mode = "no_irq"
```

### VM Types

- **Type 0 (HostVM)**: Host VM for boot from Linux (similar to Jailhouse "type1.5")
- **Type 1 (RTOS)**: Guest RTOS with resource passthrough (default)
- **Type 2 (Linux)**: Full-featured Linux guest with device emulation

### Supported Devices

#### Emulated Device Types

- **Special Devices**: Dummy, InterruptController, Console, IVCChannel
- **GIC GPPT Devices**: GPPTRedistributor, GPPTDistributor, GPPTITS
- **Virtio Devices**: VirtioBlk, VirtioNet, VirtioConsole

#### Interrupt Modes

- `no_irq`: No interrupt handling
- `emulated`: Use emulated interrupt controller
- `passthrough`: Use passthrough interrupt controller

### Architecture Templates

The project includes pre-built templates for supported architectures:

- [`templates/riscv64.toml`](templates/riscv64.toml) - RISC-V 64-bit configuration
- [`templates/aarch64.toml`](templates/aarch64.toml) - ARM64 configuration
- [`templates/x86_64.toml`](templates/x86_64.toml) - x86_64 configuration

## Contributing

Contributions are welcome! Please ensure that:

1. All tests pass (`cargo test`)
2. Code is properly formatted (`cargo fmt`)
3. No clippy warnings (`cargo clippy`)
4. Documentation is updated for new features

## License

This project is licensed under multiple licenses:

- Apache License 2.0 ([LICENSE.Apache2](LICENSE.Apache2))
- GNU General Public License v3.0 ([LICENSE.GPLv3](LICENSE.GPLv3))
- Mulan Permissive Software License v2 ([LICENSE.MulanPSL2](LICENSE.MulanPSL2))
- Mulan Public License v2 ([LICESNE.MulanPubL2](LICESNE.MulanPubL2))

## Related Projects

- [AxVisor](https://github.com/arceos-hypervisor/axvisor) - The main hypervisor project
