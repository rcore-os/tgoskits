# hello-kernel

A minimal example of writing a kernel using [axplat](../../axplat) and related [platform crates](../../platforms).

# Build & Run

```bash
make ARCH=<arch> run
```

Where `<arch>` is one of `x86_64`, `aarch64`, `riscv64`, or `loongarch64`.

It will run the minimal kernel in QEMU and output the following message:

```
Hello, ArceOS!
cpu_id = 0, arg = 0x44000000
1.001816s elapsed.
2.002296s elapsed.
3.002432s elapsed.
4.002546s elapsed.
5.00267s elapsed.
All done, shutting down!
```
