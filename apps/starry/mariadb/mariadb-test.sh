#!/bin/sh
set -eu

socket=/run/mysqld/mysqld.sock
pid_file=/run/mysqld/mysqld.pid
data_dir=/var/lib/mysql-starry-test-$$
log_file=/tmp/mariadb.log
install_log=/tmp/mariadb-install-db.out
mariadb_pid=""
test_done=0
failed=0
total_stages=16
stage_no=0
current_stage=""
green="$(printf '\033[32m')"
red="$(printf '\033[31m')"
bold="$(printf '\033[1m')"
reset="$(printf '\033[0m')"

fail() {
    if [ -n "$current_stage" ]; then
        printf "%sMARIADB_STAGE_FAILED %s/%s %s%s\n" "$red" "$stage_no" "$total_stages" "$current_stage" "$reset"
    fi
    echo "MARIADB_TEST_FAILED: $*"
    echo "MARIADB_TEST_FAILED"
    failed=1
    exit 1
}

stage_begin() {
    stage_no=$((stage_no + 1))
    current_stage="$1"
    echo "MARIADB_STAGE_NO $stage_no/$total_stages"
    echo "MARIADB_STAGE $current_stage"
}

stage_pass() {
    printf "%sMARIADB_STAGE_PASSED %s/%s %s%s\n" "$green" "$stage_no" "$total_stages" "$current_stage" "$reset"
}

prep_step() {
    printf "%sMARIADB_PREP %s%s\n" "$bold" "$1" "$reset"
}

stop_mariadb() {
    if [ -S "$socket" ]; then
        mariadb-admin --socket="$socket" shutdown >>"$log_file" 2>&1 || true
    fi

    if [ -n "$mariadb_pid" ]; then
        i=0
        while kill -0 "$mariadb_pid" >/dev/null 2>&1 && [ "$i" -lt 20 ]; do
            i=$((i + 1))
            sleep 1
        done
        kill "$mariadb_pid" >/dev/null 2>&1 || true
        wait "$mariadb_pid" >/dev/null 2>&1 || true
        mariadb_pid=""
    fi

    if [ -s "$pid_file" ]; then
        pid="$(cat "$pid_file" 2>/dev/null || true)"
        if [ -n "$pid" ]; then
            kill "$pid" >/dev/null 2>&1 || true
        fi
    fi

    rm -f "$socket" "$pid_file"
}

cleanup() {
    stop_mariadb
    rm -rf "$data_dir" 2>/dev/null || true
}

on_exit() {
    rc=$?
    if [ "$test_done" -ne 1 ] && [ "$failed" -ne 1 ]; then
        printf "%sMARIADB_TEST_RESULT FAILED%s\n" "$red" "$reset"
        echo "MARIADB_TEST_FAILED"
    fi
    cleanup
    exit "$rc"
}
trap on_exit EXIT

run_sql() {
    mariadb --socket="$socket" "$@"
}

capture_stream() {
    label="$1"
    shift

    output="/tmp/mariadb-${label}.out"

    rm -f "$output"
    (
        set +e
        "$@" >"$output" 2>&1
    )
}

start_mariadb() {
    rm -f "$socket" "$pid_file"
    mkdir -p /run/mysqld "$data_dir" /tmp

    mariadbd \
        --user=root \
        --datadir="$data_dir" \
        --socket="$socket" \
        --pid-file="$pid_file" \
        --skip-networking \
        --skip-grant-tables \
        --performance-schema=OFF \
        --innodb-buffer-pool-size=16M \
        --key-buffer-size=8M \
        --tmpdir=/tmp >>"$log_file" 2>&1 &
    mariadb_pid=$!

    i=0
    while [ "$i" -lt 120 ]; do
        if capture_stream ready run_sql -N -B -e "SELECT 1;"; then
            grep -qx "1" /tmp/mariadb-ready.out || return 1
            return 0
        fi
        if ! kill -0 "$mariadb_pid" >/dev/null 2>&1; then
            return 1
        fi
        i=$((i + 1))
        sleep 1
    done

    return 1
}

check_innodb_log() {
    if grep -E "InnoDB: (IO Error|[0-9]+ bytes should have been read)" "$log_file" >/tmp/mariadb-log-errors.out 2>&1; then
        return 1
    fi
}

expect_sql() {
    label="$1"
    expected="$2"
    query="$3"
    output="/tmp/mariadb-${label}.out"

    capture_stream "$label" run_sql -N -B -e "$query" || return 1
    grep -Fqx "$expected" "$output" || return 1
}

