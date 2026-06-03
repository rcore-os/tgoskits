# Starry PostgreSQL App

This case runs a PostgreSQL smoke test in StarryOS through the app runner.

## Automated Test

```bash
cargo xtask starry app qemu -t postgresql --arch riscv64
cargo xtask starry app qemu -t postgresql --arch aarch64
cargo xtask starry app qemu -t postgresql --arch x86_64
cargo xtask starry app qemu -t postgresql --arch loongarch64
```

The guest test installs `postgresql17`, initializes a fresh database cluster via
`initdb`, starts `postgres` over a Unix socket, verifies `SELECT 1`, then runs a
structured SQL workload over the `starry_test` database. The workload covers:

- DDL: table creation with foreign keys and indexes
- DML: multi-row inserts, updates, deletes
- Queries: filtering, ordering, aggregation, joins
- Transactions: commit and rollback
- Bulk insert via `generate_series`
- Data integrity verification (row counts, update persistence, rollback correctness)

## Manual Reproduction (without test script)

These steps let you interactively run PostgreSQL on StarryOS for demonstrations
or debugging. All commands are typed at the StarryOS shell prompt (`root@starry:`).

### Prerequisites

Build and boot StarryOS with networking enabled:

```bash
# Prepare rootfs (once)
cargo xtask starry rootfs --arch riscv64

# Build and run
cargo xtask starry app qemu -t postgresql --arch riscv64
```

Wait for the `root@starry:` prompt to appear.

### Step 1: Install PostgreSQL

```sh
apk update
apk add postgresql17
```

This installs PostgreSQL 17 from the Alpine package repository. The server
binaries are installed to `/usr/libexec/postgresql17/` and the client tools
(`psql`, `pg_ctl`, `initdb`) are available on `$PATH`.

Set a convenience variable for the binary path:

```sh
PG=/usr/libexec/postgresql17
```

### Step 2: Create the postgres user

PostgreSQL refuses to run as root. Create a dedicated system user:

```sh
echo 'postgres:x:70:70:PostgreSQL:/var/lib/postgresql:/bin/sh' >> /etc/passwd
echo 'postgres:x:70:' >> /etc/group
echo 'postgres::20000:0:99999:7:::' >> /etc/shadow
```

### Step 3: Initialize the database cluster

```sh
mkdir /tmp/pgdata
chmod 0700 /tmp/pgdata
chown postgres:postgres /tmp/pgdata
su -s /bin/sh postgres -c "$PG/initdb -D /tmp/pgdata --no-locale --username=postgres"
```

Expected output ends with:
```
Success. You can now start the database server using:
    pg_ctl -D /tmp/pgdata -l logfile start
```

### Step 4: Start the PostgreSQL server

Create a Unix socket directory and start the server:

```sh
mkdir /tmp/pgrun
chmod 0700 /tmp/pgrun
chown postgres:postgres /tmp/pgrun
su -s /bin/sh postgres -c "$PG/pg_ctl -D /tmp/pgdata -l /tmp/pg.log start -w -t 120 \
  -o '-c max_connections=10 -c shared_buffers=16MB -c unix_socket_directories=/tmp/pgrun -c fsync=off'"
```

Expected output:
```
waiting for server to start.... done
server started
```

### Step 5: Connect and run queries

Connect via Unix socket and try basic operations:

```sh
# Simple query
su -s /bin/sh postgres -c "$PG/psql -h /tmp/pgrun -d postgres -c 'SELECT 1'"

# Create a test database
su -s /bin/sh postgres -c "$PG/psql -h /tmp/pgrun -d postgres -c 'CREATE DATABASE mydb'"

# Create a table
su -s /bin/sh postgres -c "$PG/psql -h /tmp/pgrun -d mydb -c '
  CREATE TABLE users (id SERIAL PRIMARY KEY, name VARCHAR(64), age INT)'"

# Insert data
su -s /bin/sh postgres -c "$PG/psql -h /tmp/pgrun -d mydb -c \"
  INSERT INTO users(name, age) VALUES ('Alice', 21), ('Bob', 22), ('Carol', 23)\""

# Query data
su -s /bin/sh postgres -c "$PG/psql -h /tmp/pgrun -d mydb -c 'SELECT * FROM users'"
```

Expected output for the SELECT:
```
 id | name  | age
----+-------+-----
  1 | Alice |  21
  2 | Bob   |  22
  3 | Carol |  23
(3 rows)
```

### Step 6: Try more advanced SQL

```sh
# Join with aggregation
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

Expected output:
```
 name  | orders | total
-------+--------+--------
 Alice |      2 | 238.90
 Bob   |      1 |  59.00
(2 rows)
```

### Step 7: Stop the server

```sh
su -s /bin/sh postgres -c "$PG/pg_ctl -D /tmp/pgdata stop"
```

Expected output:
```
waiting for server to shut down.... done
server stopped
```

### Step 8: Verify persistence

Restart and verify data survives:

```sh
su -s /bin/sh postgres -c "$PG/pg_ctl -D /tmp/pgdata -l /tmp/pg.log start -w -t 120 \
  -o '-c max_connections=10 -c shared_buffers=16MB -c unix_socket_directories=/tmp/pgrun -c fsync=off'"
su -s /bin/sh postgres -c "$PG/psql -h /tmp/pgrun -d mydb -c 'SELECT * FROM users'"
su -s /bin/sh postgres -c "$PG/pg_ctl -D /tmp/pgdata stop"
```

The users table should still contain all three rows.

### Cleanup

```sh
rm -rf /tmp/pgdata /tmp/pg.log
```

## Interactive psql Session

For an interactive SQL session, use:

```sh
su -s /bin/sh postgres -c "$PG/psql -h /tmp/pgrun -d postgres"
```

This drops you into the psql REPL where you can type SQL directly:

```
postgres=# SELECT version();
postgres=# CREATE TABLE t(id int);
postgres=# INSERT INTO t VALUES (1),(2),(3);
postgres=# SELECT * FROM t;
postgres=# \q
```

## Architecture Notes

| Architecture | Status | Notes |
|-------------|--------|-------|
| riscv64 | Working | Tested with 1G RAM, 30 min timeout |
| aarch64 | Working | Uses cortex-a53, GICv2 |
| x86_64 | Working | Tested with qemu64 CPU |
| loongarch64 | Working | Uses la464 CPU, 30 min timeout |

## Required Kernel Support

PostgreSQL requires these kernel features (all present in tgoskits `dev`):

- Process credentials (setuid, setresuid, getuid, etc.)
- `SA_RESTART` syscall restart for signal-interrupted syscalls
- DTB-based physical memory discovery (~300MB+ required)
- `RLIMIT_STACK` default set to 8MB (Linux default)
- `fsync`/`fdatasync` directory support
- `sync_file_range` stub
- `prctl` `PR_SET_PDEATHSIG` support
- `epoll_pwait` sigsetsize compatibility (musl's 16-byte `sigset_t`)
- Unix domain sockets for initial connection handshake

## Known Limitations

- `initdb` is slow on QEMU (~40s on riscv64 TCG) due to emulation overhead
- No SSL/TLS support (compiled `--without-openssl`)
- `pgbench` stress testing is available as a separate test case
- File ownership (uid/gid) is stored correctly as of PR #1097
