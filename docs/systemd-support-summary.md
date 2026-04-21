# StarryOS systemd 启动支持 — 修改总结

## 一、最终效果

`cargo starry qemu --arch aarch64` 启动后，systemd 257.9 成功引导至 `multi-user.target`，通过 `console-getty.service` 启动 bash shell，提示符 `starry:~#`，可以正常执行命令。

---

## 二、修复的 Bug 及解决方案

### Bug 1: SIGCHLD 信号丢失，signalfd 收不到信号

**现象**: systemd 通过 signalfd 监听 SIGCHLD，但子进程退出后 signalfd 始终无信号，导致 systemd 卡住无法回收子进程。

**根因**: `signal_ignored()` 把 `SignalDisposition::Default` + 默认动作是 Ignore 的信号（如 SIGCHLD）当作"已忽略"直接丢弃，不入 pending 队列。signalfd 只从 pending 队列读取，所以永远读不到。

**修复** (`components/starry-signal/src/api/process.rs`):
```rust
// 修复前: Default + DefaultAction::Ignore 也返回 true → 信号被丢弃
// 修复后: 只有显式设置 SIG_IGN 才返回 true
pub fn signal_ignored(&self, signo: Signo) -> bool {
    matches!(
        &self.actions.lock()[signo].disposition,
        SignalDisposition::Ignore  // 只检查显式 SIG_IGN
    )
}
```

---

### Bug 2: starry-signal 本地修改未编译生效

**现象**: 修改了本地 `components/starry-signal/` 代码但编译时仍使用 crates.io 版本。

**根因**: `os/StarryOS/kernel/Cargo.toml` 依赖 `starry-signal = "0.5.7"`，Cargo 从 crates.io 拉取。本地版本是 0.5.0，版本不匹配。

**修复**:
1. `components/starry-signal/Cargo.toml`: `version` 从 `"0.5.0"` 改为 `"0.5.7"`
2. `os/StarryOS/Cargo.toml`: 添加 `[patch.crates-io]` 指向本地路径
```toml
[patch.crates-io]
starry-signal = { path = "../../components/starry-signal" }
```

---

### Bug 3: `with_blocked_signals` 信号掩码在错误路径未恢复

**现象**: signalfd 的 `ppoll` 调用中，信号掩码在 EINTR 时未恢复为原始值。

**根因**: 使用 `.inspect()` 处理结果，它只在 `Ok` 时执行，`Err` 路径（EINTR）跳过了掩码恢复。

**修复** (`kernel/src/task/signal.rs`):
```rust
// 修复前: 用 .inspect() 只在 Ok 时恢复
// 修复后: 无论成功失败都恢复
pub fn with_blocked_signals<R>(
    blocked: Option<SignalSet>,
    f: impl FnOnce() -> AxResult<R>,
) -> AxResult<R> {
    let old_blocked = blocked.map(|set| sig.set_blocked(set));
    let result = f();
    if let Some(old) = old_blocked {
        sig.set_blocked(old);  // 无论 result 是否成功都恢复
    }
    result
}
```

---

### Bug 4: `-.mount` 报错 "Failed to drain libmount events"

**现象**: systemd 启动 `-.mount` 服务时失败，libmount 库报错。

**根因** (两个):
1. `fsopen()` 返回了假 fd，libmount 误认为内核支持 new mount API
2. 缺少 `/proc/self/mountinfo`，libmount 无法验证挂载操作

**修复**:

a) `kernel/src/syscall/mod.rs` — 让所有 new mount API 返回 ENOSYS:
```rust
Sysno::fsopen | Sysno::fsconfig | Sysno::fsmount | Sysno::fspick
| Sysno::open_tree | Sysno::move_mount | Sysno::mount_setattr
    => Err(AxError::Unsupported),
```

b) `kernel/src/pseudofs/proc.rs` — 添加静态 mountinfo 文件:
```rust
"mountinfo" => SimpleFile::new_regular(fs, move || {
    Ok(String::from(
        "1 0 254:0 / / rw,relatime - ext4 /dev/vda rw\n\
         2 1 0:3 / /proc rw,nosuid,nodev,noexec,relatime - proc proc rw\n\
         ...",
    ))
})
```

---

### Bug 5: agetty 串口波特率检测超时退出

**现象**: `console-getty.service` 启动 agetty 后，agetty 检测到 ttyAMA0 是串口，进入波特率探测循环，10 秒后超时退出（exit code 1）。`-L` 标志只跳过载波检测，不跳过波特率探测。

**修复** (`scripts/build-debian-rootfs.sh` + rootfs):
- 用 `/bin/bash --login` 替代 agetty，绕过整个串口检测
- 屏蔽 `serial-getty@ttyAMA0.service` 防止 systemd 自动检测后启动 agetty
- 移除不需要的 `serial-getty@ttyS0` 和 `getty@tty1`

```ini
# console-getty.service
[Service]
ExecStart=-/bin/bash --login
StandardInput=tty
StandardOutput=tty
StandardError=tty
TTYPath=/dev/console
```

---

### Bug 6 (早期修复): TIOCMGET 缺失

**现象**: 部分串口工具查询 modem 状态失败。

**修复** (`kernel/src/pseudofs/dev/tty/mod.rs`):
```rust
TIOCMGET => {
    const TIOCM_CAR: u32 = 0x40;  // carrier detect
    const TIOCM_CTS: u32 = 0x20;  // clear to send
    const TIOCM_DSR: u32 = 0x100; // data set ready
    (arg as *mut u32).vm_write(TIOCM_CAR | TIOCM_CTS | TIOCM_DSR)?;
}
```

---

## 三、配置文件修改

| 文件 | 修改内容 |
|------|---------|
| `os/StarryOS/starryos/.build-aarch64-unknown-none-softfloat.toml` | `log = "Error"` (避免 warn 日志拖慢 QEMU) |
| `os/StarryOS/starryos/src/main.rs` | `CMDLINE = &["/sbin/init"]` |
| `os/StarryOS/Cargo.toml` | 添加 `[patch.crates-io] starry-signal` |

---

## 四、Rootfs 修改

`scripts/build-debian-rootfs.sh --init systemd` 构建的 rootfs 包含：

- systemd 257.9 + bash (Debian trixie)
- `/sbin/init` → `/lib/systemd/systemd`
- 屏蔽不兼容的 systemd 服务（udevd、journald、networkd、logind、dbus 等 ~30 个）
- console-getty 使用 bash 而非 agetty
- serial-getty@ttyAMA0 被 mask
- 静态 `/sys` 目录结构防止 systemd crash
- root 密码: `root`

---

## 五、清理工作

删除了调试用的 `warn!` 日志：
- `kernel/src/file/signalfd.rs` — signalfd has_pending 调试日志
- `components/starry-signal/src/api/process.rs` — send_signal/dequeue_signal 调试日志
- `kernel/src/task/signal.rs` — Send signal 调试日志
- `kernel/src/syscall/task/exit.rs` — sys_exit/sys_exit_group 调试日志

仍存在但属于系统级日志（未清理）：`sys_ioctl`、`sys_mkdirat`、`sys_openat`、`sys_mount` 等。
