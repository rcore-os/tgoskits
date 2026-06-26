# Starry IPC Test

`ipc` 是一个 StarryOS 应用级 IPC 能力展示用例。它使用 Rust 编写，通过 `libc` crate 直接调用 Starry 提供的 Linux/POSIX 风格 IPC syscall，启动一个父进程和一个子进程，通过多种 IPC 机制传递可识别 payload，并在日志中打印每条通道的收发结果。

## 覆盖能力

- `pipe`: 父进程向子进程发送字符串，子进程再通过另一条 pipe 回传确认。
- `AF_UNIX socketpair`: 父子进程通过 Unix domain socket 双向通信。
- `SysV message queue`: 父进程发送带 `mtype` 的消息，子进程按类型接收。
- `MAP_SHARED | MAP_ANONYMOUS mmap`: 父子进程共享一页内存，记录各自观察到的 IPC 状态和 payload。

这些通道组合在一起，可以比较直观地证明 Starry 具备跨进程数据传递、双向通信、消息队列投递和共享内存协作能力。

## 运行方式

```bash
cargo xtask starry app qemu -t ipc --arch x86_64
```

测试源码位于 `src/main.rs`。程序会通过 Starry Rust app pipeline 交叉编译，并安装为 guest 内的 `/usr/bin/ipc-test`。`qemu-x86_64.toml` 通过 `shell_init_cmd` 自动运行它。

## 成功标记

关键日志包括：

```text
IPC_CHILD_PIPE_RX: parent->child over pipe
IPC_CHILD_SOCKET_RX: parent->child over socketpair
IPC_CHILD_MSGQUEUE_RX: parent->child over msgqueue
IPC_PARENT_PIPE_ACK: child ack over pipe
IPC_PARENT_SOCKET_ACK: child->parent over AF_UNIX socket
IPC_SUMMARY: pipe=ok socketpair=ok msgqueue=ok shared_mmap=ok
IPC TEST PASSED
```

QEMU 用例以 `IPC TEST PASSED` 作为 `success_regex`，并把 `FAIL |`、`IPC TEST FAILED` 和 kernel panic 作为失败信号。
