# 默认未执行的已知失败探针

以下程序仍构建，安装到`/usr/bin/starry-known-fail`；默认runner只执行`/usr/bin/starry-test-suit/*`，因此它们未参与默认CI功能执行。此处记录现行选择，不表示已经在当前提交重新复现失败。

| 项目 | 程序 | 原保护内容 |
| --- | --- | --- |
| perf/syscall-test-ebpf-attach | test-ebpf-attach | perf kprobe与BPF attach/link |
| fs/syscall-test-io-getevents | test-io-getevents | Linux AIO负nr参数的errno顺序 |
| fs/syscall-test-io-submit | test-io-submit | Linux AIO负nr参数的errno顺序 |
| process/syscall-test-sigtimedwait | test-sigtimedwait | 排队信号及siginfo |
| devices/test-ioctl | test-ioctl | termios ioctl修改 |

旧表列出的`test-mt-execve`、`test-ptrace-exec-stop`、`test-sigqueueinfo`已经安装到默认目录，现不再列为未执行。

启用或删除前核对当前真实结果和替代覆盖，单独记录决定；不能把仅编译视为执行成功。
