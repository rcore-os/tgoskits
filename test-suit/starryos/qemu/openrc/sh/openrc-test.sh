#!/bin/sh
set -eu
stage=identity
trap 'echo "STARRY_OPENRC_FAILED: stage=$stage"' EXIT
cmdline=$(tr '\000' ' ' < /proc/1/cmdline)
runlevel=$(rc-status --runlevel)
printf 'OpenRC init: cmdline=%s exe=%s runlevel=%s\n' "$cmdline" "$(readlink /proc/1/exe)" "$runlevel"
case "$cmdline" in
    '/sbin/init '*|'init '*) ;;
    *) exit 1 ;;
esac
[ /proc/1/exe -ef /bin/busybox ]
[ "$runlevel" = default ]
stage=service-setup
cat > /etc/init.d/openrc-test-dependency <<'SERVICE'
#!/sbin/openrc-run
start() { echo dependency >> /run/openrc-test-order; }
SERVICE
cat > /etc/init.d/openrc-test-daemon <<'SERVICE'
#!/sbin/openrc-run
command=/bin/sleep
command_args=86400
command_background=yes
pidfile=/run/openrc-test-daemon.pid
depend() { need openrc-test-dependency; }
start_pre() { echo daemon >> /run/openrc-test-order; }
stop_post() { echo stopped >> /root/openrc-stops; echo STARRY_OPENRC_SERVICE_STOPPED; }
SERVICE
cat > /etc/init.d/openrc-test-failure <<'SERVICE'
#!/sbin/openrc-run
start() { return 1; }
SERVICE
cat > /etc/init.d/openrc-test-dependent <<'SERVICE'
#!/sbin/openrc-run
depend() { need openrc-test-failure; }
start() { touch /run/openrc-test-unexpected; }
SERVICE
chmod +x /etc/init.d/openrc-test-*
stage=service-start
rc-service openrc-test-daemon start
[ "$(cat /run/openrc-test-order)" = "$(printf 'dependency\ndaemon')" ]
rc-service openrc-test-daemon status
old_pid=$(cat /run/openrc-test-daemon.pid)
kill -0 "$old_pid"
stage=service-restart
rc-service openrc-test-daemon restart
new_pid=$(cat /run/openrc-test-daemon.pid)
[ "$old_pid" != "$new_pid" ]
kill -0 "$new_pid"
stage=service-stop
rc-service openrc-test-daemon stop
if rc-service openrc-test-daemon status; then exit 1; fi
if kill -0 "$new_pid" 2>/dev/null; then exit 1; fi
stage=dependency-failure
if rc-service openrc-test-dependent start; then exit 1; fi
[ ! -e /run/openrc-test-unexpected ]
stage=registration
rc-update add openrc-test-daemon default
[ -L /etc/runlevels/default/openrc-test-daemon ]
rc-service openrc-test-daemon start
# Leave the registered daemon running to verify the shutdown runlevel stops it.
trap - EXIT
echo STARRY_OPENRC_PASSED
