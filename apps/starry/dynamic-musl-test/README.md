# dynamic-musl-test

验证 StarryOS aarch64 加载动态链接 musl ELF 的能力。

## 测试命令

```bash
cargo xtask starry app run -t dynamic-musl-test --arch aarch64
```

## Result

**PASS on aarch64**

```
dynamic musl test OK
DYNAMIC_MUSL_TEST_DONE RC=0
```

## 结论

StarryOS can load a dynamic musl ELF through PT_INTERP on aarch64.
No missing syscall or loader blocker observed.

- INTERP: `/lib/ld-musl-aarch64.so.1`
- NEEDED: `libc.musl-aarch64.so.1`
- binary type: ELF 64-bit LSB pie executable, ARM aarch64, dynamically linked

## 环境依赖

| 依赖 | 版本/说明 |
|------|----------|
| Rust toolchain | `nightly-2026-05-28` |
| lld | clang 链接器，`apt-get install lld-14` |
| clang | 交叉编译器，`--target=aarch64-linux-musl` |
| debugfs | 从 rootfs 提取 sysroot |
| qemu-aarch64 | QEMU user-mode（构建系统需要） |

## 已知配置要求

1. **QEMU 需要 `root=/dev/sda`**：在 `qemu-aarch64.toml` 的 args 中添加 `-append root=/dev/sda`，否则 kernel 无法识别根设备。
2. **需要 `build-aarch64-unknown-none-softfloat.toml`**：必须包含 `ax-driver/virtio-blk` 等驱动特性，否则 kernel 不会初始化块设备。
3. **prebuild.sh 必须安装 `dynamic-test.sh`**：除了编译产物 `dynamic-test`，还需安装运行脚本到 overlay。

## 编译方式

使用 clang + lld 交叉编译：

```bash
clang --target=aarch64-linux-musl \
    --sysroot=$SYSROOT \
    -isystem $SYSROOT/usr/include \
    -fuse-ld=lld \
    -nostdlib \
    -L$SYSROOT/usr/lib \
    -Wl,--library-path=$SYSROOT/usr/lib \
    $SYSROOT/usr/lib/Scrt1.o \
    $SYSROOT/usr/lib/crti.o \
    -lc \
    $SYSROOT/usr/lib/crtn.o \
    -o dynamic-test \
    dynamic-test.c
```

其中 `$SYSROOT` 从 Alpine rootfs 镜像用 `debugfs -R "rdump / $sysroot" rootfs.img` 提取。
