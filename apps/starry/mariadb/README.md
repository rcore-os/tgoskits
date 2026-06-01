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

Before injecting the test script, `prebuild.sh` refreshes the app-specific
rootfs from the cached clean Alpine archive. This keeps each MariaDB app run
independent from a previously used or partially filled rootfs image.
