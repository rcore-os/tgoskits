#!/bin/sh
set -e

fail() {
    if [ -n "$current_stage" ]; then
        printf "%sMYSQL_STAGE_FAILED %s/%s %s%s\n" "$red" "$stage_no" "$total_stages" "$current_stage" "$reset"
        stage_output="/tmp/mysql-${current_stage}.out"
        if [ -f "$stage_output" ]; then
            echo "MYSQL_STAGE_OUTPUT $current_stage" >&2
            cat "$stage_output" >&2 || true
        fi
    fi
    echo "MYSQL_TEST_FAILED: $*" >&2
    echo "MYSQL_TEST_FAILED"
    [ -f /tmp/mysql-init.log ] && tail -n 80 /tmp/mysql-init.log >&2 || true
    [ -f /tmp/mysqld.log ] && tail -n 120 /tmp/mysqld.log >&2 || true
    exit 1
}

passed=0
server_pid=""
total_stages=15
stage_no=0
current_stage=""
green="$(printf '\033[32m')"
red="$(printf '\033[31m')"
bold="$(printf '\033[1m')"
reset="$(printf '\033[0m')"

cleanup() {
    if [ "$passed" -ne 1 ] && [ -n "$server_pid" ] && kill -0 "$server_pid" 2>/dev/null; then
        kill -9 "$server_pid" 2>/dev/null || true
    fi
}
trap cleanup EXIT

. /root/mysql-env.sh

mysql_exec() {
    /opt/mysql/bin/mysql --no-defaults -uroot --socket=/tmp/mysql.sock "$@"
}

capture_stream() {
    label="$1"
    shift

    output="/tmp/mysql-${label}.out"
    rm -f "$output"
    "$@" >"$output" 2>&1
}

prep_step() {
    printf "%sMYSQL_PREP %s%s\n" "$bold" "$1" "$reset"
}

wait_mysql_ready() {
    local i min_ready_count

    min_ready_count="$1"
    sleep 30
    i=0
    while [ "$i" -lt 180 ]; do
        if mysql_ready_probe "$min_ready_count"; then
            return
        fi

        if ! kill -0 "$server_pid" 2>/dev/null; then
            fail "mysqld exited before readiness"
        fi

        i=$((i + 1))
        sleep 10
    done

    fail "mysqld did not become ready"
}

ready_log_count() {
    if [ -f /tmp/mysqld.log ]; then
        grep -c "ready for connections" /tmp/mysqld.log 2>/dev/null || true
    else
        echo 0
    fi
}

mysql_ready_probe() {
    local ready_count min_ready_count

    min_ready_count="$1"
    ready_count="$(ready_log_count)"
    [ "$ready_count" -gt "$min_ready_count" ] || return 1

    [ -S /tmp/mysql.sock ] || return 1
    [ -f /opt/mysql/data/mysqld.pid ] || return 1
    capture_stream ready mysql_exec -e "SELECT VERSION() AS mysql_version;" || return 1

    ls -l /tmp/mysql.sock /opt/mysql/data/mysqld.pid 2>/dev/null || return 1
    cat /tmp/mysql-ready.out || true
}

start_mysqld() {
    rm -f /tmp/mysql.sock /opt/mysql/data/mysqld.pid /tmp/mysqld.log

    /opt/mysql/bin/mysqld \
        --no-defaults \
        --user=root \
        --basedir=/opt/mysql \
        --datadir=/opt/mysql/data \
        --socket=/tmp/mysql.sock \
        --pid-file=/opt/mysql/data/mysqld.pid \
        --console \
        --log-error-verbosity=3 \
        > /tmp/mysqld.log 2>&1 &
    server_pid=$!

    wait_mysql_ready 0
}

