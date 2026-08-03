# futex-ping-pong-bench

这个显式运行的重基准用于比较 StarryOS 与 Linux v7.1 PREEMPT_RT 的跨 CPU
任务唤醒开销。主线程固定在 CPU 0，工作线程固定在 CPU 1；每次往返包含两次
`FUTEX_WAKE_PRIVATE` 唤醒，结果报告单向 handoff 的 7 轮中位数。

运行 StarryOS：

```bash
cargo xtask starry app qemu -t futex-ping-pong-bench --arch x86_64
```

Linux 对照必须使用同一份 `futex-ping-pong-bench.c`，定义 `BENCH_INIT` 后静态
链接为 initramfs 的 `/init`。两边必须保持 `q35,accel=tcg`、`-cpu max`、2 个
vCPU 和 512 MiB 内存一致。QEMU TCG 的结果只适合比较同一宿主机上的完整
guest 调用路径，不等价于硬件周期，也不能单独证明实时延迟上界。

该目录记录的是性能测量工具，不设宽松或机器相关的 CI 阈值。成功条件只校验
亲和性、futex 往返和线程生命周期正确完成；性能回归由同机成对结果判定。
