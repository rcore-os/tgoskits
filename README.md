# ArceOS-VMM

Let's build a VMM (Virtual Machine Minotor or hypervisor) upon [ArceOS](https://github.com/arceos-org/arceos) unikernel!

Overall architecture overview can be found [here](doc/README.md).

Refer to these [discussions](https://github.com/arceos-hypervisor/arceos-umhv/discussions) to gain insights into the thoughts and future development directions of this project.

## Preparation

Install [cargo-binutils](https://github.com/rust-embedded/cargo-binutils) to use `rust-objcopy` and `rust-objdump` tools:

```console
$ cargo install cargo-binutils
```

Your also need to install [musl-gcc](http://musl.cc/x86_64-linux-musl-cross.tgz) to build guest user applications.

## Guest VM

### Configuration files

Since guest VM configuration is a complex process, ArceOS-Hypervisor chooses to use toml files to manage guest VM configuration, 
including vm id, vm name, vm type, number of CPU cores, memory size, virtual devices and pass-through devices, etc. 

We provide several configuration file [templates](arceos-vmm/configs) for setting up guest VMs. 

These configuration files are read and parsed by the `init_guest_vms()` in the [vmm/config](arceos-vmm/src/vmm/config.rs) mod, and are used to configure the guest VMs.

You can also use [axvmconfig](https://github.com/arceos-hypervisor/axvmconfig) tool to generate a custom config.toml.

For more information about VM configuration, visit [axvmconfig](https://arceos-hypervisor.github.io/axvmconfig/axvmconfig/index.html) for details.

### [Supported guest VMs](doc/GuestVMs.md)

* [ArceOS](https://github.com/arceos-org/arceos)
* [Starry-OS](https://github.com/Starry-OS)
* [NimbOS](https://github.com/equation314/nimbos)
* Linux
  * currently only Linux with passthrough device on aarch64 is tested.
  * single core: [config.toml](arceos-vmm/configs/linux-qemu-aarch64.toml) | [dts](arceos-vmm/configs/linux-qemu.dts)
  * smp: [config.toml](arceos-vmm/configs/linux-qemu-aarch64-smp2.toml) | [dts](arceos-vmm/configs/linux-qemu-smp2.dts)

### Loading Guest VM images

Currently, arceos-hypervisor supports loading guest VM images from arceos' fat file system, or binding guest VM images to hypervisor images through a static compilation manner (`include_bytes`).

* load from file system
  * specify `image_location="fs"` in the `config.toml` file.
  * `kernel_path` in `config.toml` refers to the location of the kernel image in the arceos rootfs (e.g. `disk.img`).
  * Note: `"fs"` feature is required for arceos-umhv, this can be configured via environment variables `APP_FEATURES=fs`.
* load from memory
  * specify `image_location="memory"` in the `config.toml` file.
  * `kernel_path` in `config.toml` refers to the relative/absolute path of the kernel image in your workspace when compiling arceos-vmm.
  * Note that the current method of binding guest VM images through static compilation only supports the loading of one guest VM image at most (usually we use this method to start Linux as a guest VM).

### Build File System image

```console
$ cd arceos-vmm
$ make disk_img
$
$ # Copy guest VM binary image files.
$ mkdir -p tmp
$ sudo mount disk.img tmp
$ sudo cp /PATH/TO/YOUR/GUEST/VM/IMAGE tmp/
$ sudo umount tmp
$
$ # Otherwise, set `image_location = "memory"` in CONFIG/FILE, then set kernel_path
$ # Arceos-VMM will load the image binaries from the first configuration in the VM_CONFIGS.
```

#### Build Ubuntu File System image

```console
# ARCH=(x86_64|aarch64|riscv64)
make ubuntu_img ARCH=aarch64
```

## Build & Run Hypervisor

### Example build commands

```console
$ cd arceos-vmm
# x86_64 for nimbos
# [LOG=warn|info|debug|trace]
$ make ARCH=x86_64 defconfig
$ make ACCEL=y ARCH=x86_64 LOG=info VM_CONFIGS=configs/vms/nimbos-x86_64.toml APP_FEATURES=fs run
# aarch64 for nimbos
$ make ARCH=aarch64 defconfig
$ make ACCEL=n ARCH=aarch64 LOG=info VM_CONFIGS=configs/vms/nimbos-aarch64.toml APP_FEATURES=fs run
# riscv64 for nimbos
$ make ARCH=riscv64 defconfig
$ make ACCEL=n ARCH=riscv64 LOG=info VM_CONFIGS=configs/vms/nimbos-riscv64.toml APP_FEATURES=fs run
# aarch64 for Linux (remember to change `phys-memory-size` in `arceos-vmm/configs/platforms/aarch64-qemu-virt-hv.toml` as `0x2_0000_0000` (8G))
$ make ARCH=aarch64 VM_CONFIGS=configs/vms/linux-qemu-aarch64.toml LOG=debug BUS=mmio NET=y FEATURES=page-alloc-64g MEM=8g run
# aarch64 for Linux SMP=2
$ make ARCH=aarch64 VM_CONFIGS=configs/vms/linux-qemu-aarch64-smp2.toml LOG=debug BUS=mmio NET=y  BLK=y SMP=2 FEATURES=page-alloc-64g MEM=8g run
```

### Demo Output

```console
$ cd arceos-vmm
$ make ACCEL=y ARCH=x86_64 LOG=warn VM_CONFIGS=configs/nimbos-x86_64.toml APP_FEATURES=fs run
......
Booting from ROM..
Initialize IDT & GDT...

       d8888                            .d88888b.   .d8888b.
      d88888                           d88P" "Y88b d88P  Y88b
     d88P888                           888     888 Y88b.
    d88P 888 888d888  .d8888b  .d88b.  888     888  "Y888b.
   d88P  888 888P"   d88P"    d8P  Y8b 888     888     "Y88b.
  d88P   888 888     888      88888888 888     888       "888
 d8888888888 888     Y88b.    Y8b.     Y88b. .d88P Y88b  d88P
d88P     888 888      "Y8888P  "Y8888   "Y88888P"   "Y8888P"

arch = x86_64
platform = x86_64-qemu-q35
target = x86_64-unknown-none
smp = 1
build_mode = release
log_level = warn

Starting virtualization...
Running guest...

NN   NN  iii               bb        OOOOO    SSSSS
NNN  NN       mm mm mmmm   bb       OO   OO  SS
NN N NN  iii  mmm  mm  mm  bbbbbb   OO   OO   SSSSS
NN  NNN  iii  mmm  mm  mm  bb   bb  OO   OO       SS
NN   NN  iii  mmm  mm  mm  bbbbbb    OOOO0    SSSSS
              ___    ____    ___    ___
             |__ \  / __ \  |__ \  |__ \
             __/ / / / / /  __/ /  __/ /
            / __/ / /_/ /  / __/  / __/
           /____/ \____/  /____/ /____/

arch = x86_64
platform = rvm-guest-x86_64
build_mode = release
log_level = warn

Initializing kernel heap at: [0xffffff800028ed00, 0xffffff800068ed00)
Initializing IDT...
Loading GDT for CPU 0...
Initializing frame allocator at: [PA:0x68f000, PA:0x1000000)
Mapping .text: [0xffffff8000200000, 0xffffff800021b000)
Mapping .rodata: [0xffffff800021b000, 0xffffff8000220000)
Mapping .data: [0xffffff8000220000, 0xffffff800028a000)
Mapping .bss: [0xffffff800028e000, 0xffffff800068f000)
Mapping boot stack: [0xffffff800028a000, 0xffffff800028e000)
Mapping physical memory: [0xffffff800068f000, 0xffffff8001000000)
Mapping MMIO: [0xffffff80fec00000, 0xffffff80fec01000)
Mapping MMIO: [0xffffff80fed00000, 0xffffff80fed01000)
Mapping MMIO: [0xffffff80fee00000, 0xffffff80fee01000)
Initializing drivers...
Initializing Local APIC...
Initializing HPET...
HPET: 100.000000 MHz, 64-bit, 3 timers
Calibrated TSC frequency: 2993.778 MHz
Calibrated LAPIC frequency: 1000.522 MHz
Initializing task manager...
/**** APPS ****
cyclictest
exit
fantastic_text
forktest
forktest2
forktest_simple
forktest_simple_c
forktree
hello_c
hello_world
matrix
sleep
sleep_simple
stack_overflow
thread_simple
user_shell
usertests
yield
**************/
Running tasks...
test kernel task: pid = TaskId(2), arg = 0xdead
test kernel task: pid = TaskId(3), arg = 0xbeef
Rust user shell
>> hello_world
Hello world from user mode program!
Shell: Process 5 exited with code 0
>>
......
```

### Dev Environment Setup

```shell
tool/dev_env.py
```