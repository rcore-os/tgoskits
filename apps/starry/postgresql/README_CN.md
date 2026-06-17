# Starry PostgreSQL 应用

这个用例通过 StarryOS 的 app runner 运行 PostgreSQL 冒烟测试。

## 自动化测试

```bash
cargo xtask starry app qemu -t postgresql --arch riscv64
cargo xtask starry app qemu -t postgresql --arch aarch64
cargo xtask starry app qemu -t postgresql --arch x86_64
cargo xtask starry app qemu -t postgresql --arch loongarch64
```

Guest 内的测试安装 `postgresql17`，通过 `initdb` 初始化全新的数据库集群，
通过 Unix socket 启动 `postgres`，先验证 `SELECT 1`，然后在 `starry_test`
数据库上执行一组结构化的 SQL 工作负载。覆盖内容：

- DDL：带外键和索引的建表
- DML：多行插入、更新、删除
- 查询：过滤、排序、聚合、连接查询
- 事务：提交和回滚
- 批量插入：通过 `generate_series`
- 数据完整性校验（行数、更新持久化、回滚正确性）

## 手动复现（不使用测试脚本）

以下步骤让你在 StarryOS 中交互式运行 PostgreSQL，用于演示或调试。
所有命令在 StarryOS shell 提示符（`root@starry:`）下输入。

### 准备工作

构建并启动带网络的 StarryOS：

```bash
# 准备 rootfs（仅需执行一次）
cargo xtask starry rootfs --arch riscv64

# 构建并运行
cargo xtask starry app qemu -t postgresql --arch riscv64
```

等待 `root@starry:` 提示符出现。

### 第一步：安装 PostgreSQL

```sh
apk update
apk add postgresql17
```

这会从 Alpine 软件仓库安装 PostgreSQL 17。服务端二进制文件安装到
`/usr/libexec/postgresql17/`，客户端工具（`psql`、`pg_ctl`、`initdb`）
安装在 `$PATH` 中。

设置一个便利变量指向二进制文件路径：

```sh
PG=/usr/libexec/postgresql17
```

### 第二步：创建 postgres 用户

PostgreSQL 不允许以 root 身份运行。创建一个专用的系统用户：

```sh
echo 'postgres:x:70:70:PostgreSQL:/var/lib/postgresql:/bin/sh' >> /etc/passwd
echo 'postgres:x:70:' >> /etc/group
echo 'postgres::20000:0:99999:7:::' >> /etc/shadow
```

### 第三步：初始化数据库集群

```sh
mkdir /tmp/pgdata
chmod 0700 /tmp/pgdata
chown postgres:postgres /tmp/pgdata
su -s /bin/sh postgres -c "$PG/initdb -D /tmp/pgdata --no-locale --username=postgres"
```

预期输出以以下内容结尾：
```
Success. You can now start the database server using:
    pg_ctl -D /tmp/pgdata -l logfile start
```

### 第四步：启动 PostgreSQL 服务

创建 Unix socket 目录并启动服务：

```sh
mkdir /tmp/pgrun
chmod 0700 /tmp/pgrun
chown postgres:postgres /tmp/pgrun
su -s /bin/sh postgres -c "$PG/pg_ctl -D /tmp/pgdata -l /tmp/pg.log start -w -t 120 \
  -o '-c max_connections=10 -c shared_buffers=16MB -c unix_socket_directories=/tmp/pgrun -c fsync=off'"
```

预期输出：
```
waiting for server to start.... done
server started
```

### 第五步：连接并执行查询

连接到服务器并尝试基本操作：

```sh
# 简单查询
su -s /bin/sh postgres -c "$PG/psql -h /tmp/pgrun -d postgres -c 'SELECT 1'"

# 创建测试数据库
su -s /bin/sh postgres -c "$PG/psql -h /tmp/pgrun -d postgres -c 'CREATE DATABASE mydb'"

# 创建表
su -s /bin/sh postgres -c "$PG/psql -h /tmp/pgrun -d mydb -c '
  CREATE TABLE users (id SERIAL PRIMARY KEY, name VARCHAR(64), age INT)'"

# 插入数据
su -s /bin/sh postgres -c "$PG/psql -h /tmp/pgrun -d mydb -c \"
  INSERT INTO users(name, age) VALUES ('Alice', 21), ('Bob', 22), ('Carol', 23)\""

# 查询数据
su -s /bin/sh postgres -c "$PG/psql -h /tmp/pgrun -d mydb -c 'SELECT * FROM users'"
```

