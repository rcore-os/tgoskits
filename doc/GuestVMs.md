# ArceOS-Hypervisor Supported GuestVMs

## [NimbOS](https://github.com/arceos-hypervisor/nimbos)

* Simple real time guest VM that can only be used for **single-core** testing
* It supports the x86_64, aarch64, and riscv64 architectures
* Configuration file templates at [nimbos-aarch64.toml](../arceos-vmm/configs/nimbos-aarch64.toml), [nimbos-x86_64.toml](../arceos-vmm/configs/nimbos-x86_64.toml), and [nimbos-riscv64.toml](../arceos-vmm/configs/nimbos-riscv64.toml)
* Kernel binary images availble at [nimbos/releases](https://github.com/arceos-hypervisor/nimbos/releases/tag/v0.6)

## [ArceOS](https://github.com/arceos-hypervisor/arceos)
* Used for **SMP** testing
* It supports the x86_64, aarch64, and riscv64 architectures
* Configuration file templates at [arceos-aarch64.toml](../arceos-vmm/configs/arceos-aarch64.toml), [arceos-x86_64.toml](../arceos-vmm/configs/arceos-x86_64.toml), and [arceos-riscv64.toml](../arceos-vmm/configs/arceos-riscv64.toml)

### Testcases

* **Hypercall**:
    * ArceOS HelloWorld application that can be used to test hypercall functionality is provided [here](https://github.com/arceos-hypervisor/arceos/blob/gvm_test/examples/helloworld/src/main.rs)
    * Just run `make A=examples/helloworld ARCH=[x86_64|aarch64|riscv64] build` to get binary images 

* **virtio-pci-devices (PCI)**: 
    * Branch (pci_pio)[https://github.com/hky1999/arceos/tree/pci_pio] can be used for virtio-pci devices testing (PCI device probed through port I/O)

## [axvm-bios](https://github.com/arceos-hypervisor/axvm-bios-x86)

* A extremely simple bios for x86_64 guests
* It can act as a bootloader for NimbOS and ArceOS
* Binary product available at [here](https://github.com/arceos-hypervisor/axvm-bios-x86/releases/download/v0.1/axvm-bios.bin)