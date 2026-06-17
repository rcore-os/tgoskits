# StarryOS 信号扩展 syscall 测试记录

本文记录本轮只改测试和文档的结果。范围固定为 6 个 syscall：`sigaltstack`、`rt_sigtimedwait`、`rt_sigsuspend`、`rt_sigqueueinfo`、`rt_tgsigqueueinfo`、`rt_sigreturn`。本轮没有修改 `os/StarryOS` 或 `components/starry-signal` 的实现源码。

## 参考环境

- 日期：2026-05-20
- 本地参考：宿主 Linux x86_64
- StarryOS 入口验证命令：`cargo xtask starry test qemu --arch x86_64 -g normal -c syscall`
- 注意：当前 shell 带有 `LD_PRELOAD=...libproxychains4.so`，运行 QEMU user 时会引入额外动态链接风险；本轮复测 StarryOS grouped case 时使用了 `env -u LD_PRELOAD`。

## 优先级和结论

| 优先级 | syscall | 本轮结论 | 下一步 |
|--------|---------|----------|--------|
| 1 | `rt_sigsuspend` | 已补独立 C 测试，Linux 参考通过 | 等 `test-raw-msg-peek` prebuild 阻塞解除后跑 StarryOS |
| 2 | `rt_sigqueueinfo` | 已补进程定向 `siginfo_t` 保真测试，Linux 参考通过 | 等 grouped syscall case 可运行后验证 StarryOS |
| 3 | `rt_tgsigqueueinfo` | 已补线程定向投递测试，Linux 参考通过 | 等 grouped syscall case 可运行后验证 StarryOS |
| 4 | `rt_sigtimedwait` | 已补 timeout、`info == NULL`、`siginfo_t` 回写测试，Linux 参考通过 | 等 grouped syscall case 可运行后验证 StarryOS |
| 5 | `rt_sigreturn` | 已补 handler 返回后 mask 恢复测试，Linux 参考通过 | 等 grouped syscall case 可运行后验证 StarryOS |
| 6 | `sigaltstack` | 已有测试覆盖备用栈基本语义，本轮保留并登记 | 后续可单独切出更细粒度 case |

## 实现和覆盖状态

| syscall | 分发入口 | 实现入口 | 用户态测试 | 当前分类 |
|---------|----------|----------|------------|----------|
| `sigaltstack` | `os/StarryOS/kernel/src/syscall/mod.rs` | `os/StarryOS/kernel/src/syscall/signal.rs::sys_sigaltstack` | `test-sigaltstack` | 已有覆盖，StarryOS 本轮验证被前置 case 阻塞 |
| `rt_sigtimedwait` | `mod.rs` | `signal.rs::sys_rt_sigtimedwait` | `test-sigtimedwait` | 新增覆盖，StarryOS 本轮验证被前置 case 阻塞 |
| `rt_sigsuspend` | `mod.rs` | `signal.rs::sys_rt_sigsuspend` | `test-sigsuspend` | 新增覆盖，StarryOS 本轮验证被前置 case 阻塞 |
| `rt_sigqueueinfo` | `mod.rs` | `signal.rs::sys_rt_sigqueueinfo` | `test-sigqueueinfo` | 新增覆盖，StarryOS 本轮验证被前置 case 阻塞 |
| `rt_tgsigqueueinfo` | `mod.rs` | `signal.rs::sys_rt_tgsigqueueinfo` | `test-tgsigqueueinfo` | 新增覆盖，StarryOS 本轮验证被前置 case 阻塞 |
| `rt_sigreturn` | `mod.rs` | `signal.rs::sys_rt_sigreturn` | `test-sigreturn` | 新增覆盖，StarryOS 本轮验证被前置 case 阻塞 |

## 新增或登记的测试

