# ArceOS-VMM-Tutorial

Let's build a VMM (Virtual Machine Minotor or hypervisor) upon [ArceOS](https://github.com/arceos-org/arceos) unikernel!

## Preparation

Initialize the ArceOS repository using Git submodule.

```console
$ git submodule init && git submodule update
```

Install [cargo-binutils](https://github.com/rust-embedded/cargo-binutils) to use `rust-objcopy` and `rust-objdump` tools:

```console
$ cargo install cargo-binutils
```

Your also need to install [musl-gcc](http://musl.cc/x86_64-linux-musl-cross.tgz) to build guest user applications.

## Build Guest OS

```console
$ cd guest/nimbos/kernel
$ make user
$ make GUEST=on
```

## Build Guest BIOS

```console
$ cd guest/bios
$ make
```

## Build File System image

```console
$ cd arceos-vmm
$ make disk_img
$ mkdir -p tmp
$ sudo mount disk.img tmp
$ # Copy guest OS binary image file.
$ sudo cp ../guest/nimbos/kernel/target/x86_64/release/nimbos.bin tmp/
$ # Copy guest BIOS binary image file.
$ sudo cp ../guest/bios/out/rvm-bios.bin tmp/
$ sudo umount tmp
```

## Build & Run Hypervisor

```console
$ cd arceos-vmm
$ make A=$(pwd) ACCEL=y BLK=y [LOG=warn|info|debug|trace] run
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

