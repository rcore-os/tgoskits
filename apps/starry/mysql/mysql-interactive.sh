#!/bin/sh
set -e

. /root/mysql-env.sh

mysql_exec() {
    /opt/mysql/bin/mysql --no-defaults -uroot --socket=/tmp/mysql.sock "$@"
}

wait_for_init() {
    i=0
    sleep 30
    while [ "$i" -lt 60 ]; do
        tail -n 80 /tmp/mysql-init.log || true
        if grep -q "Bootstrapping complete" /tmp/mysql-init.log; then
            return
        fi
        if ! kill -0 "$init_pid" 2>/dev/null; then
            echo "mysql initialize exited before Bootstrapping complete" >&2
            exit 1
        fi
        i=$((i + 1))
        sleep 10
    done
    echo "mysql initialize timed out" >&2
    exit 1
}

initialize_if_needed() {
    if [ -d /opt/mysql/data/mysql ]; then
        return
    fi

    echo "MYSQL_INTERACTIVE_PREP initialize"
    rm -rf /opt/mysql/data
    mkdir -p /opt/mysql/data /tmp /run/mysqld
    /opt/mysql/bin/mysqld \
        --initialize-insecure \
        --user=root \
        --basedir=/opt/mysql \
        --datadir=/opt/mysql/data \
        --console \
        --log-error-verbosity=3 \
        > /tmp/mysql-init.log 2>&1 &
    init_pid=$!

    wait_for_init
    kill "$init_pid" 2>/dev/null || true
    sleep 3
}

wait_for_server() {
    i=0
    sleep 30
    while [ "$i" -lt 180 ]; do
        if [ -S /tmp/mysql.sock ] && [ -f /opt/mysql/data/mysqld.pid ] \
            && mysql_exec -e "SELECT VERSION() AS mysql_version;" >/tmp/mysql-interactive-ready.out 2>&1; then
            ls -l /tmp/mysql.sock /opt/mysql/data/mysqld.pid 2>/dev/null || true
            cat /tmp/mysql-interactive-ready.out || true
            return
        fi
        if ! kill -0 "$server_pid" 2>/dev/null; then
            echo "mysqld exited before readiness" >&2
            tail -n 120 /tmp/mysqld.log >&2 || true
            exit 1
        fi
        i=$((i + 1))
        sleep 10
    done
    echo "mysqld readiness timed out" >&2
    tail -n 120 /tmp/mysqld.log >&2 || true
    exit 1
}

start_server_if_needed() {
    if mysql_exec -e "SELECT VERSION() AS mysql_version;" >/tmp/mysql-interactive-ready.out 2>&1; then
        cat /tmp/mysql-interactive-ready.out || true
        return
    fi

    echo "MYSQL_INTERACTIVE_PREP start"
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

    wait_for_server
}

initialize_if_needed
start_server_if_needed

echo "MYSQL_INTERACTIVE_READY"
exec /opt/mysql/bin/mysql --no-defaults -uroot --socket=/tmp/mysql.sock
