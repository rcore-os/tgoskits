# Starry MySQL 应用

这个应用会先准备一个带 Oracle MySQL 8.4.6 generic glibc 二进制包的 x86_64 Debian rootfs，然后在 StarryOS guest 中运行 SQL 测试。

当前只支持 x86_64 Debian/glibc rootfs。Oracle generic 包不适合 aarch64，也不适合 Alpine/musl rootfs。

## 宿主机权限要求

MySQL rootfs 准备流程必须以 `root` 身份运行，或者由具备 passwordless
`sudo` 的用户运行。`prebuild.sh` 会使用 `losetup` 挂载生成的 ext4
镜像，然后在镜像内安装 MySQL、解包 Debian 运行时依赖，并写入
`/root/mysql-env.sh`。如果没有这些权限，app 流程会在 QEMU 启动前停止。

请在 root shell、具备 root 权限的容器，或者能以 passwordless `sudo`
执行挂载流程的宿主机账号下运行：

```bash
cargo xtask starry app qemu -t mysql --arch x86_64
```

生成后的镜像会缓存在 `tmp/axbuild/rootfs/rootfs-x86_64-mysql.img`，下载内容会缓存在
`target/mysql`，但当前 `prebuild.sh` 在检查、扩容、挂载和刷新镜像时仍然需要
root 权限。

## Rootfs 准备

`prebuild.sh` 在宿主机或容器中、QEMU 启动前执行：

1. 使用 `wget --no-check-certificate` 准备 `tmp/axbuild/rootfs/rootfs-x86_64-debian.img.tar.xz`。
2. 从 Debian rootfs 压缩包解出 MySQL 专用 rootfs。
3. 将专用镜像扩容到 `5G`。
4. 使用 `wget --no-check-certificate` 下载 MySQL 8.4.6；如果已有 `MYSQL_TARBALL` 或 `mysql.tar.xz`，则直接复用。
5. 将 MySQL 安装到 `/opt/mysql`。
6. 解包运行时依赖：`libaio`、`libnuma`、`libncurses`。
7. 写入 `/root/mysql-env.sh`，配置 `PATH` 和 `LD_LIBRARY_PATH`。
8. 通过 app overlay 加入 `/usr/bin/mysql-test.sh`。

QEMU 使用生成后的 rootfs：

```text
tmp/axbuild/rootfs/rootfs-x86_64-mysql.img
```

## 交互模式

如果需要进入 MySQL 客户端手动执行 SQL，可以使用交互配置：

```bash
cargo xtask starry app qemu -t mysql --arch x86_64 \
  --qemu-config qemu-x86_64-interactive.toml
```

进入 guest 后会自动运行：

```sh
/usr/bin/mysql-interactive.sh
```

这个脚本会在 `/opt/mysql/data` 尚未初始化时执行初始化，随后后台启动 `mysqld`，等待 socket 可连接，最后进入 MySQL 交互客户端。退出 MySQL 客户端使用 `exit`，退出 QEMU 使用 `Ctrl-a x`。

## Guest 测试流程

`mysql-test.sh` 会在 StarryOS 内自动运行：

1. 后台初始化 `/opt/mysql/data`。
2. 先睡眠 30 秒，然后检查 `/tmp/mysql-init.log`，直到出现 `Bootstrapping complete`。
3. 使用普通 `kill` 停掉初始化进程，然后睡眠 3 秒。
4. 后台启动 `mysqld`，socket 使用 `/tmp/mysql.sock`。
5. 先睡眠 30 秒，然后等待 `/tmp/mysql.sock` 和 `/opt/mysql/data/mysqld.pid` 出现。
6. 执行 15 个 SQL 阶段，并输出带颜色的 `MYSQL_STAGE_PASSED`。
7. 最后一组持久化测试前使用非优雅退出重启 `mysqld`，避免触发当前已知的 shutdown 卡死路径。

测试刻意避开 `mysqladmin shutdown`，因为当前 MySQL 优雅关闭路径可能导致 guest 卡死。

## 覆盖范围

15 个 SQL 阶段覆盖：

- 版本与服务器元数据
- 数据库和 schema 创建
- 带约束的 InnoDB 表
- 多行插入和排序查询
- 更新操作
- 二级索引和 `EXPLAIN`
- 表连接
- 聚合查询
- `COMMIT` 和 `ROLLBACK` 事务
- 临时表
- 视图和 information_schema 查询
- 重启后的持久化检查

## 配置

- Guest 内存：`2G`
- StarryOS 物理内存：`0x8000_0000`
- Rootfs 目标大小：`5G`
- MySQL 包缓存：`target/mysql`
- 成功标记：`MYSQL_TEST_PASSED`
