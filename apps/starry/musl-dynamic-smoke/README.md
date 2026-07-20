# musl-dynamic-smoke

> Alpine/musl 用户态动态链接兼容性 smoke workflow

This case is an operator-facing StarryOS app workflow, not a unit test.
It verifies that StarryOS can boot a QEMU guest, prepare a rootfs overlay,
install the musl dynamic loader and shared libraries, and execute a dynamically
linked Linux user program through PT_INTERP.

## 测试命令

```bash
cargo xtask starry app run -t musl-dynamic-smoke --arch aarch64
cargo xtask starry app run -t musl-dynamic-smoke --arch riscv64
cargo xtask starry app run -t musl-dynamic-smoke --arch x86_64
```

## Result

| 架构 | 结果 |
|------|------|
| aarch64 | **PASS** |
| riscv64 | **PASS** |
| x86_64 | **PASS** |

```
dynamic musl test OK
DYNAMIC_MUSL_TEST_DONE RC=0
```

## 结论

StarryOS can load a dynamic musl ELF through PT_INTERP on aarch64, riscv64, and x86_64.
No missing syscall or loader blocker observed.

| 架构 | INTERP | NEEDED |
|------|--------|--------|
| aarch64 | `/lib/ld-musl-aarch64.so.1` | `libc.musl-aarch64.so.1` |
| riscv64 | `/lib/ld-musl-riscv64.so.1` | `libc.musl-riscv64.so.1` |
| x86_64 | `/lib/ld-musl-x86_64.so.1` | `libc.musl-x86_64.so.1` |

## 环境依赖

| 依赖 | 版本/说明 |
|------|----------|
| Rust toolchain | `nightly-2026-07-15` |
| lld | clang 链接器，`apt-get install lld-14`（提供 `lld` 或 `ld.lld` 二者之一即可，`prebuild.sh` 自动识别） |
| clang | 交叉编译器，支持 `--target={aarch64,riscv64,x86_64}-linux-musl` |
| debugfs | 从 rootfs 提取 sysroot |
| qemu-system-aarch64 / qemu-system-riscv64 / qemu-system-x86_64 | QEMU 系统模拟 |

## 已知配置要求

1. **aarch64 QEMU 需要 `root=/dev/sda`**：在 `qemu-aarch64.toml` 的 args 中添加 `-append root=/dev/sda`。
2. **riscv64/x86_64 不需要 `-append`**：kernel 自动检测 virtio-blk 根设备。
3. **x86_64 需要 `-cpu max`**：否则 SSE4.2 指令导致 SIGILL。
4. **x86_64 `to_bin = false`**：x86_64 使用 ELF 直接运行，不转 binary。
5. **需要 build config**：必须包含 `ax-driver/virtio-blk` 等驱动特性。
6. **prebuild.sh 必须安装 `dynamic-test.sh`**：除了编译产物，还需安装运行脚本到 overlay。
7. **riscv64 lld 需要 `--strip-debug`**：musl CRT 对象包含 lld 不支持的 RISC-V debug relocation。
8. **声明依赖满足时可直接复现**：按"环境依赖"表装齐 `clang`、`lld`/`ld.lld`、`debugfs`、`qemu-system-*` 与 musl rootfs 后，"测试命令"一节中的 3 条 `cargo xtask starry app run -t musl-dynamic-smoke --arch {aarch64,riscv64,x86_64}` 可直接复现三架构 PASS，无需额外 PATH shim。

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

其中 `$MUSL_TARGET` 为 `aarch64-linux-musl`、`riscv64-linux-musl` 或 `x86_64-linux-musl`，`$SYSROOT` 从 Alpine rootfs 镜像用 `debugfs` 提取。
