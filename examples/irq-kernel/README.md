# irq-kernel

A minimal example of a kernel that supports IRQ handling using [axplat](../../axplat) and related [platform crates](../../platforms).

# Build & Run

```bash
cargo install cargo-axplat
make ARCH=<arch> run
```

Where `<arch>` is one of `x86_64`, `aarch64`, `riscv64`, or `loongarch64`.

It will run the minimal kernel in QEMU and output a message of the following form:

```
Hello, ArceOS!
cpu_id = 0, arg = 0x9500
Timer IRQ handler registered.
Waiting for timer IRQs for 5 seconds...
1.001044423s elapsed. 101 Timer IRQ processed.
2.001658048s elapsed. 201 Timer IRQ processed.
3.001710074s elapsed. 301 Timer IRQ processed.
4.001761733s elapsed. 401 Timer IRQ processed.
5.001813006s elapsed. 501 Timer IRQ processed.
Timer IRQ count: 501
Timer IRQ test passed.
```
