# irq-kernel

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
Secondary CPU 1 started.
Secondary CPU 1 init OK.
Timer IRQ handler registered.
Secondary CPU 2 started.
Secondary CPU 2 init OK.
Secondary CPU 3 started.
Secondary CPU 3 init OK.
4.279632ms elapsed. Timer IRQ processed on CPU 0.
Primary CPU 0 init OK.
5.316896ms elapsed. Timer IRQ processed on CPU 3.
5.303712ms elapsed. Timer IRQ processed on CPU 2.
5.341888ms elapsed. Timer IRQ processed on CPU 1.
1.006153408s elapsed. Timer IRQ processed on CPU 0.
1.006171952s elapsed. Timer IRQ processed on CPU 1.
1.006174288s elapsed. Timer IRQ processed on CPU 3.
1.006165648s elapsed. Timer IRQ processed on CPU 2.
Primary CPU 0 finished. Shutting down...
```