restart_mysqld() {
    local old_pid
    old_pid="$server_pid"

    prep_step restart

    if [ -n "$old_pid" ] && kill -0 "$old_pid" 2>/dev/null; then
        kill -9 "$old_pid" 2>/dev/null || true
        wait "$old_pid" >/dev/null 2>&1 || true
    fi

    rm -f /tmp/mysql.sock /opt/mysql/data/mysqld.pid
    start_mysqld
}

run_stage() {
    local name="$1"
    shift
    stage_no=$((stage_no + 1))
    current_stage="$name"
    echo "MYSQL_STAGE_NO $stage_no/$total_stages"
    echo "MYSQL_STAGE $current_stage"
    capture_stream "$name" mysql_exec "$@" || fail "stage failed: $name"
    printf "%sMYSQL_STAGE_PASSED %s/%s %s%s\n" "$green" "$stage_no" "$total_stages" "$current_stage" "$reset"
}

[ -x /opt/mysql/bin/mysqld ] || fail "missing /opt/mysql/bin/mysqld"
[ -x /opt/mysql/bin/mysql ] || fail "missing /opt/mysql/bin/mysql"

rm -rf /opt/mysql/data
mkdir -p /opt/mysql/data /tmp /run/mysqld

prep_step initialize
/opt/mysql/bin/mysqld \
    --initialize-insecure \
    --user=root \
    --basedir=/opt/mysql \
    --datadir=/opt/mysql/data \
    --console \
    --log-error-verbosity=3 \
    > /tmp/mysql-init.log 2>&1 &
init_pid=$!

sleep 30
init_ready=0
i=0
while [ "$i" -lt 60 ]; do
    tail -n 120 /tmp/mysql-init.log || true
    if grep -q "Bootstrapping complete" /tmp/mysql-init.log; then
        init_ready=1
        break
    fi
    if ! kill -0 "$init_pid" 2>/dev/null; then
        fail "initialize process exited before bootstrapping completed"
    fi
    i=$((i + 1))
    sleep 10
done

[ "$init_ready" -eq 1 ] || fail "initialize did not reach Bootstrapping complete"

kill "$init_pid" 2>/dev/null || true
sleep 3

prep_step start
start_mysqld

run_stage "01-create-database" <<'SQL'
DROP DATABASE IF EXISTS starry_mysql_test;
CREATE DATABASE IF NOT EXISTS starry_mysql_test
    CHARACTER SET utf8mb4
    COLLATE utf8mb4_0900_ai_ci;
SHOW DATABASES LIKE 'starry_mysql_test';
SELECT SCHEMA_NAME, DEFAULT_CHARACTER_SET_NAME, DEFAULT_COLLATION_NAME
    FROM information_schema.SCHEMATA
    WHERE SCHEMA_NAME = 'starry_mysql_test';
SQL

run_stage "02-create-users-table" <<'SQL'
USE starry_mysql_test;
DROP TABLE IF EXISTS audit_log;
DROP TABLE IF EXISTS payments;
DROP TABLE IF EXISTS orders;
DROP TABLE IF EXISTS users;
CREATE TABLE users (
    id INT PRIMARY KEY,
    name VARCHAR(64) NOT NULL,
    email VARCHAR(128) NOT NULL UNIQUE,
    age INT NOT NULL,
    city VARCHAR(64) NOT NULL,
    status ENUM('active', 'inactive', 'blocked') NOT NULL DEFAULT 'active',
    score DECIMAL(8,2) NOT NULL DEFAULT 0,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NULL DEFAULT NULL,
    CHECK (age >= 0)
) ENGINE=InnoDB;
SHOW CREATE TABLE users;
SELECT COLUMN_NAME, DATA_TYPE, IS_NULLABLE
    FROM information_schema.COLUMNS
    WHERE TABLE_SCHEMA = 'starry_mysql_test' AND TABLE_NAME = 'users'
    ORDER BY ORDINAL_POSITION;
SQL

