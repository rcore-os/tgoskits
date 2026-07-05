#!/bin/sh
set -eu

PG=/usr/libexec/postgresql17
PGRUN=/tmp/pgrun
PGDATA=/tmp/pgdata
LOG=/tmp/postgresql.log
postgres_pid=""
test_done=0
failed=0
total_stages=14
stage_no=0
current_stage=""
green="$(printf '\033[32m')"
red="$(printf '\033[31m')"
bold="$(printf '\033[1m')"
reset="$(printf '\033[0m')"

fail() {
    if [ -n "$current_stage" ]; then
        printf "%sPOSTGRESQL_STAGE_FAILED %s/%s %s%s\n" "$red" "$stage_no" "$total_stages" "$current_stage" "$reset"
    fi
    echo "POSTGRESQL_TEST_FAILED: $*"
    echo "POSTGRESQL_TEST_FAILED"
    failed=1
    exit 1
}

stage_begin() {
    stage_no=$((stage_no + 1))
    current_stage="$1"
    echo "POSTGRESQL_STAGE_NO $stage_no/$total_stages"
    echo "POSTGRESQL_STAGE $current_stage"
}

stage_pass() {
    printf "%sPOSTGRESQL_STAGE_PASSED %s/%s %s%s\n" "$green" "$stage_no" "$total_stages" "$current_stage" "$reset"
}

prep_step() {
    printf "%sPOSTGRESQL_PREP %s%s\n" "$bold" "$1" "$reset"
}

stop_postgres() {
    # Try clean shutdown first
    if [ -n "$postgres_pid" ] && kill -0 "$postgres_pid" >/dev/null 2>&1; then
        su -s /bin/sh postgres -c "$PG/pg_ctl -D $PGDATA stop -w -t 30" >>"$LOG" 2>&1 || true
    fi
    # Force kill if still running
    if [ -n "$postgres_pid" ] && kill -0 "$postgres_pid" >/dev/null 2>&1; then
        kill "$postgres_pid" >/dev/null 2>&1 || true
        i=0
        while kill -0 "$postgres_pid" >/dev/null 2>&1 && [ "$i" -lt 30 ]; do
            i=$((i + 1))
            sleep 1
        done
        kill -9 "$postgres_pid" >/dev/null 2>&1 || true
        wait "$postgres_pid" >/dev/null 2>&1 || true
    fi
    postgres_pid=""
}

cleanup() {
    stop_postgres
    rm -rf "$PGDATA" 2>/dev/null || true
}

on_exit() {
    rc=$?
    if [ "$test_done" -ne 1 ] && [ "$failed" -ne 1 ]; then
        printf "%sPOSTGRESQL_TEST_RESULT FAILED%s\n" "$red" "$reset"
        echo "POSTGRESQL_TEST_FAILED"
    fi
    cleanup
    exit "$rc"
}
trap on_exit EXIT

ensure_postgres_user() {
    grep -q '^postgres:' /etc/passwd 2>/dev/null || echo 'postgres:x:70:70:PostgreSQL:/var/lib/postgresql:/bin/sh' >> /etc/passwd
    grep -q '^postgres:' /etc/group 2>/dev/null || echo 'postgres:x:70:' >> /etc/group
    grep -q '^postgres:' /etc/shadow 2>/dev/null || echo 'postgres::20000:0:99999:7:::' >> /etc/shadow
}

pg_sql() {
    printf '%s\n' "$2" > /tmp/_pg_sql.sql
    su -s /bin/sh postgres -c "$PG/psql -h $PGRUN -d $1 -U postgres -f /tmp/_pg_sql.sql"
}

pg_sql_quiet() {
    printf '%s\n' "$2" > /tmp/_pg_sql.sql
    su -s /bin/sh postgres -c "$PG/psql -h $PGRUN -d $1 -U postgres -t -A -f /tmp/_pg_sql.sql"
}