prep_step install
capture_stream apk apk add mariadb mariadb-client || fail "apk add mariadb failed"

prep_step initdb
stop_mariadb
rm -f "$log_file" "$install_log"
mkdir -p /run/mysqld "$data_dir"

if command -v mariadb-install-db >/dev/null 2>&1; then
    capture_stream install-db mariadb-install-db --user=root --datadir="$data_dir" || fail "mariadb-install-db failed"
elif command -v mysql_install_db >/dev/null 2>&1; then
    capture_stream install-db mysql_install_db --user=root --datadir="$data_dir" || fail "mysql_install_db failed"
else
    fail "missing mariadb-install-db/mysql_install_db"
fi

: >"$log_file"
prep_step start
start_mariadb || fail "mariadbd did not become ready"

capture_stream select1 run_sql -N -B -e "SELECT 1;" || fail "SELECT 1 failed"
grep -qx "1" /tmp/mariadb-select1.out || fail "SELECT 1 returned unexpected output"

stage_begin schema
capture_stream schema run_sql <<'SQL' || fail "schema SQL failed"
DROP DATABASE IF EXISTS starry_test;
CREATE DATABASE starry_test;
USE starry_test;

CREATE TABLE users (
    id INT PRIMARY KEY AUTO_INCREMENT,
    name VARCHAR(64) NOT NULL,
    age INT NOT NULL,
    city VARCHAR(64),
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
) ENGINE=InnoDB;

CREATE TABLE orders (
    id INT PRIMARY KEY AUTO_INCREMENT,
    user_id INT NOT NULL,
    product VARCHAR(64) NOT NULL,
    amount DECIMAL(10,2) NOT NULL,
    status VARCHAR(16) NOT NULL,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    INDEX idx_user_id(user_id),
    INDEX idx_status(status)
) ENGINE=InnoDB;
SQL
stage_pass

stage_begin insert
capture_stream insert run_sql <<'SQL' || fail "insert SQL failed"
USE starry_test;
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
(3, 'usb', 19.90, 'paid'),
(4, 'disk', 299.00, 'cancel'),
(5, 'phone', 3999.00, 'paid');
SQL
stage_pass

stage_begin select-all
capture_stream select-all run_sql <<'SQL' || fail "select-all SQL failed"
USE starry_test;
SELECT * FROM users;
SELECT * FROM orders;
SQL
stage_pass

stage_begin filter-order-limit
capture_stream filter-order-limit run_sql <<'SQL' || fail "filter-order-limit SQL failed"
USE starry_test;
SELECT id, name, age FROM users WHERE age >= 23 ORDER BY age DESC LIMIT 3;
SQL
stage_pass

stage_begin aggregate
capture_stream aggregate run_sql <<'SQL' || fail "aggregate SQL failed"
USE starry_test;
SELECT status, COUNT(*) AS cnt, SUM(amount) AS total_amount
FROM orders
GROUP BY status
ORDER BY total_amount DESC;
SQL
stage_pass

stage_begin join-query
capture_stream join-query run_sql <<'SQL' || fail "join SQL failed"
USE starry_test;
SELECT u.name, u.city, o.product, o.amount, o.status
FROM users u
JOIN orders o ON u.id = o.user_id
WHERE o.status = 'paid'
ORDER BY o.amount DESC;
SQL
stage_pass

stage_begin update
capture_stream update run_sql <<'SQL' || fail "update SQL failed"
USE starry_test;
UPDATE users SET city = 'Hangzhou' WHERE name = 'Alice';
SELECT * FROM users WHERE name = 'Alice';
SQL
expect_sql alice-city "Alice:Hangzhou" "SELECT CONCAT(name, ':', city) FROM starry_test.users WHERE name = 'Alice';" || fail "alice-city query failed"
stage_pass

stage_begin delete
capture_stream delete run_sql <<'SQL' || fail "delete SQL failed"
USE starry_test;
DELETE FROM orders WHERE status = 'cancel';
SELECT * FROM orders;
SQL
expect_sql order-count-after-delete 6 "SELECT COUNT(*) FROM starry_test.orders;" || fail "order-count-after-delete query failed"
stage_pass

stage_begin commit
capture_stream commit run_sql <<'SQL' || fail "commit SQL failed"
USE starry_test;
START TRANSACTION;
INSERT INTO users(name, age, city) VALUES ('Frank', 26, 'Nanjing');
INSERT INTO orders(user_id, product, amount, status) VALUES (6, 'router', 188.00, 'paid');
COMMIT;

