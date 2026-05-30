# glibc-test

验证 StarryOS 运行 glibc 动态链接 binary 的可行性。

## 测试命令

```bash
cargo xtask starry app run -t glibc-test --arch aarch64
cargo xtask starry app run -t glibc-test --arch riscv64
cargo xtask starry app run -t glibc-test --arch x86_64
```

## Result

| 架构 | rootfs | glibc-test | proc-self-exe | pthread | regex |
|------|--------|------------|---------------|---------|-------|
| aarch64 | Alpine | **PASS** | **PASS** | **PASS** | **PASS** |
| aarch64 | Debian | **PASS** | **PASS** | **PASS** | **PASS** |
| riscv64 | Alpine | **PASS** | **PASS** | **PASS** | **PASS** |
| x86_64 | Alpine | **PASS** | **PASS** | **PASS** | **PASS** |

## 结论

StarryOS can load glibc dynamic ELF through PT_INTERP on aarch64, riscv64, and x86_64.
- /proc/self/exe: available on all architectures
- pthread: working on all architectures
- regex: working on all architectures
- No missing syscall or loader blocker observed.

## INTERP 路径

| 架构 | INTERP | NEEDED |
|------|--------|--------|
| aarch64 | `/lib/ld-linux-aarch64.so.1` | `libc.so.6` |
| riscv64 | `/lib/ld-linux-riscv64-lp64d.so.1` | `libc.so.6` |
| x86_64 | `/lib64/ld-linux-x86-64.so.2` | `libc.so.6` |

## 测试内容

- `glibc-test` - 最小 printf 程序
- `proc-self-exe-test` - /proc/self/exe readlink 验证
- `pthread-test` - pthread 线程创建/同步
- `regex-test` - POSIX regex 正则表达式

## 文件说明

- `glibc-test.c` - 最小 glibc 动态链接测试程序
- `proc-self-exe-test.c` - /proc/self/exe 验证程序
- `pthread-test.c` - pthread 测试程序
- `regex-test.c` - regex 测试程序
- `prebuild.sh` - 编译 + 安装到 overlay（多架构支持）
- `glibc-test.sh` - QEMU 内运行的测试脚本
- `qemu-aarch64.toml` / `qemu-riscv64.toml` / `qemu-x86_64.toml` - QEMU 运行配置
- `build-aarch64-unknown-none-softfloat.toml` / `build-riscv64gc-unknown-none-elf.toml` / `build-x86_64-unknown-none.toml` - 内核构建配置

## 环境依赖

| 依赖 | 版本/说明 |
|------|----------|
| Rust toolchain | `nightly-2026-05-28` |
| aarch64-linux-gnu-gcc | Debian 交叉编译工具链 |
| riscv64-linux-gnu-gcc | Debian 交叉编译工具链 |
| x86_64-linux-gnu-gcc | Debian 交叉编译工具链 |
| readelf | binutils |
| qemu-system-aarch64 / qemu-system-riscv64 / qemu-system-x86_64 | QEMU 系统模拟 |

## 已知配置要求

| 架构 | 特殊要求 |
|------|----------|
| aarch64 | QEMU 需要 `-append root=/dev/sda` |
| riscv64 | 不需要 `-append`，kernel 自动检测 |
| x86_64 | 需要 `-cpu max`，`to_bin=false`，不需要 `-append` |