start_postgres() {
    su -s /bin/sh postgres -c "$PG/postgres -D $PGDATA -c max_connections=10 -c shared_buffers=16MB -c unix_socket_directories=$PGRUN -c fsync=off >>$LOG 2>&1 & echo \$!" >/tmp/postgres.pid
    postgres_pid="$(cat /tmp/postgres.pid 2>/dev/null)" || true
    if [ -z "$postgres_pid" ] || ! kill -0 "$postgres_pid" >/dev/null 2>&1; then
        echo "postgres failed to start" >&2
        return 1
    fi
    # Wait until server actually accepts connections
    i=0
    while [ "$i" -lt 30 ]; do
        if pg_sql_quiet postgres "SELECT 1;" >/dev/null 2>&1; then
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done
    echo "postgres started but did not accept connections within 30s" >&2
    return 1
}

# ─── Test starts here ─────────────────────────────────────────

prep_step "installing PostgreSQL"
apk add postgresql17 >/dev/null 2>&1 || fail "apk add postgresql17 failed"

prep_step "setting up postgres user"
ensure_postgres_user

prep_step "preparing data directory"
rm -rf "$PGDATA" "$PGRUN"
mkdir -p "$PGRUN"
chmod 0700 "$PGDATA" "$PGRUN" 2>/dev/null || true
chown postgres:postgres "$PGDATA" "$PGRUN" 2>/dev/null || true

# Stage 1 — initdb
stage_begin "initdb"
out="$(su -s /bin/sh postgres -c "$PG/initdb -D $PGDATA --no-locale --username=postgres" 2>&1)" || {
    echo "$out" >>"$LOG"
    fail "initdb: $out"
}
echo "$out" >>"$LOG"
stage_pass

# Stage 2 — server start
stage_begin "server-start"
stop_postgres
start_postgres || fail "server did not become ready — see $LOG"
stage_pass

# Stage 3 — basic connectivity
stage_begin "select-1"
r="$(pg_sql_quiet postgres "SELECT 1;" 2>/dev/null)" || true
[ "$r" = "1" ] || fail "SELECT 1: got '$r'"
stage_pass

# Stage 4 — create database
stage_begin "create-database"
pg_sql postgres "CREATE DATABASE starry_test;" >>"$LOG" 2>&1 || fail "CREATE DATABASE failed"
stage_pass

# Stage 5 — create tables
stage_begin "create-tables"
pg_sql starry_test "
CREATE TABLE users (
    id SERIAL PRIMARY KEY,
    name VARCHAR(64) NOT NULL,
    age INT NOT NULL,
    city VARCHAR(64),
    created_at TIMESTAMP DEFAULT NOW()
);
CREATE TABLE orders (
    id SERIAL PRIMARY KEY,
    user_id INT NOT NULL REFERENCES users(id),
    product VARCHAR(64) NOT NULL,
    amount DECIMAL(10,2) NOT NULL,
    status VARCHAR(16) NOT NULL,
    created_at TIMESTAMP DEFAULT NOW()
);
CREATE INDEX idx_orders_user_id ON orders(user_id);
CREATE INDEX idx_orders_status ON orders(status);
" >>"$LOG" 2>&1 || fail "CREATE TABLE failed"
stage_pass

# Stage 6 — insert data
stage_begin "insert-data"
pg_sql starry_test "
INSERT INTO users(name, age, city) VALUES
    ('Alice', 21, 'Chongqing'),
    ('Bob', 22, 'Chengdu'),
    ('Carol', 23, 'Beijing'),
    ('David', 24, 'Shanghai'),
    ('Eve', 25, 'Shenzhen');
INSERT INTO orders(user_id, product, amount, status) VALUES
    (1, 'book', 39.90, 'paid'),
    (1, 'keyboard', 199.00, 'paid'),
    (2, 'mouse', 59.00, 'paid'),
    (2, 'monitor', 899.00, 'pending'),
    (3, 'usb-cable', 19.90, 'paid'),
    (4, 'ssd-disk', 299.00, 'cancel'),
    (5, 'phone', 3999.00, 'paid');
" >>"$LOG" 2>&1 || fail "INSERT failed"
stage_pass

# Stage 7 — select with filter and ordering
stage_begin "select-filter-order"
r="$(pg_sql_quiet starry_test "SELECT CONCAT(name,':',city) FROM users WHERE age >= 23 ORDER BY age DESC LIMIT 3;" 2>/dev/null)" || true
echo "$r" | grep -q "Carol:Beijing" || fail "filter/order: expected Carol:Beijing, got '$r'"
stage_pass

