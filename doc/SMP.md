# About SMP support in ArceOS-Hypervisor

## How to boot Starry and ArceOS on different cores currently

* Note that Starry and ArceOS themselves are all configured to run on a single core.
* only tested in x86_64
* refer to [README.md](../README.md) for the preparation of `disk.img`.

### Guest VM images and configs

* ArceOS binary image
    * repo: https://github.com/arceos-hypervisor/arceos/tree/gvm_sleep
    * build: 
        * `make A=examples/helloworld build`
    * copy image: 
        * `sudo cp /PATH/TO/arceos/examples/helloworld/helloworld_x86_64-qemu-q35.bin DISK/MOUNT/ON/tmp/arceos-x86-sleep.bin`
    * config file for vmm available at [arceos-x86_64-sleep.toml](../configs/vms/arceos-x86_64-sleep.toml)
    * Note: ArceOS use COM1 at **0x2f8** for serial output.

* Starry binary image
    * repo: https://github.com/arceos-org/starry-next/tree/tick_loop
    * build: 
        * `./scripts/get_deps.sh`
        * `make user_apps`
        * `make ARCH=x86_64 build`
    * copy image: 
        * `sudo cp /PATH/TO/starry-next/starry-next_x86_64-qemu-q35.bin DISK/MOUNT/ON/tmp/starry-x86_64.bin`
    * config file for vmm available at [starry-x86_64.toml](../configs/vms/starry-x86_64.toml)

### How to run

* open first terminal
    * `make ARCH=x86_64 ACCEL=y VM_CONFIGS=configs/vms/arceos-x86_64-sleep.toml:configs/vms/starry-x86_64.toml SMP=2 SECOND_SERIAL=y run`
    * ArceOS-hypervisor itself and Starry-next will print to this terminal.
    * `SECOND_SERIAL=y` will make qemu open a second serial port and listen on the socket interface from localhost constrained by the `TELNET_PORT` variable (default is 4321, currently only valid under `qemu_system_x86_64`)
        ```bash
        qemu-system-x86_64: -serial telnet:localhost:4321,server: info: QEMU waiting for connection on: disconnected:telnet:127.0.0.1:4321,server=on
        ```

* open another terminal
    * `telnet localhost 4321`
    * ArceOS as guest VM will print to this terminal.

## How to boot a guest OS with SMP support

Currently, the arceos-hypervisor supports booting a guest VM configured with multiple cores (vCPUs). 

> Due to the lack of interrupt virtualization support in this project, each vCPU is currently pinned to a specific physical core. 
> Once interrupt virtualization is supported, flexible many-to-many scheduling between vCPUs and physical cores will be enabled.

### Guest VM images and configs

Refer to [GuestVMs.md](./GuestVMs.md) for available guest VMs.

[arceos-aarch64-smp.toml](../configs/vms/arceos-aarch64-smp.toml) and [arceos-riscv64-smp.toml](../configs/vms/arceos-riscv64-smp.toml) provide templates for configuring multiple vCPUs for a guest VM. 

Key configuration options include:
* `cpu_num`: Specifies the number of vCPUs required by the guest VM.
* `phys_cpu_ids`: Represents the physical CPU IDs from the guest VM’s perspective. Due to certain hardware platform requirements with clustered CPU designs, physical CPU IDs may be non-contiguous. This information can be retrieved from the DTB.
* `phys_cpu_sets`: Defines the affinity bitmap for each vCPU, binding it to specific physical CPUs.
    * For example, in an SMP configuration with `cpu_num = 2`, if the `phys_cpu_set` for a particular vCPU is set to 0x1 (0b01), that vCPU will only run on physical core 0. If set to 0x3 (0b11), it could be scheduled on either core 0 or core 1. If set to 0x8 (0b1000), it will result in an error due to exceeding the available physical cores.

    Example configuration:

    ```toml
    cpu_num = 4
    phys_cpu_ids = [0x00, 0x100, 0x200, 0x300]
    phys_cpu_sets = [0x1, 0x2, 0xC, 0xC]
    ```
    * In this example, the guest VM is configured with 4 vCPUs. The physical CPU IDs for the vCPUs are 0x00, 0x100, 0x200, and 0x300, respectively. The first vCPU is fixed to run on physical core 0, the second on physical core 1, while the third and fourth vCPUs can be scheduled on either physical core 2 or core 3.

### How to run

* example command

    ```bash
    make ARCH=aarch64 VM_CONFIGS=configs/vms/arceos-aarch64-smp.toml SMP=2 run
    ```