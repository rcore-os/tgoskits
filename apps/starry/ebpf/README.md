# eBPF apps

## 依赖安装

1. stable rust toolchains: `rustup toolchain install stable`
1. nightly rust toolchains: `rustup toolchain install nightly --component rust-src`
1. (if cross-compiling) rustup target: `rustup target add ${ARCH}-unknown-linux-musl`
1. (if cross-compiling) LLVM: (e.g.) `brew install llvm` (on macOS)
1. (if cross-compiling) C toolchain: (e.g.) [`brew install filosottile/musl-cross/musl-cross`](https://github.com/FiloSottile/homebrew-musl-cross) (on macOS)
1. bpf-linker: `cargo install bpf-linker` (`--no-default-features` on macOS)


## APP说明
1. syscall_count: 通过kprobe hook内核系统调用分发函数，统计系统调用次数
2. kret: 通过kretprobe hook内核的sys_getpid函数，统计getpid调用的返回值。程序会在后台运行，因此你可以使用 `ls` / `uname` 等命令来触发sys_getpid系统调用。由于sys_getpid的返回值是`Result<>`类型，有些架构可能将返回值保存在栈上，有些则是用两个寄存器返回，因此程序的结果在不同架构上可能会有所不同。
3. mytrace: 通过tracepoint hook内核的sys_enter函数(sys_openat)，打印路径。程序会在后台运行，因此你可以使用 `ls /home` 等命令来触发sys_openat系统调用。
4. rawtp: 通过raw tracepoint hook内核的sys_clone函数，打印调用参数。程序会在后台运行，因此你可以使用 `ls` / `uname` 等命令来触发sys_clone系统调用。
5. upb: 通过uprobe hook用户态的函数，打印调用参数。
6. upb2: 通过uprobe hook用户态的libc库函数(mkdir)，打印调用参数。

## 支持状态

| app           | x86_64             | riscv64            | aarch64            | loongarch64        |
| ------------- | ------------------ | ------------------ | ------------------ | ------------------ |
| syscall_count | :white_check_mark: | :white_check_mark: | :white_check_mark: | :white_check_mark: |
| kret          | :white_check_mark: | :white_check_mark: | :white_check_mark: | :white_check_mark: |
| mytrace       | :white_check_mark: | :white_check_mark: | :white_check_mark: | :white_check_mark: |
| rawtp         | :white_check_mark: | :white_check_mark: | :white_check_mark: | :white_check_mark: |
| upb           | :white_check_mark: | :white_check_mark: | :white_check_mark: | :white_check_mark: |
| upb2          | :white_check_mark: | :white_check_mark: | :white_check_mark: | :white_check_mark: |
| profile       | :white_check_mark: | :white_check_mark: | :white_check_mark: | :white_check_mark: |
| sched_trace   | :white_check_mark: | :white_check_mark: | :white_check_mark: | :white_check_mark: |

## Build & Run

```sh
cargo xtask starry app qemu -t ebpf/{?} --arch {?}
```