# Stage 8 — aggregate query
stage_begin "aggregate"
set +e
r="$(pg_sql_quiet starry_test "SELECT COUNT(*) FROM orders WHERE status = 'paid';" 2>&1)"
agg_rc=$?
set -e
echo "aggregate rc=$agg_rc result='$r'" >>"$LOG"
[ "$agg_rc" -eq 0 ] || fail "aggregate query failed: $r"
[ "$r" = "5" ] || fail "aggregate: expected 5 paid orders, got '$r'"
stage_pass

# Stage 9 — join query
stage_begin "join-query"
r="$(pg_sql_quiet starry_test "SELECT u.name FROM users u JOIN orders o ON u.id = o.user_id WHERE o.amount > 100 ORDER BY o.amount DESC;" 2>/dev/null)" || true
echo "$r" | grep -q "Alice" || fail "JOIN: expected Alice in results, got '$r'"
stage_pass

# Stage 10 — update
stage_begin "update"
pg_sql starry_test "UPDATE users SET city = 'Hangzhou' WHERE name = 'Alice';" >>"$LOG" 2>&1 || fail "UPDATE failed"
r="$(pg_sql_quiet starry_test "SELECT city FROM users WHERE name = 'Alice';" 2>/dev/null)" || true
[ "$r" = "Hangzhou" ] || fail "UPDATE: expected Hangzhou, got '$r'"
stage_pass

# Stage 11 — delete and rollback
stage_begin "delete-rollback"
pg_sql starry_test "
DELETE FROM orders WHERE status = 'cancel';
BEGIN;
INSERT INTO users(name, age, city) VALUES ('RollbackUser', 99, 'Nowhere');
ROLLBACK;
" >>"$LOG" 2>&1 || fail "DELETE/ROLLBACK failed"
r="$(pg_sql_quiet starry_test "SELECT COUNT(*) FROM users WHERE name = 'RollbackUser';" 2>/dev/null)" || true
[ "$r" = "0" ] || fail "ROLLBACK: expected 0, got '$r'"
stage_pass

# Stage 12 — bulk insert via generate_series
stage_begin "bulk-insert"
pg_sql starry_test "
CREATE TABLE metrics(seq INT, val INT, data TEXT);
INSERT INTO metrics(seq, val, data) SELECT g, g*2, 'row_'||g FROM generate_series(1,200) g;
" >>"$LOG" 2>&1 || fail "bulk insert failed"
r="$(pg_sql_quiet starry_test "SELECT COUNT(*) FROM metrics;" 2>/dev/null)" || true
[ "$r" = "200" ] || fail "bulk: expected 200, got '$r'"
stage_pass

# Stage 13 — verify data integrity (all data present before shutdown)
stage_begin "data-integrity"
r="$(pg_sql_quiet starry_test "SELECT COUNT(*) FROM users;" 2>/dev/null)" || true
[ "$r" = "5" ] || fail "users: expected 5, got '$r'"
r="$(pg_sql_quiet starry_test "SELECT city FROM users WHERE name = 'Alice';" 2>/dev/null)" || true
[ "$r" = "Hangzhou" ] || fail "update: expected Hangzhou, got '$r'"
r="$(pg_sql_quiet starry_test "SELECT COUNT(*) FROM metrics;" 2>/dev/null)" || true
[ "$r" = "200" ] || fail "metrics: expected 200, got '$r'"
# Verify rollback didn't persist
r="$(pg_sql_quiet starry_test "SELECT COUNT(*) FROM users WHERE name = 'RollbackUser';" 2>/dev/null)" || true
[ "$r" = "0" ] || fail "rollback: expected 0, got '$r'"
stage_pass

# Stage 14 — clean shutdown
stage_begin "clean-shutdown"
stop_postgres
stage_pass

test_done=1
trap - EXIT
cleanup

printf "%sPOSTGRESQL_TEST_RESULT PASSED%s\n" "$green" "$reset"
echo "POSTGRESQL_TEST_PASSED"