run_stage "03-insert-users" <<'SQL'
USE starry_mysql_test;
INSERT INTO users(id, name, email, age, city, status, score) VALUES
    (1, 'alice', 'alice@example.test', 20, 'beijing', 'active', 88.50),
    (2, 'bob', 'bob@example.test', 21, 'shanghai', 'active', 91.25),
    (3, 'carol', 'carol@example.test', 22, 'guangzhou', 'inactive', 79.00),
    (4, 'dave', 'dave@example.test', 23, 'shenzhen', 'active', 95.75),
    (5, 'eve', 'eve@example.test', 24, 'hangzhou', 'blocked', 66.00),
    (6, 'frank', 'frank@example.test', 25, 'chengdu', 'active', 83.40),
    (7, 'grace', 'grace@example.test', 26, 'beijing', 'active', 98.10);
SELECT COUNT(*) AS users_count FROM users;
SELECT status, COUNT(*) AS cnt, MIN(age) AS min_age, MAX(score) AS max_score
    FROM users
    GROUP BY status
    ORDER BY status;
SQL

run_stage "04-query-filter-order" <<'SQL'
USE starry_mysql_test;
SELECT id, name, city, age, score
    FROM users
    WHERE age BETWEEN 21 AND 26 AND status IN ('active', 'inactive')
    ORDER BY score DESC, id ASC
    LIMIT 5;
SELECT city, GROUP_CONCAT(name ORDER BY name SEPARATOR ',') AS names
    FROM users
    GROUP BY city
    HAVING COUNT(*) >= 1
    ORDER BY city;
SQL

run_stage "05-update" <<'SQL'
USE starry_mysql_test;
UPDATE users
    SET age = age + 1,
        score = score + 2.50,
        updated_at = CURRENT_TIMESTAMP
    WHERE name = 'alice';
UPDATE users
    SET status = 'inactive'
    WHERE score < 70;
SELECT id, name, age, status, score, updated_at FROM users WHERE name IN ('alice', 'eve');
SELECT COUNT(*) AS inactive_count FROM users WHERE status = 'inactive';
SQL

run_stage "06-index" <<'SQL'
USE starry_mysql_test;
CREATE INDEX idx_users_age_city ON users(age, city);
CREATE INDEX idx_users_status_score ON users(status, score);
SHOW INDEX FROM users;
EXPLAIN SELECT * FROM users WHERE age >= 22 AND city = 'beijing';
EXPLAIN SELECT * FROM users WHERE status = 'active' ORDER BY score DESC;
SQL

run_stage "07-create-orders" <<'SQL'
USE starry_mysql_test;
DROP TABLE IF EXISTS orders;
CREATE TABLE orders (
    id INT PRIMARY KEY,
    user_id INT NOT NULL,
    product VARCHAR(64) NOT NULL,
    amount DECIMAL(10,2) NOT NULL,
    status VARCHAR(16) NOT NULL,
    created_at DATETIME NOT NULL,
    note VARCHAR(128),
    INDEX idx_orders_user_status(user_id, status),
    INDEX idx_orders_created(created_at),
    CONSTRAINT fk_orders_user FOREIGN KEY (user_id) REFERENCES users(id)
) ENGINE=InnoDB;
SHOW CREATE TABLE orders;
SELECT TABLE_NAME, ENGINE, TABLE_ROWS
    FROM information_schema.TABLES
    WHERE TABLE_SCHEMA = 'starry_mysql_test' AND TABLE_NAME = 'orders';
SQL

run_stage "08-insert-orders-join" <<'SQL'
USE starry_mysql_test;
INSERT INTO orders(id, user_id, product, amount, status, created_at, note) VALUES
    (1, 1, 'book', 99.50, 'paid', '2026-06-01 10:00:00', 'first order'),
    (2, 2, 'keyboard', 188.00, 'paid', '2026-06-01 10:05:00', 'mechanical'),
    (3, 3, 'mouse', 42.25, 'pending', '2026-06-01 10:10:00', NULL),
    (4, 4, 'monitor', 899.99, 'paid', '2026-06-02 09:30:00', 'large item'),
    (5, 1, 'usb-c cable', 19.90, 'paid', '2026-06-02 09:45:00', 'accessory'),
    (6, 6, 'ssd', 499.00, 'pending', '2026-06-03 11:20:00', 'storage'),
    (7, 7, 'router', 299.00, 'paid', '2026-06-03 12:00:00', 'network');
