# sysbench QEMU 功能验证

这个应用验证 StarryOS 能执行 sysbench 的 CPU、线程、互斥锁和内存负载。它通过 `prebuild.sh` 注入与板卡共用的 `harness/run.sh`，运行时使用项目配置的 Alpine 软件源安装 sysbench。

## 1. 功能运行

`qemu-aarch64.toml` 和 `qemu-aarch64-matrix.toml` 使用四核 AArch64、NVMe 根文件系统和用户网络。每个检查步骤必须完成全部选中负载，才能打印 `SYSBENCH_DONE`。

### 1.1 快速检查

默认配置运行一次短 CPU 负载，用于确认动态库和基本线程执行链路可用。

```bash
cargo xtask starry app qemu -t sysbench --arch aarch64
```

### 1.2 完整负载

矩阵配置执行 1、2、4 线程 CPU、线程同步、互斥锁和内存操作。`run.sh` 保留每项原始结果，并将命令失败传播到外层检查。

```bash
cargo xtask starry app qemu -t sysbench --arch aarch64 --qemu-config qemu-aarch64-matrix.toml
```

## 2. 性能解释

QEMU 数据包含模拟器、宿主调度和虚拟设备开销，只用于功能验证。真实 Linux/StarryOS 对比使用 [板卡应用](../sysbench-board/README.md)；其 CMake 会话工具包保证两边使用相同的 sysbench 与动态库。
