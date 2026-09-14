# memcached 应用验证

本应用从 PR #1321 的 `6b7547d09b74` 迁移而来，通过真实 memcached 服务验证
StarryOS 上的 TCP 文本协议、跨连接存储和删除行为。四个 `qemu-*.toml`
共享 `python/memcached-test.py`，由 `cargo xtask starry app qemu` 显式运行。

## 1. 协议验证

`memcached-test.py` 启动单工作线程服务，绑定 `127.0.0.1:11211`，关闭 UDP。
客户端每次请求建立新 TCP 连接，验证数据确实保存在服务端，并检查完整响应。

### 1.1 数据与统计

`check()` 先以有界连接重试等待服务就绪，再检查版本、存储确认、读取值及长度、
删除确认、删除后读取为空和重复删除返回 `NOT_FOUND`。`stats` 必须完整结束，
其 PID 必须属于本次子进程，写入次数、读取命中和未命中次数必须与操作一致。
`request()` 限制单次响应时间和大小，超时、截断、额外内容均不能算成功。

### 1.2 LTP 覆盖边界

核对 LTP `20260529` 的 `runtest/` 和 `testcases/` 后，未发现 memcached
用例。LTP 的网络与系统调用测试验证套接字等内核接口，没有启动 memcached，
也不验证其 set/get/delete/stats 协议，不能替代本应用的完整行为。
上游源码见 [LTP 20260529](https://github.com/linux-test-project/ltp/tree/20260529)。

原 PR 中独立的 `nc` 回环探针已移除：这里每一步协议交互都经过真实 TCP
回环，已经包含连通性检查。应用不再单独维护基础网络探针，也不增加重复的
LTP 系统调用用例；现有入口仍是 `test-suit/starryos/qemu/system/ltp-syscalls`。

## 2. QEMU 运行

应用保留原 PR 的四种体系结构及其 CPU、内存配置，只启用
`ax-driver/nvme` 和 `ax-driver/virtio-net`，移除无关的图形、输入、USB
外设及附加 BusyBox 镜像依赖。运行时使用逐用例根文件系统副本，
安装依赖和测试写入不会持久修改共享镜像。

x86_64 和 LoongArch 对齐当前 system 套件，使用 `uefi = true`、
`to_bin = true`，由运行器准备 OVMF 和固件系统分区。AArch64 与 RISC-V
使用直接二进制启动。旧 x86_64 ELF 直接加载会因缺少 PVH note 而失败。

### 2.1 执行入口

Python 文件流水线在准备阶段安装 `python3` 并注入客户端；客户机通过
`apk add memcached python3` 安装服务并保留客户端依赖，避免 apk 根据客户机
world 清单移除准备阶段注入的 Python。因此准备阶段和客户机均需要可用的软件源。
选择需要验证的体系结构执行，四种取值为 `x86_64`、`aarch64`、`riscv64`
和 `loongarch64`。

```bash
cargo xtask starry app qemu -t memcached --arch x86_64
```

### 2.2 结果与清理

`main()` 在成功和异常路径都回收自己启动的子进程，先发送终止信号，五秒内
未退出则强制终止并等待回收，不使用全局 `pkill`。日志只来自本次服务。
全部协议判定及回收完成后才输出 `MEMCACHED_TEST_PASSED`；异常输出
`MEMCACHED_TEST_FAILED` 并非零退出。`shell_check_steps` 同时捕获依赖安装
失败，由外层运行器判为失败。此用例不提供吞吐量或高并发压力指标。

## 3. 验证记录

2026-09-14，在 PR head `6b7547d09b74` 变基到 `c1f5be7373` 后，使用
`cargo xtask starry app qemu -t memcached --arch <arch>` 分别验证 x86_64、
AArch64、RISC-V 和 LoongArch。四个架构均运行 Alpine memcached 1.6.42，
输出 `MEMCACHED_TEST_PASSED`，项目命令退出码均为 0。

宿主 Linux 的真实 memcached 也通过同一客户端。临时改变期望的读取值后，
客户端非零退出、输出失败标记且不输出成功标记；恢复后再次通过，确认失败
路径释放了监听端口。QEMU 中缺失 Python 的失败运行同样被外层运行器拒绝。
