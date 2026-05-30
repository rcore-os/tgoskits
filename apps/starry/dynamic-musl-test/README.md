# dynamic-musl-test

验证 StarryOS 加载动态链接 musl ELF 的能力。

## 测试命令

```bash
cargo xtask starry app run -t dynamic-musl-test --arch aarch64
cargo xtask starry app run -t dynamic-musl-test --arch riscv64
```

## Result

| 架构 | 结果 |
|------|------|
| aarch64 | **PASS** |
| riscv64 | **PASS** |

```
dynamic musl test OK
DYNAMIC_MUSL_TEST_DONE RC=0
```

## 结论

StarryOS can load a dynamic musl ELF through PT_INTERP on aarch64 and riscv64.
No missing syscall or loader blocker observed.

| 架构 | INTERP | NEEDED |
|------|--------|--------|
| aarch64 | `/lib/ld-musl-aarch64.so.1` | `libc.musl-aarch64.so.1` |
| riscv64 | `/lib/ld-musl-riscv64.so.1` | `libc.musl-riscv64.so.1` |

## 环境依赖

| 依赖 | 版本/说明 |
|------|----------|
| Rust toolchain | `nightly-2026-05-28` |
| lld | clang 链接器，`apt-get install lld-14` |
| clang | 交叉编译器，支持 `--target=aarch64-linux-musl` 和 `--target=riscv64-linux-musl` |
| debugfs | 从 rootfs 提取 sysroot |
| qemu-system-aarch64 / qemu-system-riscv64 | QEMU 系统模拟 |

## 已知配置要求

1. **aarch64 QEMU 需要 `root=/dev/sda`**：在 `qemu-aarch64.toml` 的 args 中添加 `-append root=/dev/sda`。
2. **riscv64 不需要 `-append`**：kernel 自动检测 virtio-blk 根设备。
3. **需要 build config**：必须包含 `ax-driver/virtio-blk` 等驱动特性。
4. **prebuild.sh 必须安装 `dynamic-test.sh`**：除了编译产物，还需安装运行脚本到 overlay。
5. **riscv64 lld 需要 `--strip-debug`**：musl CRT 对象包含 lld 不支持的 RISC-V debug relocation。

## 编译方式

使用 clang + lld 交叉编译，prebuild.sh 自动处理：

```bash
clang --target=$MUSL_TARGET \
    --sysroot=$SYSROOT \
    -isystem $SYSROOT/usr/include \
    -fuse-ld=lld \
    -nostdlib \
    -Wl,--strip-debug \
    -L$SYSROOT/usr/lib \
    -Wl,--library-path=$SYSROOT/usr/lib \
    $SYSROOT/usr/lib/Scrt1.o \
    $SYSROOT/usr/lib/crti.o \
    -lc \
    $SYSROOT/usr/lib/crtn.o \
    -o dynamic-test \
    dynamic-test.c
```

其中 `$MUSL_TARGET` 为 `aarch64-linux-musl` 或 `riscv64-linux-musl`，`$SYSROOT` 从 Alpine rootfs 镜像用 `debugfs` 提取。
