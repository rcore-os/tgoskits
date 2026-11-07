# Starry MariaDB App

This case runs a MariaDB smoke test in StarryOS through the app runner.

```bash
cargo xtask starry app run -t mariadb --arch x86_64
cargo xtask starry app run -t mariadb --arch aarch64
cargo xtask starry app run -t mariadb --arch riscv64
cargo xtask starry app run -t mariadb --arch loongarch64
```

The guest test installs `mariadb` and `mariadb-client`, initializes a fresh data
directory, starts `mariadbd` over a Unix socket, verifies `SELECT 1`, then runs a
larger SQL workload over `starry_test`. The workload covers InnoDB table
creation, multi-row inserts, filtering, ordering, aggregation, joins, update,
delete, commit, rollback, secondary indexes, temporary tables, views, schema
inspection, and final statistics. The test then restarts the server and checks
that the persistent rows and view results are still present.

The script also fails the case if the MariaDB log contains the InnoDB I/O
patterns covered by the Starry direct I/O fixes:

- short reads such as `bytes should have been read`
- write failures such as `InnoDB: IO Error`

项目 app runner 负责从基础镜像复制本次运行独立的 per-app rootfs，并在运行后清理；
`prebuild.sh` 只扩容 runner 提供的镜像，并注入测试脚本、`/etc/apk/repositories`
和 `/etc/resolv.conf`，不直接解压 registry archive。

## Rootfs 容量

`prebuild.sh` 在注入 overlay 前对 per-app rootfs 执行只增不减的扩容，默认
`MARIADB_ROOTFS_SIZE=4G`；已有更大镜像保持原大小，不会被缩小。扩容使用
`truncate`、`e2fsck` 和 `resize2fs`，需要主机提供 `e2fsprogs` 与 `coreutils`
中的 `debugfs`、`e2fsck`、`resize2fs`、`numfmt`、`stat` 和 `truncate`。
`e2fsck` 返回真实错误或 `resize2fs` 失败都会终止 prebuild。