SELECT 的预期输出：
```
 id | name  | age
----+-------+-----
  1 | Alice |  21
  2 | Bob   |  22
  3 | Carol |  23
(3 rows)
```

### 第六步：尝试更高级的 SQL

```sh
# 带聚合的连接查询
su -s /bin/sh postgres -c "$PG/psql -h /tmp/pgrun -d mydb -c '
  CREATE TABLE orders (
    id SERIAL PRIMARY KEY,
    user_id INT REFERENCES users(id),
    product VARCHAR(64),
    amount DECIMAL(10,2)
  )'"

su -s /bin/sh postgres -c "$PG/psql -h /tmp/pgrun -d mydb -c \"
  INSERT INTO orders(user_id, product, amount) VALUES
    (1, 'book', 39.90), (1, 'keyboard', 199.00), (2, 'mouse', 59.00)\""

su -s /bin/sh postgres -c "$PG/psql -h /tmp/pgrun -d mydb -c '
  SELECT u.name, COUNT(o.id) AS orders, SUM(o.amount) AS total
  FROM users u JOIN orders o ON u.id = o.user_id
  GROUP BY u.name ORDER BY total DESC'"
```

预期输出：
```
 name  | orders | total
-------+--------+--------
 Alice |      2 | 238.90
 Bob   |      1 |  59.00
(2 rows)
```

### 第七步：停止服务

```sh
su -s /bin/sh postgres -c "$PG/pg_ctl -D /tmp/pgdata stop"
```

预期输出：
```
waiting for server to shut down.... done
server stopped
```

### 第八步：验证持久化

重启服务并验证数据仍在：

```sh
su -s /bin/sh postgres -c "$PG/pg_ctl -D /tmp/pgdata -l /tmp/pg.log start -w -t 120 \
  -o '-c max_connections=10 -c shared_buffers=16MB -c unix_socket_directories=/tmp/pgrun -c fsync=off'"
su -s /bin/sh postgres -c "$PG/psql -h /tmp/pgrun -d mydb -c 'SELECT * FROM users'"
su -s /bin/sh postgres -c "$PG/pg_ctl -D /tmp/pgdata stop"
```

users 表仍应包含全部三行数据。

### 清理

```sh
rm -rf /tmp/pgdata /tmp/pg.log
```

## 交互式 psql 会话

使用以下命令进入交互式 SQL 会话：

```sh
su -s /bin/sh postgres -c "$PG/psql -h /tmp/pgrun -d postgres"
```

这会进入 psql REPL，你可以直接输入 SQL：

```
postgres=# SELECT version();
postgres=# CREATE TABLE t(id int);
postgres=# INSERT INTO t VALUES (1),(2),(3);
postgres=# SELECT * FROM t;
postgres=# \q
```

## 架构说明

| 架构 | 状态 | 备注 |
|------|------|------|
| riscv64 | 可用 | 已测试，1G 内存，30 分钟超时 |
| aarch64 | 可用 | 使用 cortex-a53，GICv2 |
| x86_64 | 可用 | 已测试，qemu64 CPU |
| loongarch64 | 可用 | 使用 la464 CPU，30 分钟超时 |

## 需要的内核支持

PostgreSQL 需要以下内核特性（均已包含在 tgoskits `dev` 中）：

- 进程凭证子系统（setuid、setresuid、getuid 等 13 个系统调用）
- `SA_RESTART` 系统调用重启（用于被信号中断的系统调用）
- DTB 物理内存发现（需要约 300MB+ 内存）
- `RLIMIT_STACK` 默认值设为 8MB（Linux 默认值）
- `fsync`/`fdatasync` 目录支持
- `sync_file_range` 存根
- `prctl` `PR_SET_PDEATHSIG` 支持
- `epoll_pwait` sigsetsize 兼容（musl 的 16 字节 `sigset_t`）
- Unix domain socket（用于初始连接握手）

## 已知限制

- `initdb` 在 QEMU 中较慢（riscv64 TCG 下约 40 秒），受模拟开销影响
- 不支持 SSL/TLS（编译时使用 `--without-openssl`）
- `pgbench` 压力测试以单独测试用例形式提供
- 文件所有权（uid/gid）自 PR #1097 起正确存储