SELECT u.name, u.city, o.product, o.amount, o.status
    FROM users u JOIN orders o ON u.id = o.user_id
    WHERE o.amount >= 40
    ORDER BY o.amount DESC, o.id ASC;
SELECT u.name, COALESCE(SUM(o.amount), 0) AS total_amount
    FROM users u LEFT JOIN orders o ON u.id = o.user_id
    GROUP BY u.id, u.name
    ORDER BY total_amount DESC, u.id ASC;
SQL

run_stage "09-aggregation" <<'SQL'
USE starry_mysql_test;
SELECT status, COUNT(*) AS cnt, SUM(amount) AS total_amount, AVG(amount) AS avg_amount
    FROM orders
    GROUP BY status
    ORDER BY total_amount DESC;
SELECT DATE(created_at) AS order_day, COUNT(*) AS cnt, SUM(amount) AS day_total
    FROM orders
    GROUP BY DATE(created_at)
    ORDER BY order_day;
SELECT city, COUNT(*) AS user_count, ROUND(AVG(score), 2) AS avg_score
    FROM users
    GROUP BY city
    ORDER BY avg_score DESC;
SQL

run_stage "10-transaction-commit" <<'SQL'
USE starry_mysql_test;
START TRANSACTION;
INSERT INTO users(id, name, email, age, city, status, score)
    VALUES (8, 'heidi', 'heidi@example.test', 27, 'nanjing', 'active', 87.70);
INSERT INTO orders(id, user_id, product, amount, status, created_at, note)
    VALUES (8, 8, 'dock', 268.80, 'paid', '2026-06-04 08:00:00', 'committed order');
INSERT INTO orders(id, user_id, product, amount, status, created_at, note)
    VALUES (9, 8, 'adapter', 38.60, 'paid', '2026-06-04 08:05:00', 'second committed order');
COMMIT;
SELECT u.id, u.name, COUNT(o.id) AS order_count, SUM(o.amount) AS total_amount
    FROM users u JOIN orders o ON u.id = o.user_id
    WHERE u.id = 8
    GROUP BY u.id, u.name;
SQL

run_stage "11-transaction-rollback-delete" <<'SQL'
USE starry_mysql_test;
START TRANSACTION;
INSERT INTO users(id, name, email, age, city, status, score)
    VALUES (9, 'rollback_user', 'rollback@example.test', 99, 'nowhere', 'active', 1.00);
DELETE FROM orders WHERE status = 'pending';
UPDATE users SET score = 0 WHERE status = 'blocked';
ROLLBACK;
SELECT COUNT(*) AS rollback_user_count FROM users WHERE id = 9;
SELECT COUNT(*) AS pending_orders_after_rollback FROM orders WHERE status = 'pending';
SELECT id, name, score FROM users WHERE status = 'blocked';
SQL

run_stage "12-temporary-table" <<'SQL'
USE starry_mysql_test;
CREATE TEMPORARY TABLE tmp_order_summary AS
    SELECT user_id, COUNT(*) AS order_count, SUM(amount) AS total_amount
    FROM orders
    GROUP BY user_id;
SELECT s.user_id, u.name, s.order_count, s.total_amount
    FROM tmp_order_summary s JOIN users u ON s.user_id = u.id
    ORDER BY s.total_amount DESC;
CREATE TEMPORARY TABLE tmp_numbers (n INT NOT NULL, label VARCHAR(16));
INSERT INTO tmp_numbers VALUES (1, 'one'), (2, 'two'), (3, 'three'), (4, 'four'), (5, 'five');
SELECT SUM(n) AS total, GROUP_CONCAT(label ORDER BY n SEPARATOR '|') AS labels FROM tmp_numbers;
SQL

