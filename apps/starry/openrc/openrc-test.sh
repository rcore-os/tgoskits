#!/bin/sh
echo "=== install openrc ==="
apk update
apk add openrc
if ! command -v rc-service >/dev/null 2>&1; then
    echo "OPENRC_TEST_FAILED"
    echo "Debug: apk add openrc failed, rc-service not found"
    exit 1
fi

echo "=== configure openrc for non-PID1 operation ==="
mkdir -p /run/openrc /etc/init.d /etc/conf.d
touch /run/openrc/softlevel
echo "default" > /run/openrc/softlevel

# Disable cgroups (not supported yet)
sed -i 's/^rc_cgroup_/#rc_cgroup_/' /etc/rc.conf 2>/dev/null || true

echo "=== create test service ==="
cat > /etc/init.d/testservice <<'SVCEOF'
#!/sbin/openrc-run

name="testservice"
description="Simple test service for StarryOS"
pidfile="/run/testservice.pid"
command="/bin/sh"
command_args="-c 'echo TESTSERVICE_RUNNING > /tmp/testservice.out; sleep 3600'"
command_background=true
SVCEOF
chmod +x /etc/init.d/testservice

echo "=== start test service ==="
rc-service testservice start
START_RET=$?
echo "start returned: $START_RET"
sleep 2

echo "=== check service status ==="
STATUS=$(rc-service testservice status 2>&1)
echo "Status: $STATUS"

echo "=== verify service ran ==="
if [ -f /tmp/testservice.out ]; then
    CONTENT=$(cat /tmp/testservice.out)
    echo "Service output: $CONTENT"
else
    echo "Service output file not found"
fi

echo "=== stop test service ==="
rc-service testservice stop 2>&1
sleep 1

echo "=== verify service stopped ==="
STOP_STATUS=$(rc-service testservice status 2>&1)
echo "After stop: $STOP_STATUS"

echo "=== final verdict ==="
if echo "$STATUS" | grep -qi "started" && [ -f /tmp/testservice.out ] && grep -q "TESTSERVICE_RUNNING" /tmp/testservice.out; then
    echo "OPENRC_TEST_PASSED"
else
    echo "OPENRC_TEST_FAILED"
    echo "Debug: STATUS=$STATUS"
    echo "Debug: START_RET=$START_RET"
    [ -f /tmp/testservice.out ] && echo "Debug: OUT=$(cat /tmp/testservice.out)"
fi
