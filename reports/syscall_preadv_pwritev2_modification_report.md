# preadv/pwritev/preadv2/pwritev2 修复报告

## Change 1: 修复 vectored positioned I/O syscall ABI 与语义

**修改文件**
- `os/StarryOS/kernel/src/syscall/mod.rs`
- `os/StarryOS/kernel/src/syscall/fs/io.rs`

**修改前问题**
- `pwritev2` 错误调用 `read_at`，导致写 syscall 实际执行读路径。
- `preadv2/pwritev2` raw syscall 入口只读取到 `arg4`，把 offset high 参数位置误当作 flags，漏读真实 flags `arg5`。
- `preadv2/pwritev2` 没有处理 Linux 的 `offset == -1` 语义。
- `preadv/pwritev` 对负 offset 没有显式拒绝，可能把负数转换成超大 `u64`。
- `O_APPEND` fd 上的 `pwritev/pwritev2(offset != -1)` 没有按 Linux 行为追加写入。

**Linux 对拍证据**
Linux oracle 在 Docker 中运行：

```bash
docker run --rm -v "$PWD":/workspace -w /workspace ghcr.io/rcore-os/tgoskits-container:latest bash -lc 'gcc -Wall -Wextra -O2 test-suit/starryos/syscall/preadv_pwritev2.c -o /tmp/preadv_pwritev2_linux && /tmp/preadv_pwritev2_linux'
```

关键结果以 Linux Docker 实测为准：
- `pwritev(fd, iov, 2, 5)` 返回 `3`，写出 `01234AAB89`，当前 offset 不变。
- `preadv(fd, iov, 2, 0)` 返回 `5`，当前 offset 不变。
- `pwritev2(fd, iov, 2, 5, flags=0)` 与 `pwritev` 对齐。
- `pwritev2(fd, iov, 2, -1, flags=0)` 使用并更新当前 offset。
- `preadv2(fd, iov, 2, -1, flags=0)` 使用并更新当前 offset。
- 未知 flags 返回 `-1/EOPNOTSUPP(95)`。
- `O_APPEND` fd 上 `pwritev/pwritev2(offset != -1)` 追加写入，但不更新当前 offset。

**修改方案**
- 在 syscall dispatcher 中为 `preadv/pwritev` 传入 raw ABI 的 offset high 参数并在 64-bit StarryOS 中忽略它；`offset` 仍来自 `arg3`。
- 在 `preadv2/pwritev2` 中把 flags 改为读取 `arg5`，`arg4` 作为 64-bit ABI 的 offset high 参数保留但忽略。
- 将 `pwritev2` 改为写路径。
- 抽出 `sys_preadv_at` / `sys_pwritev_at`，统一处理负 offset。
- 对 `offset == -1` 的 `preadv2/pwritev2` 调用当前 offset 的 `read/write` 路径，使其更新文件 offset。
- 对 `O_APPEND` fd 上的 positioned write 使用 backend append 路径，匹配 Linux 的追加写入且不更新当前 offset。
- 当前 StarryOS 不实现 `RWF_*` 附加语义，非零 flags 返回 `AxError::OperationNotSupported`，映射到 Linux `EOPNOTSUPP`。

**安全性与边界处理**
- `iovec` 数组通过 `vm_read` copyin，不直接解引用用户指针。
- `iovcnt > 1024` 返回 `EINVAL`。
- offset 负值在 `preadv/pwritev` 与 `preadv2/pwritev2(offset != -1)` 中返回 `EINVAL`。
- 未知/暂不支持 flags 返回 `EOPNOTSUPP`。
- 不新增 `unwrap/expect/panic`。

**对现有逻辑的影响**
- `readv/writev` 共享 `IoVectorBuf` 校验，用户地址错误更早返回 `EFAULT`。
- `preadv/pwritev/preadv2/pwritev2` 的文件 offset 行为更接近 Linux。
- 非零 `RWF_*` flags 仍未实现真实同步/nowait/append 语义，风险已明确。

**测试情况**
- Linux Docker 对拍：PASS，输出 `PV_TEST_PASS`。
- `cargo fmt`：PASS，Docker 内执行。
- `cargo xtask clippy --package starry-kernel`：未通过，失败点在既有依赖 `ax-cpu`、`rsext4`、`ax-fs-ramfs`，以及使用镜像内 `nightly-2026-04-27` 时 `ax-io` 与仓库指定 `nightly-2026-04-01` 不匹配；不是本次 syscall 修改引入。
- StarryOS/QEMU：未实际完成。`target/debug/tg-xtask starry rootfs --arch riscv64` 在 Docker 内下载 rootfs 时长时间无输出阻塞，已终止。

**当前局限**
- `RWF_HIPRI/RWF_DSYNC/RWF_SYNC/RWF_NOWAIT/RWF_APPEND` 暂未实现，统一返回 `EOPNOTSUPP`。
- pipe/socket/目录 fd 的 positioned vectored I/O 尚未扩展到完整 Linux 语义。
- QEMU 对拍二进制已能在 Docker 内交叉编译，但 rootfs 下载阻塞导致未注入运行。

