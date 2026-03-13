# smp-kernel

A minimal example of a kernel with multi-core and timer interrupt support using [axplat](../../axplat) and related [platform crates](../../platforms).

# Build & Run

```bash
make ARCH=<arch> run
```

Where `<arch>` is one of `x86_64`, `aarch64`, `riscv64`, or `loongarch64`.

It will run the minimal kernel in QEMU and output a message of the following form:

```
Hello, ArceOS!
Primary CPU 0 started.
Timer IRQ handler registered.
2.57776ms elapsed. Timer IRQ processed on CPU 0.
Primary CPU 0 init OK.
1.004237792s elapsed. Timer IRQ processed on CPU 0.
Primary CPU 0 finished. Shutting down...
```

If you want to run the kernel with multiple cores, you can add `SMP` argument to the `make` command. For example, to run with 4 cores:

```bash
make ARCH=<arch> run SMP=4
```

In this case, the output will be similar to:

```
Hello, ArceOS!
Primary CPU 0 started.
Secondary CPU 1 init OK.
Secondary CPU 2 init OK.
Secondary CPU 3 init OK.
Timer IRQ handler registered.
43.6855ms elapsed. Timer IRQ processed on CPU 0.
Primary CPU 0 init OK.
44.741ms elapsed. Timer IRQ processed on CPU 1.
44.775ms elapsed. Timer IRQ processed on CPU 3.
44.7756ms elapsed. Timer IRQ processed on CPU 2.
1.0462813s elapsed. Timer IRQ processed on CPU 3.
1.0464641s elapsed. Timer IRQ processed on CPU 1.
1.046386s elapsed. Timer IRQ processed on CPU 2.
1.0463975s elapsed. Timer IRQ processed on CPU 0.
...
4.0457381s elapsed. Timer IRQ processed on CPU 1.
4.0457352s elapsed. Timer IRQ processed on CPU 0.
4.045778s elapsed. Timer IRQ processed on CPU 2.
4.0457964s elapsed. Timer IRQ processed on CPU 3.
Primary CPU 0 finished. Shutting down...
```