run_stage "13-view-and-schema" <<'SQL'
USE starry_mysql_test;
CREATE OR REPLACE VIEW paid_orders_view AS
    SELECT u.name, u.city, o.product, o.amount, o.created_at
    FROM users u JOIN orders o ON u.id = o.user_id
    WHERE o.status = 'paid';
SELECT COUNT(*) AS users_count FROM users;
SELECT COUNT(*) AS orders_count FROM orders;
SELECT name, product, amount FROM paid_orders_view ORDER BY amount DESC, name;
SELECT COUNT(*) AS paid_view_count FROM paid_orders_view;
SELECT TABLE_NAME, ENGINE FROM information_schema.TABLES
    WHERE TABLE_SCHEMA = 'starry_mysql_test'
    ORDER BY TABLE_NAME;
SELECT COLUMN_NAME, COLUMN_TYPE
    FROM information_schema.COLUMNS
    WHERE TABLE_SCHEMA = 'starry_mysql_test' AND TABLE_NAME = 'orders'
    ORDER BY ORDINAL_POSITION;
SQL

run_stage "14-consistency-report" <<'SQL'
USE starry_mysql_test;
CREATE TABLE IF NOT EXISTS audit_log (
    id INT PRIMARY KEY AUTO_INCREMENT,
    category VARCHAR(32) NOT NULL,
    detail VARCHAR(128) NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    INDEX idx_audit_category_created(category, created_at)
) ENGINE=InnoDB;
INSERT INTO audit_log(category, detail)
SELECT 'user_status', CONCAT(status, ':', COUNT(*))
    FROM users
    GROUP BY status;
INSERT INTO audit_log(category, detail)
SELECT 'order_status', CONCAT(status, ':', COUNT(*), ':', COALESCE(SUM(amount), 0))
    FROM orders
    GROUP BY status;
SELECT category, COUNT(*) AS entries, GROUP_CONCAT(detail ORDER BY detail SEPARATOR ';') AS details
    FROM audit_log
    GROUP BY category
    ORDER BY category;
SELECT u.city,
       COUNT(DISTINCT u.id) AS user_count,
       COUNT(o.id) AS order_count,
       COALESCE(SUM(CASE WHEN o.status = 'paid' THEN o.amount ELSE 0 END), 0) AS paid_amount
    FROM users u
    LEFT JOIN orders o ON o.user_id = u.id
    GROUP BY u.city
    ORDER BY paid_amount DESC, u.city;
SELECT s.TABLE_NAME, s.INDEX_NAME, COUNT(*) AS column_count
    FROM information_schema.STATISTICS s
    WHERE s.TABLE_SCHEMA = 'starry_mysql_test'
    GROUP BY s.TABLE_NAME, s.INDEX_NAME
    ORDER BY s.TABLE_NAME, s.INDEX_NAME;
SQL

restart_mysqld

run_stage "15-restart-persistence" <<'SQL'
USE starry_mysql_test;
SELECT COUNT(*) AS users_after_restart FROM users;
SELECT COUNT(*) AS orders_after_restart FROM orders;
SELECT SUM(amount) AS paid_total_after_restart FROM orders WHERE status = 'paid';
SELECT name, product, amount FROM paid_orders_view ORDER BY amount DESC, name;
SELECT COUNT(*) AS rollback_user_after_restart FROM users WHERE id = 9;
SELECT COUNT(*) AS pending_orders_after_restart FROM orders WHERE status = 'pending';
SELECT u.name, COUNT(o.id) AS order_count
    FROM users u LEFT JOIN orders o ON u.id = o.user_id
    GROUP BY u.id, u.name
    ORDER BY u.id;
SQL

passed=1
echo "MYSQL_TEST_PASSED"
