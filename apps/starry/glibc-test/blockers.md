# glibc-test Blocker 清单

> 最后更新：2026-05-30

## 当前状态

**aarch64 glibc 动态链接：PASS**

```
glibc dynamic test OK
GLIBC_TEST_DONE RC=0
```

## Blocker 列表

无 blocker。glibc 基础动态链接在 aarch64 上正常工作。

## 待验证

| 项目 | 状态 | 说明 |
|------|------|------|
| /proc/self/exe | 待验证 | proc-self-exe-test 已安装但未自动运行，需手动验证 |
| riscv64 glibc | 未测试 | 需要 riscv64 glibc 交叉编译工具链 |
| x86_64 glibc | 未测试 | 需要 x86_64 glibc 交叉编译工具链 |

## 与 dynamic musl 的对比

| 项目 | dynamic musl | debian glibc |
|------|-------------|--------------|
| aarch64 | PASS | PASS |
| riscv64 | PASS | 未测试 |
| x86_64 | PASS | 未测试 |
| interpreter | `/lib/ld-musl-*.so.1` | `/lib/ld-linux-aarch64.so.1` |
| libc | `libc.musl-*.so.1` | `libc.so.6` |
| procfs 依赖 | 无 | 强依赖 /proc/self/exe（待验证） |
