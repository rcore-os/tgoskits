# qperf

Performance analysis tools for OS kernel in QEMU

Based on [QEMU TCG Plugins](https://www.qemu.org/docs/master/devel/tcg-plugins.html)

*Experimental*

## Requirements

- QEMU Version 9.2.0 or later (Plugin API Version 4)
- Code segment address mask `0x8000_0000_0000_0000`  
  This is required to distinguish kernel code from user code. We don't want to trace user programs.
- [DWARF debugging information](https://dwarfstd.org/)
- Frame pointers enabled

## Quick Start

### 0. Rebuild kernel with debug options

To generate DWARF debugging information and enable frame pointers, the kernel image needs to be build with some options.

- Rust: pass these [codegen options](https://doc.rust-lang.org/rustc/codegen-options/index.html) to rustc via [`RUSTFLAGS` env variable](https://doc.rust-lang.org/cargo/reference/environment-variables.html) or [build.rustflags](https://doc.rust-lang.org/cargo/reference/config.html#buildrustflags) cargo configuration: `-C force-frame-pointers -C debuginfo=2 -C strip=none`
- C: pass these flags to gcc (usually via `CFLAGS`): `-fno-omit-frame-pointer -mno-omit-leaf-frame-pointer -g`

### 1. Build qperf plugin

```bash
$ cargo build --release
```

`target/release/libqperf.so` is what we need.

### 2. Install qperf-analyzer

```bash
$ cargo install --path analyzer
```

### 3. Run QEMU with qperf plugin

```bash
$ qemu-system-xxx ... -plugin path/to/libqperf.so
```

By default, it will sample at 99Hz and save intermediate results in `qperf.bin`. You can pass optional arguments to change this behaviour:

```bash
$ qemu-system-xxx ... -plugin path/to/libqperf.so,freq=101,out=kernel.bin
```

This will change qperf to sample at 101Hz and save intermediate results in `kernel.bin`.

### 4. Run analyzer

```bash
$ qperf-analyzer -h
Usage: qperf-analyzer --elf <ELF> <INPUT> <OUTPUT>

Arguments:
  <INPUT>   
  <OUTPUT>  

Options:
  -e, --elf <ELF>  
  -h, --help       Print help
```

```bash
$ qperf-analyzer -e path/to/kernel.elf path/to/qperf.bin path/to/result.folded
```

This will dump the result in the [folded stacks](https://profilerpedia.markhansen.co.nz/formats/folded-stacks/) format.

### 5. Visualization

There are many visualization options. Recommendations:
- Use [flamegraph.pl](https://github.com/brendangregg/FlameGraph#3-flamegraphpl) or [inferno-flamegraph](https://github.com/jonhoo/inferno#as-a-binary) to generate a flame graph
- **(Highly recommended)** Use [speedscope](https://www.speedscope.app/) for interactive viewing
- Convert to the [pprof](https://profilerpedia.markhansen.co.nz/formats/pprof/) format via [pprofutils folded](https://github.com/felixge/pprofutils#folded) and use visualizers like [pprof.me](https://pprof.me/)  
  Note: pprof.me can also handle the folded stacks format but it has a 2MB upload limit and files will usually exceed this limit. The pprof format is gzip compressed so it's much smaller. In contrast, speedscope processes files locally in your browser so there is no size limit. It also works with the pprof format!

### Note for Starry OS

- The default build options (`BACKTRACE=y`) should already enable all the debugging options qperf needs.
- Use `make ... run QEMU_ARGS="-plugin libqperf.so"` to enable qperf plugin.
