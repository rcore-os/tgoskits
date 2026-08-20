# wakeup-latency-bench

这个显式运行的重基准使用同一份用户态 C 源码，对比 StarryOS、当前 `dev` 和
Linux v7.1 PREEMPT_RT 的任务唤醒延迟分布。它不把完整 QEMU 总时长或一轮
ping-pong 总耗时当作单次唤醒延迟。

## 测量对象

默认分别在 `SCHED_OTHER` 和 `SCHED_FIFO:80` 下运行四类场景：

- `thread_futex_same_cpu`：同一 CPU 上两个线程的 futex handoff；
- `thread_futex_cross_cpu`：CPU 0 唤醒 CPU 1 上已经 park 的线程；
- `process_futex_cross_cpu`：跨进程、跨 CPU 的共享 futex 唤醒；
- `absolute_timer_same_cpu`：基于 `CLOCK_MONOTONIC` 绝对期限的周期 timer 唤醒。

futex 场景只把 `FUTEX_WAIT` 确实返回“已被唤醒”的样本计入分布。若 producer
在 waiter 进入内核前更新 futex，`EAGAIN` 样本会记入 `not_parked`，不会伪装成
极低延迟。跨 CPU producer 在计时区间外留出 50 us park 窗口；同 CPU 场景改为
显式 `sched_yield()`，让已发布 armed 的 receiver 获得一次完整调度机会并进入
`FUTEX_WAIT`。这样同核 wake 调用链不会被每轮无关的 task deadline 污染。timer
每次执行 `deadline += period`，因此不会把相对 sleep 漂移混入调度延迟。

每个场景输出一行 `WAKEUP_LATENCY_RESULT` JSON，包含：

- `min/mean/stddev/p50/p95/p99/p99.9/max`；
- 实际样本、尝试次数和未 park 次数；
- timer missed deadlines；
- 固定纳秒区间直方图。

默认预热 1,000 次，futex 测量 20,000 次，timer 测量 10,000 次、周期 1 ms。
`clock_resolution_ns` 与连续两次 raw `SYS_clock_gettime` 的最小开销单独报告，
不从结果中猜测性扣除。Linux 与 StarryOS 都强制走同一个 syscall ABI，不能让 Linux
的 vDSO 快路径把用户态取时成本伪装成调度收益；metadata 中的
`clock_read=raw_syscall` 用来确认这条测量边界。

## 运行

StarryOS：

```bash
cargo xtask starry app qemu -t wakeup-latency-bench --arch x86_64
```

单场景剖析直接调用二进制；`--policy` 和 `--case` 都是显式选择器，不接受隐式
别名：

```bash
/usr/bin/wakeup-latency-bench \
  --policy other \
  --case thread_futex_same_cpu
```

每个场景分别在策略和亲和性配置完成后、正式 workload 前输出
`WAKEUP_LATENCY_CASE_START`，并在 workload 返回后、排序和报告前输出
`WAKEUP_LATENCY_CASE_DONE`，后跟 `case=<name> policy=<policy>`。单场景 qperf 运行可
用这两个 marker 收窄窗口。

完整运行只使用唯一的 `WAKEUP_LATENCY_PROFILE_START` 和
`WAKEUP_LATENCY_PROFILE_DONE`。前者位于 metadata、CPU 数量检查和调度策略能力探测
之后，后者位于最后一个场景报告之后。qperf monitor 只消费第一次 start/stop，因此
不能在完整运行中把多组 case marker 当作多个窗口。

Linux 对照必须从本目录的 `main.c`、`handoff.c`、`timer.c`、`stats.c` 构建，定义
`BENCH_INIT` 后静态链接为 initramfs 的 `/init`。三方保持以下 QEMU 参数一致：

```text
q35,accel=tcg
-cpu max
-smp 2
-m 512M
```

Linux 内核使用本地 `~/linux-src` 的 v7.1 提交 `8cd9520d35a6`，并确认
`CONFIG_PREEMPT_RT=y`。若宿主不允许 `SCHED_FIFO`，基准输出明确的
`WAKEUP_LATENCY_SKIP`，不得把普通策略结果标成 RT 结果。

## 解释边界

QEMU TCG 的虚拟时钟还包含宿主线程调度与翻译开销，不能解释为硬件实时上界。
严格横比只接受同一宿主、同一 QEMU 参数、同一源码、同一 workload 的成对结果。
跨裸机或不同加速模式的数据只用于趋势判断。

`cyclictest`/Linux `timerlat` 测的是 timer deadline 到 RT thread 运行的延迟；本基准
的 `absolute_timer_same_cpu` 与它同类。futex 场景测显式任务 handoff，不应被称为
timer latency。吞吐量、上下文切换次数和 qperf 调用栈是另一组指标，不能替代这里的
尾延迟分布。

该工具不设置与机器绑定的 CI 性能阈值。成功标志只证明测量协议、亲和性、策略切换、
线程/进程生命周期和输出完整；性能回退由同机 A/B 数据判定。
