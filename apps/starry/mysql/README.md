# Starry MySQL App

This app prepares an x86_64 Debian rootfs with Oracle MySQL 8.4.6 generic glibc binaries, then runs a StarryOS guest-side SQL workload.

Only x86_64 Debian/glibc rootfs images are supported. The Oracle generic package is not suitable for aarch64 or Alpine/musl rootfs images.

## Host Privileges

MySQL rootfs preparation must run as `root`, or as a user with passwordless `sudo`.
The prebuild script attaches the generated ext4 image with `losetup` and mounts it
to install MySQL, unpack Debian runtime libraries, and write `/root/mysql-env.sh`.
Without those privileges, the app flow stops before QEMU starts.

Run the app from a root shell, a root-capable container, or a host account that
can run the required mount flow with passwordless `sudo`:

```bash
cargo xtask starry app qemu -t mysql --arch x86_64
```

The generated image is cached at `tmp/axbuild/rootfs/rootfs-x86_64-mysql.img`,
and downloads are cached under `target/mysql`, but the current prebuild still
needs root privileges when it verifies, resizes, mounts, and refreshes the image.

## Rootfs Preparation

`prebuild.sh` runs on the host/container before QEMU starts:

1. Prepares `tmp/axbuild/rootfs/rootfs-x86_64-debian.img.tar.xz` with `wget --no-check-certificate`.
2. Extracts the Debian rootfs archive into a dedicated MySQL rootfs image.
3. Expands the dedicated image to `5G`.
4. Downloads MySQL 8.4.6 with `wget --no-check-certificate`, unless `MYSQL_TARBALL` or `mysql.tar.xz` is already available.
5. Installs MySQL into `/opt/mysql`.
6. Unpacks runtime dependencies: `libaio`, `libnuma`, and `libncurses`.
7. Writes `/root/mysql-env.sh` with `PATH` and `LD_LIBRARY_PATH`.
8. Adds `/usr/bin/mysql-test.sh` through the app overlay.

The QEMU config uses the generated rootfs:

```text
tmp/axbuild/rootfs/rootfs-x86_64-mysql.img
```

## 指定测试配置

`qemu-x86_64-interactive.toml` 保留原文件名，现与默认入口一样执行完整 MySQL SQL 测例，不再自动进入手动交互客户端：

```bash
cargo xtask starry app qemu -t mysql --arch x86_64 \
  --qemu-config qemu-x86_64-interactive.toml
```

该配置通过 `shell_check_steps` 执行 `/usr/bin/mysql-test.sh`，匹配 `MYSQL_TEST_PASSED` 后退出；遇到 `MYSQL_TEST_FAILED` 或内核错误则失败，整体超时为 2400 秒。

## Guest Test Flow

`mysql-test.sh` runs automatically inside StarryOS:

1. Initializes `/opt/mysql/data` in the background.
2. Sleeps 30 seconds, then checks `/tmp/mysql-init.log` until `Bootstrapping complete` appears.
3. Stops the initialization process with plain `kill`, then sleeps 3 seconds.
4. Starts `mysqld` in the background with socket `/tmp/mysql.sock`.
5. Sleeps 30 seconds, then waits for `/tmp/mysql.sock` and `/opt/mysql/data/mysqld.pid`.
6. Runs 15 SQL stages with colored `MYSQL_STAGE_PASSED` output.
7. Restarts `mysqld` with a non-graceful exit before the final persistence stage to avoid the known shutdown hang path.

The test intentionally avoids `mysqladmin shutdown`, which currently can hang the guest during MySQL graceful shutdown.

## Coverage

The 15 SQL stages cover:

- version and server metadata
- database and schema creation
- InnoDB tables with constraints
- multi-row inserts and ordered queries
- updates
- secondary indexes and `EXPLAIN`
- joins
- aggregations
- transactions with `COMMIT` and `ROLLBACK`
- temporary tables
- views and information_schema queries
- restart persistence checks

## Configuration

- Guest memory: `2G`
- StarryOS physical memory: `0x8000_0000`
- Rootfs target size: `5G`
- MySQL package cache: `target/mysql`
- Success marker: `MYSQL_TEST_PASSED`
