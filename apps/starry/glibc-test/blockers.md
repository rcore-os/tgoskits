# glibc-test Blocker 清单

> 最后更新：2026-05-30

## 当前状态

**三架构 glibc 动态链接：全部 PASS**

| 架构 | rootfs | glibc-test | proc-self-exe | pthread | regex |
|------|--------|------------|---------------|---------|-------|
| aarch64 | Alpine | PASS | PASS | PASS | PASS |
| aarch64 | Debian | PASS | PASS | PASS | PASS |
| riscv64 | Alpine | PASS | PASS | PASS | PASS |
| x86_64 | Alpine | PASS | PASS | PASS | PASS |

## Blocker 列表

无 blocker。glibc 动态链接、/proc/self/exe、pthread、regex 在三架构上均正常工作。

## 已验证

| 项目 | aarch64 | riscv64 | x86_64 |
|------|---------|---------|--------|
| glibc dynamic test | PASS | PASS | PASS |
| /proc/self/exe | PASS | PASS | PASS |
| pthread | PASS | PASS | PASS |
| regex | PASS | PASS | PASS |
| ld-linux | PASS | PASS | PASS |
| libc.so.6 | PASS | PASS | PASS |

## 待验证

| 项目 | 状态 | 说明 |
|------|------|------|
| dlopen/dlsym | 未测试 | 需要动态库文件 |
| C++ exception | 未测试 | 需要 C++ 编译器 |
| locale | 未测试 | 需要 locale 数据 |
| Debian rootfs | **PASS** | debootstrap 构建成功，glibc-test 全部通过 |

## 与 dynamic musl 的对比

| 项目 | dynamic musl | debian glibc |
|------|-------------|--------------|
| aarch64 | PASS | PASS |
| riscv64 | PASS | PASS |
| x86_64 | PASS | PASS |
| interpreter | `/lib/ld-musl-*.so.1` | `/lib/ld-linux-*.so.*` |
| libc | `libc.musl-*.so.1` | `libc.so.6` |
| procfs 依赖 | 无 | /proc/self/exe PASS |
| pthread | N/A | PASS |
| regex | N/A | PASS |
