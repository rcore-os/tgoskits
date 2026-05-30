# glibc-test

验证 StarryOS aarch64 运行 glibc 动态链接 binary 的可行性。

## 测试命令

```bash
cargo xtask starry app run -t glibc-test --arch aarch64
```

## Result

**PASS on aarch64**

```
glibc dynamic test OK
GLIBC_TEST_DONE RC=0
```

## 结论

StarryOS can load a glibc dynamic ELF through PT_INTERP on aarch64.
No missing syscall or loader blocker observed.

- INTERP: `/lib/ld-linux-aarch64.so.1`
- NEEDED: `libc.so.6`

## 文件说明

- `glibc-test.c` - 最小 glibc 动态链接测试程序
- `proc-self-exe-test.c` - /proc/self/exe 验证程序
- `prebuild.sh` - 编译 + 安装到 overlay（aarch64-linux-gnu-gcc）
- `glibc-test.sh` - QEMU 内运行的测试脚本
- `qemu-aarch64.toml` - QEMU 运行配置
- `build-aarch64-unknown-none-softfloat.toml` - 内核构建配置

## 环境依赖

| 依赖 | 版本/说明 |
|------|----------|
| Rust toolchain | `nightly-2026-05-28` |
| aarch64-linux-gnu-gcc | Debian 交叉编译工具链 |
| readelf | binutils |
| qemu-system-aarch64 | QEMU 系统模拟 |
