# qperf

Performance analysis tools for OS kernel in QEMU

Based on [QEMU TCG Plugins](https://www.qemu.org/docs/master/devel/tcg-plugins.html)

*Experimental*

## Requirements

- QEMU 11.1.1（Plugin API v7）。不支持旧插件 API；本地绑定和回调所有权见
  [API v7 适配](ABI.md)。
- A kernel text address range passed through `filter_start`/`filter_end` when kernel-only sampling is
  required. Do not infer an x86_64 low-address alias from the high-half address: UEFI and
  identity-mapped code can occupy the same low window.
- [DWARF debugging information](https://dwarfstd.org/)
- Frame pointers enabled

## Quick Start

### 0. Rebuild kernel with debug options

To generate DWARF debugging information and enable frame pointers, the kernel image needs to be build with some options.

- Rust: pass these [codegen options](https://doc.rust-lang.org/rustc/codegen-options/index.html) to rustc via [`RUSTFLAGS` env variable](https://doc.rust-lang.org/cargo/reference/environment-variables.html) or [build.rustflags](https://doc.rust-lang.org/cargo/reference/config.html#buildrustflags) cargo configuration: `-C force-frame-pointers -C debuginfo=2 -C strip=none`
- C: pass these flags to gcc (usually via `CFLAGS`): `-fno-omit-frame-pointer -mno-omit-leaf-frame-pointer -g`

### 1. Build qperf plugin

```bash
apps/qperf/prebuild.sh cargo build --release -p qperf -p qperf-analyzer --target-dir tools/qperf/target
```

从仓库根目录执行上述命令，插件位于 `tools/qperf/target/release/libqperf.so`。
`apps/qperf` 和 `cargo xtask starry perf` 都使用本地源码，不下载外部插件实现。

### 2. Install qperf-analyzer

```bash
cargo install --path tools/qperf/analyzer
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
- The plugin recognizes QEMU targets `riscv64`, `loongarch64`, and `x86_64`.
- Prefer `cargo xtask starry perf`; it builds the plugin/analyzer, prepares the matching StarryOS QEMU
  boot flow, supplies kernel ranges, and generates the report artifacts.
- For an x86_64 kernel boot profile, use:

```bash
cargo xtask starry perf --arch x86_64 --kernel-filter --format folded \
    --shell-init-cmd "echo QPERF_BOOT_DONE" \
    --stop-marker "QPERF_BOOT_DONE" --timeout 60
```

加载、退出、参数错误及实际采样回归使用真实 QEMU 11.1.1。用户态采样回归需要
`qemu-x86_64`、宿主 C 编译器及静态 libc，覆盖 TB/指令采样、双线程及 FP 解栈：

```bash
python3 tools/qperf/tests/prebuild.py
python3 tools/qperf/tests/qemu_load.py tools/qperf/target/release/libqperf.so
python3 tools/qperf/tests/qemu_sample.py tools/qperf/target/release/libqperf.so tools/qperf/target/release/qperf-analyzer
```