SELECT * FROM users WHERE name = 'Frank';
SELECT * FROM orders WHERE user_id = 6;
SQL
expect_sql frank-count 1 "SELECT COUNT(*) FROM starry_test.users WHERE name = 'Frank';" || fail "frank-count query failed"
stage_pass

stage_begin rollback
capture_stream rollback run_sql <<'SQL' || fail "rollback SQL failed"
USE starry_test;
START TRANSACTION;
INSERT INTO users(name, age, city) VALUES ('RollbackUser', 99, 'Nowhere');
ROLLBACK;

SELECT * FROM users WHERE name = 'RollbackUser';
SQL
expect_sql rollback-count 0 "SELECT COUNT(*) FROM starry_test.users WHERE name = 'RollbackUser';" || fail "rollback-count query failed"
stage_pass

stage_begin index
capture_stream index run_sql <<'SQL' || fail "index SQL failed"
USE starry_test;
CREATE INDEX idx_users_city ON users(city);
SHOW INDEX FROM users;
SQL
stage_pass

stage_begin temporary-table
capture_stream temporary-table run_sql <<'SQL' || fail "temporary-table SQL failed"
USE starry_test;
CREATE TEMPORARY TABLE temp_summary AS
SELECT user_id, COUNT(*) AS order_count, SUM(amount) AS total_amount
FROM orders
GROUP BY user_id;

SELECT * FROM temp_summary ORDER BY total_amount DESC;
SQL
stage_pass

stage_begin view
capture_stream view run_sql <<'SQL' || fail "view SQL failed"
USE starry_test;
CREATE VIEW paid_orders_view AS
SELECT u.name, o.product, o.amount
FROM users u
JOIN orders o ON u.id = o.user_id
WHERE o.status = 'paid';

SELECT * FROM paid_orders_view ORDER BY amount DESC;
SQL
stage_pass

stage_begin show-schema
capture_stream show-schema run_sql <<'SQL' || fail "show-schema SQL failed"
USE starry_test;
SHOW TABLES;
DESCRIBE users;
DESCRIBE orders;
SQL
stage_pass

stage_begin final-statistics
capture_stream final-statistics run_sql <<'SQL' || fail "final-statistics SQL failed"
USE starry_test;
SELECT
    (SELECT COUNT(*) FROM users) AS user_count,
    (SELECT COUNT(*) FROM orders) AS order_count,
    (SELECT COUNT(*) FROM paid_orders_view) AS paid_order_count;
SQL
expect_sql user-count 6 "SELECT COUNT(*) FROM starry_test.users;" || fail "user-count query failed"
expect_sql order-count 7 "SELECT COUNT(*) FROM starry_test.orders;" || fail "order-count query failed"
expect_sql paid-order-count 6 "SELECT COUNT(*) FROM starry_test.paid_orders_view;" || fail "paid-order-count query failed"
expect_sql paid-total 4504.80 "SELECT COALESCE(SUM(amount), 0) FROM starry_test.orders WHERE status = 'paid';" || fail "paid-total query failed"
check_innodb_log || fail "InnoDB reported I/O errors"
stage_pass
stage_begin restart-persistence
stop_mariadb
start_mariadb || fail "mariadbd did not become ready after restart"

expect_sql restart-user-count 6 "SELECT COUNT(*) FROM starry_test.users;" || fail "restart-user-count query failed"
expect_sql restart-order-count 7 "SELECT COUNT(*) FROM starry_test.orders;" || fail "restart-order-count query failed"
expect_sql restart-paid-order-count 6 "SELECT COUNT(*) FROM starry_test.paid_orders_view;" || fail "restart-paid-order-count query failed"
expect_sql restart-alice-city "Alice:Hangzhou" "SELECT CONCAT(name, ':', city) FROM starry_test.users WHERE name = 'Alice';" || fail "restart-alice-city query failed"
expect_sql restart-rollback-count 0 "SELECT COUNT(*) FROM starry_test.users WHERE name = 'RollbackUser';" || fail "restart-rollback-count query failed"
expect_sql restart-paid-total 4504.80 "SELECT COALESCE(SUM(amount), 0) FROM starry_test.orders WHERE status = 'paid';" || fail "restart-paid-total query failed"
check_innodb_log || fail "InnoDB reported I/O errors after restart"
stage_pass
stop_mariadb

test_done=1
trap - EXIT
cleanup

printf "%sMARIADB_TEST_RESULT PASSED%s\n" "$green" "$reset"
echo "MARIADB_TEST_PASSED"
