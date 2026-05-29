# Starry PostgreSQL 应用

这个用例通过 StarryOS 的 app runner 运行 PostgreSQL 冒烟测试。

```bash
cargo xtask starry app run -t postgresql --arch x86_64
cargo xtask starry app run -t postgresql --arch aarch64
cargo xtask starry app run -t postgresql --arch riscv64
cargo xtask starry app run -t postgresql --arch loongarch64
```

Guest 内的测试使用 `prebuild.sh` 注入的预编译 PostgreSQL 16.4 二进制文件。
测试通过 `initdb` 初始化全新的数据库集群，在 5433 端口通过 TCP 启动
`postgres`，先验证 `SELECT 1`，然后在 `starry_test` 数据库上执行一组
结构化的 SQL 工作负载。覆盖内容：

- DDL: 带外键和索引的建表
- DML: 多行插入、更新、删除
- 查询: 过滤、排序、聚合、连接查询
- 事务: 提交和回滚
- 批量插入: 通过 `generate_series`
- 持久化: 服务重启和数据校验

在注入测试脚本之前，`prebuild.sh` 会从缓存的干净 Alpine 归档刷新该应用
专用的 rootfs，用 musl 为目标架构交叉编译 PostgreSQL 16.4，并将二进制文件
及其运行时库依赖复制到 rootfs overlay 中。

## 前置条件

`prebuild.sh` 需要目标架构的 musl 交叉编译器：

| 架构 | 交叉编译器 |
|------|----------|
| riscv64 | `riscv64-linux-musl-gcc` |
| aarch64 | `aarch64-linux-musl-gcc` |
| x86_64 | `x86_64-linux-musl-gcc` |
| loongarch64 | `loongarch64-linux-musl-gcc` |

macOS 上可通过 [musl-cross-make](https://github.com/richfelker/musl-cross-make) 安装，
Linux 上使用发行版自带的交叉编译器包。

## 需要的内核补丁

运行 PostgreSQL 需要以下内核改动（均已合入 tgoskits `dev`）：

- 进程凭证子系统（setuid/setresuid/getuid 等 13 个系统调用）
- SA_RESTART 系统调用重启
- DTB 物理内存发现（PostgreSQL 需要约 300MB+ 内存）
- RLIMIT_STACK 默认值修正（512K → 8MB，匹配 Linux 默认值）
- fsync/fdatasync 目录支持
- sync_file_range 存根
- prctl PR_SET_PDEATHSIG 支持
- epoll_pwait sigsetsize 兼容（musl 的 16 字节 sigset_t）

## 已知限制

- `initdb` 较慢（riscv64 QEMU 上约 40 秒），为 QEMU 模拟开销
- 需要动态链接 + `--export-dynamic` 以支持扩展模块 `dlopen`
- `pgbench` TPC-B 基线测试：待 VFS 层支持 per-file uid/gid 存储后执行
