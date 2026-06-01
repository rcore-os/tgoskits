# Starry MariaDB 应用

这个用例通过 StarryOS 的 app runner 运行 MariaDB 冒烟测试。

```bash
cargo xtask starry app run -t mariadb --arch x86_64
cargo xtask starry app run -t mariadb --arch aarch64
cargo xtask starry app run -t mariadb --arch riscv64
cargo xtask starry app run -t mariadb --arch loongarch64
```

Guest 内的测试会安装 `mariadb` 和 `mariadb-client`，初始化一份全新的数据目录，
通过 Unix socket 启动 `mariadbd`，先验证 `SELECT 1`，然后在 `starry_test`
数据库上执行一组更完整的 SQL 工作负载。该工作负载覆盖 InnoDB 建表、多行插入、
过滤、排序、聚合、连接查询、更新、删除、提交、回滚、二级索引、临时表、视图、
表结构检查和最终统计。随后测试会重启服务，并检查持久化的行数据和视图结果仍然存在。

如果 MariaDB 日志中出现 Starry direct I/O 修复所覆盖的 InnoDB I/O 错误模式，
脚本也会判定用例失败：

- 短读，例如 `bytes should have been read`
- 写入失败，例如 `InnoDB: IO Error`

在注入测试脚本之前，`prebuild.sh` 会从缓存的干净 Alpine 归档刷新该应用专用的
rootfs。这样每次 MariaDB app 运行都不会依赖之前用过的、或者已经被部分填满的
rootfs 镜像。