## Change 2: 加强 iovec copyin 前的边界检查

**修改文件**
- `os/StarryOS/kernel/src/mm/io.rs`

**修改前问题**
- `IoVectorBuf::new` 只累计 `iov_len`，没有检查累计溢出或总长度超过 `SSIZE_MAX`。
- 对超大 `iov_len` 的坏用户地址，后续 I/O 才会触发错误，错误位置不够明确。

**Linux 对拍证据**
新增测试中的边界 case 在 Linux Docker 中实测：
- `iovcnt = 0` 返回 `0`。
- `iovcnt = 1025` 返回 `EINVAL`。
- 超大 `iov_len` 加无效用户地址返回 `EFAULT`。
- 非法 iovec 指针返回 `EFAULT`。
- 非法 `iov_base` 返回 `EFAULT`。

**修改方案**
- 对每个非零长度 iovec 先用 `check_access(iov_base, iov_len)` 做用户地址范围检查。
- 使用 `checked_add` 累加总长度。
- 总长度超过 `isize::MAX` 返回 `EINVAL`。

**安全性与边界处理**
- 零长度 iovec 不检查 `iov_base`，保持 Linux 允许零长度空指针的行为。
- 坏 iovec 指针仍由 `vm_read` 返回 `EFAULT`。
- 坏 iov_base 范围返回 `EFAULT`。

**对现有逻辑的影响**
- 影响 `readv/writev/preadv/pwritev/preadv2/pwritev2` 和 net message 中复用 `IoVectorBuf` 的路径。
- 行为更早拒绝无效用户地址，避免后续 I/O 内部触发。

**测试情况**
- Linux Docker 对拍：PASS。
- QEMU 未完成，原因同 Change 1。

**当前局限**
- `IoVectorBuf` 是方向无关校验，只检查用户地址范围，不区分读写权限位。

## Change 3: 补充可复现 Linux-vs-StarryOS 对拍测试

**修改文件**
- `test-suit/starryos/syscall/preadv_pwritev2.c`

**测试覆盖**
- 基础 `pwritev/preadv` 多 iovec。
- `preadv2/pwritev2 flags=0`。
- `preadv2/pwritev2 offset == -1`。
- `iovcnt=0/1/多 iovec/零长度 iovec/iovcnt 过大/超大长度/坏 iovec 指针/坏 iov_base`。
- 非法 fd、只读 fd 写、只写 fd 读。
- `O_APPEND` fd。
- 未知 flags。

**实际运行过的 Docker 命令**
Linux oracle：

```bash
docker run --rm -v "$PWD":/workspace -w /workspace ghcr.io/rcore-os/tgoskits-container:latest bash -lc 'gcc -Wall -Wextra -O2 test-suit/starryos/syscall/preadv_pwritev2.c -o /tmp/preadv_pwritev2_linux && /tmp/preadv_pwritev2_linux'
```

Starry riscv64 测试二进制构建：

```bash
docker run --rm -v "$PWD":/workspace -w /workspace ghcr.io/rcore-os/tgoskits-container:latest bash -lc 'mkdir -p target/starry-syscall-tests && riscv64-linux-musl-gcc -static -Wall -Wextra -O2 test-suit/starryos/syscall/preadv_pwritev2.c -o target/starry-syscall-tests/preadv_pwritev2-riscv64'
```

Starry rootfs 准备尝试：

```bash
docker run --rm -v "$PWD":/workspace -w /workspace ghcr.io/rcore-os/tgoskits-container:latest bash -lc 'target/debug/tg-xtask starry rootfs --arch riscv64'
```

结果：rootfs 准备命令在 Docker 内长时间无输出阻塞，未能进入 QEMU 注入/运行阶段。

## Change 4: 修复 bitmap-allocator 现有 clippy 阻塞

**修改文件**
- `memory/bitmap-allocator/src/lib.rs`

**修改前问题**
`cargo xtask clippy --package starry-kernel` 首次运行时在 `bitmap-allocator` 依赖处被两个 `clippy::question_mark` 既有 warning 阻塞。

**修改方案**
按 clippy 建议把 `if let Some(..) { .. } else { return None }` 改成 `?`，不改变算法语义。

**测试情况**
Docker 内执行：

```bash
docker run --rm -v "$PWD":/workspace -v /home/cg24/.cargo/registry:/opt/cargo/registry -v /home/cg24/.cargo/config.toml:/opt/cargo/config.toml:ro -w /workspace -e RUSTUP_TOOLCHAIN=nightly-2026-04-27-x86_64-unknown-linux-gnu -e CARGO_HTTP_TIMEOUT=30 ghcr.io/rcore-os/tgoskits-container:latest bash -lc 'cargo xtask clippy --package bitmap-allocator'
```

结果：PASS。

**当前局限**
该修改是为了 unblock `starry-kernel` clippy，但 `starry-kernel` 仍被其他既有依赖 clippy/toolchain 问题阻塞。
