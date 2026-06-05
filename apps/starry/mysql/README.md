# Starry MySQL App

This app prepares an x86_64 Debian rootfs with Oracle MySQL 8.4.6 generic glibc binaries, then runs a StarryOS guest-side SQL workload.

```bash
cargo xtask starry app qemu -t mysql --arch x86_64
```

Only x86_64 Debian/glibc rootfs images are supported. The Oracle generic package is not suitable for aarch64 or Alpine/musl rootfs images.

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

## Interactive Mode

To enter the MySQL client and run SQL manually, use the interactive QEMU config:

```bash
cargo xtask starry app qemu -t mysql --arch x86_64 \
  --qemu-config qemu-x86_64-interactive.toml
```

The guest automatically runs:

```sh
/usr/bin/mysql-interactive.sh
```

The script initializes `/opt/mysql/data` if needed, starts `mysqld` in the background, waits until the Unix socket is usable, then enters the MySQL interactive client. Use `exit` to leave the MySQL client and `Ctrl-a x` to exit QEMU.

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