| 测试命令 | 相关 syscall | 测试重点 |
|----------|--------------|----------|
| `/usr/bin/test-sigaltstack` | `sigaltstack` | 查询、设置、禁用、禁用时 `old_ss` 回填、非法参数、`SA_ONSTACK` 相关备用栈行为 |
| `/usr/bin/test-sigsuspend` | `rt_sigsuspend` | 临时 mask 替换、handler 唤醒、返回 `-1/EINTR`、旧 mask 完整恢复、唤醒信号不残留 pending |
| `/usr/bin/test-sigqueueinfo` | `rt_sigqueueinfo` | 进程定向排队、`siginfo_t` 的 `si_code`/`si_pid`/`si_uid`/value 保真、非法 signo、已退出 pid |
| `/usr/bin/test-tgsigqueueinfo` | `rt_tgsigqueueinfo` | 线程定向投递、tgid/tid 校验、非法 signo、目标线程收到完整 `siginfo_t` |
| `/usr/bin/test-sigtimedwait` | `rt_sigtimedwait` | timeout、非法 timeout、pending signal 消费、`info == NULL`、`siginfo_t` 回写、消费后不重复返回 |
| `/usr/bin/test-sigreturn` | `rt_sigreturn` | handler 返回用户态后恢复当前信号和 action mask 对应的 signal mask |

这些命令已登记到以下 grouped syscall 配置：

- `test-suit/starryos/qemu-smp1/system/qemu-x86_64.toml`
- `test-suit/starryos/qemu-smp1/system/qemu-riscv64.toml`
- `test-suit/starryos/qemu-smp1/system/qemu-aarch64.toml`
- `test-suit/starryos/qemu-smp1/system/qemu-loongarch64.toml`

## 验证结果

| 环境 | 命令 | 结果 |
|------|------|------|
| C 编译 | `gcc -std=c11 -Wall -Wextra -Werror <test>/c/src/main.c -pthread -o /tmp/<test>` | 5 个新增测试全部通过 |
| Linux 参考 | `env -u LD_PRELOAD /tmp/test-sigaltstack-cmake/test-sigaltstack` | 42 passed, 0 failed |
| Linux 参考 | `env -u LD_PRELOAD /tmp/test-sigsuspend` | 13 passed, 0 failed |
| Linux 参考 | `env -u LD_PRELOAD /tmp/test-sigqueueinfo` | 8 passed, 0 failed |
| Linux 参考 | `env -u LD_PRELOAD /tmp/test-tgsigqueueinfo` | 14 passed, 0 failed |
| Linux 参考 | `env -u LD_PRELOAD /tmp/test-sigtimedwait` | 10 passed, 0 failed |
| Linux 参考 | `env -u LD_PRELOAD /tmp/test-sigreturn` | 9 passed, 0 failed |
| CMake | `cmake -S <test>/c -B /tmp/<test>-cmake && cmake --build /tmp/<test>-cmake` | 6 个 signal 测试全部通过 |
| StarryOS 发现 | `cargo xtask starry test qemu -l --arch x86_64` | 通过，能发现 `qemu-smp1/system` |
| StarryOS grouped syscall | `env -u LD_PRELOAD cargo xtask starry test qemu --arch x86_64 -c qemu-smp1/system` | 失败于既有 `test-raw-msg-peek/c/prebuild.sh`，未执行到 signal 测试 |

StarryOS grouped case 的具体阻塞点：

```text
test-suit/starryos/qemu-smp1/system/syscall-test-raw-msg-peek/c/prebuild.sh: line 4: apk: Symbolic link loop
failed to run test-raw-msg-peek prebuild.sh
```

`test-raw-msg-peek` 的第 4 行是 `apk add binutils gcc musl-dev`。xtask 会在 staging rootfs 内通过 qemu-user 和 busybox shell 运行 prebuild 脚本，并把 `target/.../guest-bin/apk` wrapper 放到 `PATH` 前面。当前环境下 guest shell 执行这个 wrapper 时 `execve()` 返回 `ELOOP`，所以 grouped case 在前置预构建阶段中断。

## 学习笔记

本轮未拆分独立学习笔记，以下 syscall 的语义和验证结果已在本文汇总。

| syscall | 记录 |
|---------|------|
| `sigaltstack` | 本文汇总 |
| `rt_sigsuspend` | 本文汇总 |
| `rt_sigqueueinfo` | 本文汇总 |
| `rt_tgsigqueueinfo` | 本文汇总 |
| `rt_sigtimedwait` | 本文汇总 |
| `rt_sigreturn` | 本文汇总 |

## 后续建议

1. 先修复或绕过 `test-raw-msg-peek` 的 `apk` wrapper / qemu-user prebuild 阻塞。
2. 阻塞解除后，重新运行 `env -u LD_PRELOAD cargo xtask starry test qemu --arch x86_64 -g normal -c syscall`。
3. 如果 signal 测试在 StarryOS 中失败，再按单个 syscall 记录 expected-versus-observed，并只修对应最小实现缺口。
