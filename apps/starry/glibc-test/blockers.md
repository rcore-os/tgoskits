# glibc-test Blocker 清单

> 最后更新：2026-05-30

## 当前状态

**aarch64 glibc 动态链接：PASS**

```
glibc dynamic test OK
GLIBC_TEST_DONE RC=0
/proc/self/exe -> /usr/bin/proc-self-exe-test
PROC_SELF_EXE_TEST_DONE RC=0
```

## Blocker 列表

无 blocker。glibc 基础动态链接和 /proc/self/exe 在 aarch64 上正常工作。

## 已验证

| 项目 | 状态 | 说明 |
|------|------|------|
| glibc dynamic test | PASS | 最小 printf 程序正常运行 |
| /proc/self/exe | PASS | readlink 正常返回可执行文件路径 |
| ld-linux-aarch64.so.1 | PASS | glibc dynamic linker 正常加载 |
| libc.so.6 | PASS | glibc C library 正常链接 |

## 待验证

| 项目 | 状态 | 说明 |
|------|------|------|
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
| procfs 依赖 | 无 | /proc/self/exe PASS |
