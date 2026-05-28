#!/bin/sh
set -eu

log=/tmp/gdbserver-smoke.log
rm -f "$log"

/usr/bin/gdbserver 0.0.0.0:1234 /usr/bin/gdbserver-smoke-target >"$log" 2>&1 &
server_pid=$!
trap 'kill "$server_pid" 2>/dev/null || true' EXIT

listening=0
for _ in 1 2 3 4 5 6 7 8 9 10; do
    if grep -q "Listening on port 1234" "$log"; then
        listening=1
        break
    fi
    sleep 1
done

cat "$log"
if [ "$listening" -ne 1 ]; then
    echo "FAIL: gdbserver did not start listening"
    exit 1
fi

/usr/bin/gdb -q -batch -x /usr/bin/gdbserver-smoke.gdb /usr/bin/gdbserver-smoke-target

if ! wait "$server_pid"; then
    cat "$log"
    exit 1
fi
trap - EXIT

cat "$log"
echo GDBSERVER_SMOKE_DONE
